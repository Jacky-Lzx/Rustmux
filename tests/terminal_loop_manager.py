"""Live manager keymaps, acknowledged saves and asynchronous list refreshes."""
import fcntl
import os
from pathlib import Path
import signal
import struct
import subprocess
import tempfile
import termios
import time
import tomllib
from terminal_loop_support import BINARY, Session

with tempfile.TemporaryDirectory(prefix="rustmux-manager-") as temporary:
    root=Path(temporary)
    selected=root / "manager.toml"
    env=dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root/"state"),
             RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name=f"manager-{os.getpid()}"
    helper=f"manager-helper-{os.getpid()}"
    session=None
    stopped=None

    def write(save="Ctrl s", create="a", down="n"):
        selected.write_text(f'''save_scrollback=true
[session_manager]
save=["{save}"]
create=["{create}"]
down=["{down}"]
''')

    def run(*arguments, success=True):
        result=subprocess.run([BINARY,*map(str,arguments)],env=env,capture_output=True,text=True,timeout=10)
        assert (result.returncode==0)==success,(arguments,result)
        return result

    def expect(*texts):
        deadline=time.monotonic()+5
        while not all(t in session.output for t in texts):
            session.read()
            assert time.monotonic()<deadline,(texts,bytes(session.output[-2000:]))
        result=bytes(session.output)
        session.output.clear()
        session.frames.clear()
        return result

    def open_manager():
        global session
        session=Session(extra_env=env, arguments=("--config",str(selected),"attach",name))
        fcntl.ioctl(session.slave,termios.TIOCSWINSZ,struct.pack("HHHH",24,160,0,0))
        session.expect(b"RUSTMUX_READY>")
        session.send(b"\x02\x17")
        transition=expect(b"Session Manager",b"<Ctrl-S> Save")
        for destructive in (b"\x1b[?1049l",b"\x1b[?1049h",b"\x1b[2J"):
            assert destructive not in transition, (destructive,transition)
        assert b"\x1b[=0u" in transition  # Manager keys use baseline terminal encoding.

    def leave():
        global session
        for attempt in range(2):
            session.send(b"\x1b")
            transition=session.expect(b"RUSTMUX_READY>")
            # Returning to the current session must not expose the outer shell
            # or clear the alternate screen before the session redraws it.
            for destructive in (b"\x1b[?1049l",b"\x1b[?1049h",b"\x1b[2J"):
                assert destructive not in transition, (destructive,transition)
            if attempt == 0:
                session.send(b"\x02\x17")
                expect(b"Session Manager")
        session.send(b"\x02d")
        session.finish(0)
        assert b"\x1b[?1049l" in session.output
        session.close()
        session=None

    try:
        write()
        checked=tomllib.loads(run("config", "check","--config",selected,"--strict","--toml").stdout)
        assert checked["session_manager"]["save"]==["Ctrl s"]
        run("new",name,"--detached","--config",selected)
        panes=tomllib.loads(run("pane", "list","-s",name,"--toml").stdout)["panes"]
        run("pane", "send-keys","-s",name,"-p",panes[0]["id"],"--literal","--enter","stty -echo; VALUE=survives; printf 'MANAGER_TEXT\\n'")
        # While the server is paused, the picker remains usable and cannot
        # report success before the save endpoint acknowledges the write.
        open_manager()
        listed=run("list","--long").stdout
        pid=int(next(line.split()[2] for line in listed.splitlines() if line.startswith(name+" ")))
        os.kill(pid,signal.SIGSTOP)
        stopped=pid
        session.send(b"\x13")
        expect(b"Saving "+name.encode())
        session.send(b"/manager")
        captured=expect(b"Search: manager_")
        assert b"Saved "+name.encode() not in captured
        assert b"\x1b[2J" not in captured
        os.kill(pid,signal.SIGCONT)
        stopped=None
        expect(b"Saved "+name.encode())
        snapshot=root / "state/rustmux/main-human/sessions" / f"{name}.toml"
        saved=tomllib.loads(snapshot.read_text())
        assert any(p.get("history") for w in saved["windows"] for p in w["panes"])
        session.send(b"\x1b")  # clear search
        expect(b"<Ctrl-S> Save")
        # Config reload replaces handling and hints without closing the view.
        write(save="Ctrl g", create="b", down="Ctrl n")
        expect(b"<Ctrl-G> Save",b"<b> New")
        selected.write_text('[session_manager]\nsave=["k"]')
        expect(b"Config reload failed")
        before=snapshot.stat().st_mtime_ns
        session.send(b"\x07")  # prior good binding still works
        deadline=time.monotonic()+5
        while snapshot.stat().st_mtime_ns==before:
            session.read()
            assert time.monotonic()<deadline
        write(save="Ctrl g", create="b", down="Ctrl n")
        expect(b"<Ctrl-G> Save")
        # Ordinary action characters stay text while editing a search/name;
        # reload during editing keeps the typed query intact.
        session.send(b"/bknqajd")
        expect(b"Search: bknqajd_")
        write(save="Ctrl g",create="v",down="Ctrl n")
        deadline=time.monotonic()+1.1
        while time.monotonic()<deadline: session.read(0.02)
        session.send(b"x")
        expect(b"Search: bknqajdx_")
        session.send(b"\x1b")
        expect(b"<v> New")
        session.send(b"v")
        expect(b"New session: _")
        session.send(b"ajdkq")
        expect(b"New session: ajdkq_")
        session.send(b"\x1b")
        expect(b"<v> New")
        # Listings refresh in the open view and retain the search editor.
        run("new",helper,"--detached","--config",selected)
        expect(helper.encode())
        run("kill",helper)
        # A no-op key paints after the worker has refreshed, then search proves
        # the stopped session is gone rather than matching an older frame.
        deadline=time.monotonic()+1.1
        while time.monotonic()<deadline: session.read(0.02)
        session.output.clear()
        session.send(("/"+helper).encode())
        frame=expect(("Search: "+helper+"_").encode(),b"0 SESSIONS")
        session.send(b"\x1b")
        expect(b"<v> New")
        leave()
        # Failure is reported after the write fails, without losing the client
        # or replacing the prior snapshot. A new picker gets a fresh keymap.
        write()
        original=snapshot.read_bytes()
        backup=snapshot.with_suffix(".backup")
        snapshot.rename(backup)
        snapshot.mkdir()
        try:
            open_manager()
            session.send(b"\x13")
            expect(b"Save failed")
            assert backup.read_bytes()==original
            leave()
        finally:
            snapshot.rmdir()
            backup.rename(snapshot)
        # Reconnection still has the same shell environment.
        run("pane", "send-keys","-s",name,"-p",panes[0]["id"],"--literal","--enter", "printf 'VALUE_%s\\n' \"$VALUE\"")
        deadline=time.monotonic()+5
        while "VALUE_survives" not in run("pane", "capture","-s",name,"-p",panes[0]["id"]).stdout:
            assert time.monotonic()<deadline
            time.sleep(0.02)
    finally:
        if stopped: os.kill(stopped,signal.SIGCONT)
        if session: session.close()
        for target in (name,helper): subprocess.run([BINARY,"kill",target],env=env,capture_output=True,timeout=8)

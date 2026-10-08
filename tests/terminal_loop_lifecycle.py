"""Exited panes stay usable; respawn replaces only the chosen process in place."""
import fcntl
import signal
import termios
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import time
import tomllib
from terminal_loop_support import BINARY, Session, expect_footer

def expect_title(session, text, absent=None):
    # The content helper intentionally strips pane borders; titles belong to
    # the raw completed frame, so verify them there.
    deadline = time.monotonic() + 5
    while text not in session.last_frame or (absent and absent in session.last_frame):
        session.read()
        assert time.monotonic() < deadline, session.last_frame
    session.output.clear()
    session.frames.clear()


with tempfile.TemporaryDirectory(prefix="rustmux-lifecycle-") as temporary:
    root = Path(temporary)
    config = root / "rustmux" / "config.toml"
    config.parent.mkdir()
    config.write_text('tab_name="title"\n' + "remain_on_exit = true\nsave_scrollback = true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="",
               EDITOR="/usr/bin/true", VISUAL="/usr/bin/true")
    name = f"lifecycle-{os.getpid()}"
    layout = root / "layout.toml"
    layout.write_text("""[[windows]]
name='jobs'
[[windows.panes]]
remain_on_exit=true
command="printf 'run\\n' >> runs; i=0; while [ $i -lt 100 ]; do printf 'LINE_%s\\n' $i; i=$((i+1)); done; printf 'FINAL_OUTPUT\\n'; exit 7"
[[windows.panes]]
remain_on_exit=false
command='exit 9'
[[windows.panes]]
""")
    session = None

    def run(*args, success=True):
        result = subprocess.run([BINARY, *map(str, args)], env=env, capture_output=True,
                                text=True, timeout=8)
        assert (result.returncode == 0) == success, (args, result)
        return result

    def panes():
        return tomllib.loads(run("list-panes", "-s", name, "--toml").stdout)["panes"]

    def wait(predicate):
        deadline = time.monotonic() + 5
        while True:
            if session:
                session.read(seconds=0.01)
            entries = panes()
            if predicate(entries):
                return entries
            assert time.monotonic() < deadline, entries
            time.sleep(0.01)

    def control(action, pane, *args, success=True):
        return run(action, "-s", name, "-p", pane, *args, success=success)

    try:
        run("new", name, "--detached", "--layout", layout)
        entries = wait(lambda ps: len(ps) == 2 and ps[0]["exited"] and ps[0]["output_complete"])
        retained, sibling = entries
        pane = retained["id"]
        assert retained["exit_code"] == 7 and "exit_signal" not in retained
        assert retained["directory"] == str(root.resolve())
        captured = control("capture-pane", pane, "--history").stdout
        assert "LINE_0" in captured and "FINAL_OUTPUT" in captured, captured
        before = panes()
        control("respawn-pane", sibling["id"], success=False)
        control("respawn-pane", pane, "--cwd", root / "missing", success=False)
        control("respawn-pane", pane, "--command", "", success=False)
        control("send-keys", pane, "--literal", "ignored", success=False)
        assert panes() == before
        assert control("capture-pane", pane, "--history").stdout == captured
        # Original project command replays, with the same pane ID and new process.
        assert int(control("respawn-pane", pane).stdout) == pane
        entries = wait(lambda ps: ps[0]["exited"] and ps[0]["output_complete"])
        assert len((root / "runs").read_text().splitlines()) == 2
        assert entries[0]["pid"] != retained["pid"]
        assert entries[1] == sibling
        assert entries[0]["active"] == retained["active"]
        # An explicit command becomes the next startup command. Signal exits are distinct.
        control("respawn-pane", pane, "--command", "printf SIGNAL_OUTPUT; kill -KILL $$")
        entries = wait(lambda ps: ps[0]["exited"] and ps[0]["output_complete"])
        assert entries[0]["exit_signal"] == 9 and "exit_code" not in entries[0]
        assert "SIGNAL_OUTPUT" in control("capture-pane", pane).stdout
        assert "FINAL_OUTPUT" not in control("capture-pane", pane, "--history").stdout
        # Moving an exited pane retains its identity and status.
        assert int(control("break-pane", pane, "--name", "retained").stdout) == pane
        moved = next(p for p in panes() if p["id"] == pane)
        assert moved["exit_signal"] == 9 and moved["active"]
        # All processes may exit without destroying a server containing retained panes.
        control("send-keys", sibling["id"], "--literal", "--enter", "exit 0")
        wait(lambda ps: all(p["exited"] and p["output_complete"] for p in ps))
        assert name in run("ls").stdout
        run("save-session", name)
        snapshot = root / "state" / "rustmux" / "main-human" / "sessions" / f"{name}.toml"
        saved = tomllib.loads(snapshot.read_text())
        explicit = saved["windows"][1]["panes"][0]
        assert explicit["remain_on_exit"] is True
        assert explicit["command"] == "printf SIGNAL_OUTPUT; kill -KILL $$"
        # Exit status is runtime-only; snapshot restore starts the recorded commands.
        run("kill", name)
        config.write_text('tab_name="title"\n' + "remain_on_exit = false\nsave_scrollback = true\n")
        run("new", name, "--detached")
        restored = wait(lambda ps: len(ps) == 2 and any(p.get("exit_signal") == 9 for p in ps))
        ordinary = next(p for p in restored if not p["exited"])
        control("send-keys", ordinary["id"], "--literal", "--enter", "exit 0")
        restored = wait(lambda ps: len(ps) == 1 and ps[0]["output_complete"])
        # An attached client can browse history and close the final retained pane.
        session = Session(extra_env=env, arguments=("attach", name))
        expect_title(session, b"signal 9")
        fcntl.ioctl(session.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 100, 0, 0))
        os.kill(session.app_pid, signal.SIGWINCH)
        session.send(b"discarded\x02[")
        expect_footer(session, b"HISTORY")
        session.send(b"q\x02xyes\r")
        session.finish(0)
        session.close()
        session = None
        assert name in run("ls").stdout  # The saved workspace remains discoverable.
        assert not Path(f"/tmp/rustmux-{os.geteuid()}/{name}.sock").exists()
        run("list-panes", "-s", name, success=False)
    finally:
        if session:
            session.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

    # Foreground mode uses the same config and can respawn without a named socket.
    config.write_text('tab_name="title"\n' + "remain_on_exit = true\n")
    editor = root / "editor.sh"
    editor.write_text("sleep 0.3\n")
    foreground_env = dict(env, EDITOR=f"/bin/sh {editor}", VISUAL=f"/bin/sh {editor}")
    session = Session(extra_env=foreground_env)
    try:
        session.expect(b"RUSTMUX_READY>")
        # Temporary editors still close automatically despite global retention.
        session.send(b"\x02E")
        expect_title(session, b"2 history")
        expect_title(session, b"1 shell", absent=b"2 history")
        session.send(b"stty -echo; printf 'FOREGROUND_FINAL\\n'; sleep 0.2; exit 7\n")
        session.send(b"\x1b[200~")  # A stopped process cannot finish this paste.
        expect_title(session, b"exited 7")
        session.send(b"discarded\x02R")
        # The retained screen still contains the old prompt. Wait for the live
        # title before accepting a prompt from the replacement process.
        expect_title(session, b"1 shell", absent=b"exited 7")
        session.expect(b"RUSTMUX_READY> ")
        assert not any(b"FOREGROUND_FINAL" in row for row in session.last_rows)
        session.send(b"printf 'RESPAWN_ALIVE\\n'\n")
        session.expect(b"RESPAWN_ALIVE")
        session.send(b"\x02xyes\r")
        session.finish(0)
    finally:
        session.close()

print("retention policy, final output, stable respawn, signal status, saved overrides and dead-pane UI passed")

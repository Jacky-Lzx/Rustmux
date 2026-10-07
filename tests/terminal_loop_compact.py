"""Compact chrome preserves child geometry, modal editors, reload and snapshots."""
import fcntl
import os
import shlex
from pathlib import Path
import struct
import subprocess
import tempfile
import termios
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_bar, expect_footer

with tempfile.TemporaryDirectory(prefix="rustmux-compact-") as temporary:
    root = Path(temporary)
    config = root / "server.toml"
    other = root / "client.toml"
    other.write_text("compact=false\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"compact-{os.getpid()}"
    session = None

    def write(compact, extra=""):
        config.write_text(f"compact={str(compact).lower()}\nmouse_hover_cursor=true\n"
                          "save_scrollback=true\nscrollback_lines=100\n" + extra)

    def run(action, *args, success=True):
        target = [name] if action in ("new", "kill", "save") else ["-s", name]
        result = subprocess.run([BINARY, action, *target, *map(str, args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert (result.returncode == 0) == success, (action, args, result)
        return result.stdout

    def panes():
        return tomllib.loads(run("list-panes", "--toml"))["panes"]

    def status():
        return tomllib.loads(run("show-config"))

    def wait(predicate):
        deadline = time.monotonic() + 6
        while not predicate():
            if session: session.read(0.01)
            assert time.monotonic() < deadline, (status(), session.physical_rows if session else None)
            time.sleep(0.01)

    def attach():
        global session
        session = Session(extra_env=env, arguments=("--config", str(other), "attach", name), lifetime=100)
        session.expect(b"RUSTMUX_READY>")

    def detach():
        global session
        session.send(b"\x02d")
        session.finish(0)
        session.close()
        session = None

    def resize(rows, columns=80):
        fcntl.ioctl(session.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        wait(lambda: len(session.physical_rows) == rows)

    def size(marker, expected):
        session.send(f"stty -echo; printf '{marker}:%s\\n' \"$(stty size)\"\n".encode())
        session.expect(f"{marker}:{expected}".encode())

    try:
        write(True)
        run("new", "--detached", "--config", config)
        attach()
        expect_bar(session, b"LOCKED")
        assert b"LOCKED" not in session.physical_rows[-1]
        original = panes()[0]
        session.send(b"stty -echo; KEEP=preserved; printf 'BOOT:%s\\n' \"$(stty size)\"\n")
        session.expect(b"BOOT:21 78")  # Compact gains one real PTY row.
        # The gained last content row forwards mouse coordinates to the child.
        spy = root / "mouse.py"
        recorded = root / "mouse.bin"
        spy.write_text("""import os,sys,termios,tty
from pathlib import Path
original=termios.tcgetattr(0)
try:
    tty.setraw(0)
    os.write(1,b"\\x1b[?1000h\\x1b[?1006hMOUSE_READY")
    data=bytearray()
    while not data.endswith(b'm'):
        data.extend(os.read(0,1))
    Path(sys.argv[1]).write_bytes(data)
finally:
    os.write(1,b"\\x1b[?1000l\\x1b[?1006l")
    termios.tcsetattr(0,termios.TCSANOW,original)
os.write(1,b"\\r\\nMOUSE_DONE\\r\\n")
""")
        session.send(f"python3 {shlex.quote(str(spy))} {shlex.quote(str(recorded))}\n".encode())
        session.expect(b"MOUSE_READY")
        session.send(b"\x1b[<0;2;23M\x1b[<0;2;23m")
        session.expect(b"MOUSE_DONE")
        assert recorded.read_bytes() == b"\x1b[<0;1;21M\x1b[<0;1;21m"
        session.send(b"printf '\\033[21;1HBOTTOM_CONTENT'\n")
        session.expect(b"BOTTOM_CONTENT")
        assert b"BOTTOM_CONTENT" in session.physical_rows[-2]
        session.send(b"\x02,")
        expect_bar(session, b"Rename:")
        assert b"BOTTOM_CONTENT" in session.physical_rows[-2]
        session.send(b"renamed\r")
        expect_bar(session, b"1 renamed")
        expect_bar(session, b"LOCKED")
        before = run("capture-pane", "--history")
        session.send(b"\x02[")
        expect_bar(session, b"History")
        session.send(b"/BOTTOM")
        expect_bar(session, b"/BOTTOM")
        assert session.last_frame.endswith(b"\x1b[?25h")
        session.send(b"\x03q")
        expect_bar(session, b"LOCKED")
        assert run("capture-pane", "--history") == before
        session.send(b"\x02?")
        session.expect(b"Shortcut Help")
        session.send(b"q")
        expect_bar(session, b"LOCKED")
        # Mode badge cells have no tab hitbox or child action.
        run("new-window", "--name", "other")
        expect_bar(session, b"2 other")
        session.expect(b"RUSTMUX_READY>")
        size("NEW_WINDOW", "21 78")
        session.send(b"\x1b[<0;79;1M\x1b[<0;79;1m")
        size("BADGE", "21 78")
        assert next(p for p in panes() if p["active"])["window"] == 2
        # Actual top-bar window click still works with space reserved for the badge.
        row = session.physical_rows[0]
        column = len(row[:row.index(b"1 renamed")].decode()) + 2
        session.send(f"\x1b[<0;{column};1M\x1b[<0;{column};1m".encode())
        wait(lambda: next(p for p in panes() if p["active"])["window"] == 1)
        session.send(b"printf 'KEEP_%s\\n' \"$KEEP\"\n")
        session.expect(b"KEEP_preserved")
        # Reload is deferred during a modal snapshot and applies on exit.
        session.send(b"\x02[")
        expect_bar(session, b"History")
        write(False)
        wait(lambda: status()["pending"])
        assert status()["settings"]["compact"]
        session.send(b"q")
        wait(lambda: not status()["settings"]["compact"])
        expect_footer(session, b"LOCKED")
        size("STANDARD", "20 78")
        assert panes()[0]["pid"] == original["pid"]
        detach()
        write(True)
        wait(lambda: status()["settings"]["compact"])
        # Control creation while detached also uses compact dimensions.
        run("new-window", "--name", "detached")
        attach()
        expect_bar(session, b"3 detached")
        size("DETACHED", "21 78")
        run("select-window", "-w", 1)
        expect_bar(session, b"1 renamed")
        # Earlier simple windows must stay unchanged if a later inactive tree
        # cannot fit: preflight all windows before committing any geometry.
        resize(7)
        run("select-window", "-w", 3)
        expect_bar(session, b"3 detached")
        run("split-pane", "--down")
        wait(lambda: len([p for p in panes() if p["window"] == 3]) == 2)
        run("select-window", "-w", 1)
        expect_bar(session, b"1 renamed")
        accepted = status()
        write(False, "remain_on_exit=true\n")
        wait(lambda: "error" in status())
        failed = status()
        assert failed["settings"] == accepted["settings"] and failed["generation"] == accepted["generation"]
        expect_bar(session, b"Config reload failed")
        size("ATOMIC", "4 78")
        assert {p["pid"] for p in panes()} >= {original["pid"]}
        write(True)
        wait(lambda: "error" not in status())
        expect_bar(session, b"LOCKED")
        # A compact-only saved tree validates, reloads and restores before any shell starts.
        run("save")
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        saved = snapshot.read_bytes()
        assert b"KEEP_preserved" in saved
        detach()
        run("kill")
        session = Session(extra_env=env, arguments=("--config", str(config), "attach", name, "--create"))
        session.expect(b"RUSTMUX_READY>")
        expect_bar(session, b"LOCKED")
        assert len(panes()) == 4
        assert panes()[0]["pid"] != original["pid"]
        assert snapshot.read_bytes() == saved
        resize(28, 100)
        run("select-window", "-w", 2)
        expect_bar(session, b"2 other")
        size("RESIZED", "25 98")
        detach()
    finally:
        if session: session.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

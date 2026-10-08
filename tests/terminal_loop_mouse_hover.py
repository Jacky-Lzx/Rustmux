"""Hover feedback never changes child pointer stacks, input modes or processes."""
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import time
import tomllib
from terminal_loop_support import BINARY, Session, expect_footer

with tempfile.TemporaryDirectory(prefix="rustmux-hover-") as temporary:
    root = Path(temporary)
    # Keep the label stable while probing pointer behavior with Python children.
    selected = root / "server.toml"
    client = root / "client.toml"
    selected.write_text('tab_name="title"\n' + "mouse_hover_cursor=false\n")
    client.write_text('tab_name="title"\n' + "mouse_hover_cursor=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"hover-{os.getpid()}"
    session = None
    probe = root / "probe.py"
    probe.write_text(r'''
import json, os, select, sys, tty
from pathlib import Path
tty.setraw(0)
path = Path(sys.argv[1])
def emit(value): os.write(1, b"\x1b]22;" + value.encode() + b"\x1b\\")
emit(sys.argv[2])
os.write(1, b"HOVER_READY\r\n")
events = bytearray()
query = None
mode = 0
previous = None
while True:
    if select.select([0], [], [], 0.01)[0]:
        data = os.read(0, 4096)
        if not data: break
        events.extend(data)
        for byte in data:
            if byte in (1,2,3,4):
                mode = {1:1000, 2:1002, 3:1003, 4:0}[byte]
                os.write(1, b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006h")
                if mode: os.write(1, f"\x1b[?{mode}h".encode())
                # Writing the mode is asynchronous: publishing JSON immediately
                # lets the parent send motion before Rustmux parses the change.
                # Its ordered DECRQM reply acknowledges that the modes above
                # have been applied before we publish the new input/mode state.
                os.write(1, b"\x1b[?1003$p")
                reply = bytearray()
                while not reply.endswith(b"$y"): reply.extend(os.read(0,1))
                expected = b"\x1b[?1003;1$y" if mode == 1003 else b"\x1b[?1003;2$y"
                assert reply == expected, (mode, reply)
            elif byte == 5: emit("help")
            elif byte == 6:
                emit("?__current__")
                reply = bytearray()
                while not reply.endswith(b"\x1b\\"): reply.extend(os.read(0,1))
                query = reply.decode()
            elif byte == 7: os.write(1, b"\x1b[?2026hSYNC_HIDDEN")
            elif byte == 8: os.write(1, b"\x1b[?2026l")
    size = os.get_terminal_size(0)
    state = {"pid":os.getpid(), "input":events.hex(), "query":query, "mode":mode,
             "rows":size.lines, "columns":size.columns}
    if state != previous:
        path.with_suffix(".tmp").write_text(json.dumps(state))
        os.replace(path.with_suffix(".tmp"), path)
        previous = state
''')

    def run(action, *args):
        target = [name] if action in ("new", "kill") else ["-s", name]
        result = subprocess.run([BINARY, action, *target, *map(str, args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, (action, args, result)
        return result.stdout

    def record(label="b"):
        path = root / f"{label}.json"
        return json.loads(path.read_text()) if path.exists() else None

    def wait(predicate):
        deadline = time.monotonic() + 5
        while not predicate():
            if session: session.read(0.01)
            assert time.monotonic() < deadline, bytes(session.output[-2500:]) if session else "detached"
            time.sleep(0.01)

    def setting(enabled):
        wait(lambda: tomllib.loads(run("show-config"))["settings"]["mouse_hover_cursor"] == enabled)

    def shape(expected):
        def matches():
            found = re.findall(rb"\x1b\]22;([^\x1b]*)\x1b\\", session.output)
            return bool(found) and found[-1] == expected.encode()
        wait(matches)
        session.output.clear()

    def motion(column, row, button=35, terminator="M"):
        session.send(f"\x1b[<{button};{column};{row}{terminator}".encode())

    def command(pane, byte, label="b"):
        old = record(label)["input"]
        run("send-keys", "-p", pane, "--literal", chr(byte))
        wait(lambda: record(label)["input"] == old + bytes([byte]).hex())

    def no_input(before, label="b"):
        deadline = time.monotonic() + 0.1
        while time.monotonic() < deadline: session.read(0.01)
        assert record(label)["input"] == before

    def detach():
        global session
        session.send(b"\x02d")
        session.finish(0)
        assert b"\x1b]22;\x1b\\\x1b[0m\x1b[?25h\x1b[?1049l" in session.output
        session.close()
        session = None

    try:
        run("new", "--detached", "--config", selected)
        left = tomllib.loads(run("list-panes", "--toml"))["panes"][0]["id"]
        launch = lambda label, shape: f"exec python3 -u {shlex.quote(str(probe))} {shlex.quote(str(root / (label+'.json')))} {shape}"
        run("send-keys", "-p", left, "--literal", "--enter", launch("a", "crosshair"))
        wait(lambda: record("a") is not None)
        right = int(run("split-pane", "-p", left, "--command", launch("b", "wait")))
        wait(lambda: record() is not None)
        identities = {p["id"]:p["pid"] for p in tomllib.loads(run("list-panes", "--toml"))["panes"]}
        session = Session(extra_env=env, arguments=("--config", str(client), "attach", name))
        shape("wait")
        wait(lambda: bool(session.physical_rows) and b"shell" in session.physical_rows[0])
        tab_column = session.physical_rows[0].decode().index("shell") + 1
        assert session.private_modes.get(1002) and not session.private_modes.get(1003)
        motion(tab_column, 1)
        before = record()["input"]
        no_input(before)
        assert b"\x1b]22;pointer\x1b\\" not in session.output  # Server setting wins over client.
        selected.write_text('tab_name="title"\n' + "mouse_hover_cursor=true\n")
        setting(True)
        shape("pointer")  # Reload reevaluates the last known position.
        wait(lambda: session.private_modes.get(1003))
        assert not session.private_modes.get(1006)  # Keep existing legacy encoding.
        command(right, 6)
        wait(lambda: record()["query"] == "\x1b]22;wait\x1b\\")
        # Hovering never overwrites the application-owned state queried above.
        command(right, 7)
        wait(lambda: "SYNC_HIDDEN" in run("capture-pane", "-p", right))
        motion(50, 5)
        no_input(record()["input"])
        assert b"SYNC_HIDDEN" not in session.output, "hover exposed an unfinished synchronized frame"
        command(right, 8)
        shape("wait")
        session.expect(b"SYNC_HIDDEN")
        no_input(record()["input"])
        for byte in (1, 2):  # Button and Drag children do not receive unpressed motion.
            command(right, byte)
            wait(lambda: record()["mode"] == {1:1000, 2:1002}[byte])
            before = record()["input"]
            motion(51+byte, 5)
            no_input(before)
        command(right, 3)
        wait(lambda: record()["mode"] == 1003)
        before = record()["input"]
        motion(55, 5)
        wait(lambda: record()["input"] == before + b"\x1b[<35;14;3M".hex())
        command(right, 4)
        motion(40, 5)
        shape("ew-resize")
        motion(40, 5, 0)
        shape("grabbing")
        motion(43, 5, 32)
        wait(lambda: record()["columns"] == 35)
        motion(43, 5, 0, "m")
        shape("ew-resize")
        motion(50, 5)
        shape("wait")
        # Bottom action hover preserves NORMAL; its click still executes the old action.
        session.send(b"\x02")
        expect_footer(session, b"NORMAL")
        motion(tab_column, 1)
        shape("pointer")
        expect_footer(session, b"NORMAL")
        session.send(b"\x1b")
        expect_footer(session, b"LOCKED")
        wait(lambda: len(session.physical_rows) == 24 and b"Ctrl-B" in session.physical_rows[-1])
        footer_column = session.physical_rows[-1].decode().index("Ctrl-B") + 1
        motion(50, 5)
        shape("wait")
        motion(footer_column, 24)
        shape("pointer")
        motion(footer_column, 24, 0)
        motion(footer_column, 24, 0, "m")
        expect_footer(session, b"NORMAL")
        session.send(b"\x1b")
        expect_footer(session, b"LOCKED")
        motion(50, 5)
        shape("wait")
        session.send(b"\x02Z")  # Zoom removes separator hover hitboxes.
        wait(lambda: record()["columns"] == 78)
        motion(43, 5)
        no_input(record()["input"])
        assert b"\x1b]22;ew-resize\x1b\\" not in session.output
        session.send(b"\x02Z")
        wait(lambda: record()["columns"] == 35)
        motion(tab_column, 1)
        shape("pointer")
        command(right, 5)  # Application changes its pointer underneath a hover override.
        selected.write_text('tab_name="title"\n' + "mouse_hover_cursor=false\n")
        setting(False)
        shape("help")
        wait(lambda: session.private_modes.get(1002) and not session.private_modes.get(1003))
        selected.write_text('tab_name="title"\n' + "mouse_hover_cursor='invalid'\n")
        wait(lambda: "error" in tomllib.loads(run("show-config")))
        setting(False)
        selected.write_text('tab_name="title"\n' + "mouse_hover_cursor=true\n")
        setting(True)
        shape("pointer")
        # A reload error replaces the footer actions; it is not a hover target.
        selected.write_text('tab_name="title"\n' + "mouse_hover_cursor='invalid'\n")
        wait(lambda: "error" in tomllib.loads(run("show-config")))
        motion(footer_column, 24)
        shape("help")
        motion(tab_column, 1)
        shape("pointer")
        selected.write_text('tab_name="title"\n' + "mouse_hover_cursor=true\n")
        wait(lambda: "error" not in tomllib.loads(run("show-config")))
        detach()
        selected.write_text('tab_name="title"\n' + "mouse_hover_cursor=false\n")
        setting(False)
        session = Session(extra_env=env, arguments=("attach", name))
        shape("help")
        run("select-pane", "-p", left)
        shape("crosshair")
        assert {p["id"]:p["pid"] for p in tomllib.loads(run("list-panes", "--toml"))["panes"]} == identities
        detach()
    finally:
        if session: session.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

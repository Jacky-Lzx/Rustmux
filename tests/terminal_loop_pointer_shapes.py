"""OSC 22 queries and pointer state stay with their pane across real PTY lifecycle."""
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

with tempfile.TemporaryDirectory(prefix="rustmux-pointer-") as temporary:
    root = Path(temporary)
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"pointer-{os.getpid()}"
    session = None
    counter = 0
    probe = root / "probe.py"
    probe.write_text(r'''
import json, os, sys, tty
from pathlib import Path
tty.setraw(0)
path = Path(sys.argv[1])
def emit(command):
    os.write(1, b"\x1b]22;" + command.encode() + b"\x1b\\")
emit(sys.argv[2])
os.write(1, b"POINTER_READY\r\n")
path.write_text(json.dumps({"token": 0, "pid": os.getpid()}) + "\n")
while True:
    line = bytearray()
    while not line.endswith(b"\n"):
        byte = os.read(0, 1)
        if not byte: sys.exit(0)
        line.extend(byte)
    token, operation, value = line.decode().rstrip("\n").split(":", 2)
    reply = None
    if operation == "osc": emit(value)
    elif operation == "alt": os.write(1, b"\x1b[?1049h")
    elif operation == "main": os.write(1, b"\x1b[?1049l")
    elif operation == "reset": os.write(1, b"\x1bc")
    elif operation == "soft": os.write(1, b"\x1b[!p")
    elif operation == "query":
        emit("?" + value)
        data = bytearray()
        while not data.endswith(b"\x1b\\"): data.extend(os.read(0, 1))
        reply = data.decode()
    with path.open("a") as output:
        output.write(json.dumps({"token": int(token), "pid": os.getpid(), "reply": reply}) + "\n")
    os.write(1, f"POINTER_ACK_{token}\r\n".encode())
''')

    def run(action, *args):
        target = [name] if action in ("new", "kill") else ["-s", name]
        result = subprocess.run([BINARY, action, *target, *map(str, args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, (action, args, result)
        return result.stdout

    def wait(predicate):
        deadline = time.monotonic() + 5
        while not predicate():
            if session:
                session.read(0.01)
            assert time.monotonic() < deadline, bytes(session.output[-2000:]) if session else "detached"
            time.sleep(0.01)

    def records(path):
        if not path.exists():
            return []
        # An independent reader may observe the writer between write and close.
        return [json.loads(line) for line in path.read_text().splitlines(keepends=True) if line.endswith("\n")]

    def command(pane, path, operation, value=""):
        global counter
        counter += 1
        token = counter
        run("send-keys", "-p", pane, "--literal", f"{token}:{operation}:{value}\n")
        wait(lambda: any(item["token"] == token for item in records(path)))
        return next(item for item in records(path) if item["token"] == token)

    def query(pane, path, expected):
        item = command(pane, path, "query", "__current__")
        assert item["reply"] == f"\x1b]22;{expected}\x1b\\", item

    def expect_shape(shape):
        encoded = shape.encode()
        def matches():
            shapes = re.findall(rb"\x1b\]22;([^\x1b]*)\x1b\\", session.output)
            return bool(shapes) and shapes[-1] == encoded
        wait(matches)
        session.output.clear()

    def detach():
        global session
        session.send(b"\x02d")
        session.finish(0)
        assert b"\x1b]22;\x1b\\\x1b[0m\x1b[?25h\x1b[?1049l" in session.output
        session.close()
        session = None

    try:
        run("new", "--detached")
        panes = tomllib.loads(run("list-panes", "--toml"))["panes"]
        left = panes[0]["id"]
        a = root / "a.jsonl"
        b = root / "b.jsonl"
        launch_a = f"exec python3 {shlex.quote(str(probe))} {shlex.quote(str(a))} pointer"
        run("send-keys", "-p", left, "--literal", "--enter", launch_a)
        wait(lambda: bool(records(a)))
        launch_b = f"exec python3 {shlex.quote(str(probe))} {shlex.quote(str(b))} wait"
        right = int(run("split-pane", "-p", left, "--command", launch_b))
        wait(lambda: bool(records(b)))
        identities = {p["id"]: p["pid"] for p in tomllib.loads(run("list-panes", "--toml"))["panes"]}
        query(left, a, "pointer")  # Both queries also work without a displayed client.
        query(right, b, "wait")
        session = Session(extra_env=env, arguments=("attach", name))
        expect_shape("wait")
        session.expect(b"POINTER_READY")
        run("select-pane", "-p", left)
        expect_shape("pointer")
        command(right, b, "osc", "crosshair")
        query(right, b, "crosshair")
        deadline = time.monotonic() + 0.15
        while time.monotonic() < deadline: session.read(0.01)
        assert b"\x1b]22;crosshair\x1b\\" not in session.output
        assert b"\x1b]22;?" not in session.output
        run("select-pane", "-p", right)
        expect_shape("crosshair")
        command(right, b, "osc", ">wait,pointer")
        query(right, b, "pointer")
        expect_shape("pointer")
        command(right, b, "osc", "=text")
        expect_shape("text")
        command(right, b, "osc", "<")
        query(right, b, "wait")  # A set did not discard the previous pushes.
        expect_shape("wait")
        command(right, b, "alt")
        expect_shape("")
        query(right, b, "0")
        command(right, b, "osc", "grab")
        expect_shape("grab")
        command(right, b, "main")
        query(right, b, "wait")
        expect_shape("wait")
        session.send(b"\x02?")
        expect_shape("")
        session.expect(b"Shortcut Help")
        session.send(b"q")
        expect_shape("wait")
        session.send(b"\x02[")
        expect_shape("")
        expect_footer(session, b"HISTORY")
        session.send(b"q")
        expect_shape("wait")
        detach()
        command(right, b, "alt")
        query(right, b, "grab")
        command(right, b, "osc", "=zoom-in")
        query(right, b, "zoom-in")
        session = Session(extra_env=env, arguments=("attach", name))
        expect_shape("zoom-in")
        command(right, b, "soft")
        query(right, b, "0")
        expect_shape("")
        command(right, b, "main")
        query(right, b, "0")
        command(right, b, "osc", "help")
        expect_shape("help")
        command(right, b, "reset")
        expect_shape("")
        query(right, b, "0")
        run("select-pane", "-p", left)
        expect_shape("pointer")
        assert {p["id"]: p["pid"] for p in tomllib.loads(run("list-panes", "--toml"))["panes"]} == identities
        assert {item["pid"] for item in records(a)} == {records(a)[0]["pid"]}
        assert {item["pid"] for item in records(b)} == {records(b)[0]["pid"]}
        detach()
    finally:
        if session:
            session.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

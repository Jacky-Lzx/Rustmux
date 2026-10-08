"""Explicit scripted closure cleans up owned children and preserves survivors."""
import json
import os
from pathlib import Path
import shlex
import socket
import struct
import subprocess
import tempfile
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_footer


with tempfile.TemporaryDirectory(prefix="rustmux-pane-close-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("remain_on_exit=true\nsave_scrollback=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"pane-close-{os.getpid()}"
    runtime = Path(f"/tmp/rustmux-{os.geteuid()}")
    client = None
    probe = root / "probe.py"
    probe.write_text("""import json,os,select,sys,tty
tty.setraw(0)
os.write(1,b'\\x1b[?1004h\\x1b[?1h\\x1b[2J\\x1b[HCLOSE_PROBE_READY')
events=b''
previous=None
while True:
    if select.select([0],[],[],0.01)[0]:
        data=os.read(0,1024)
        if not data: break
        events+=data
    size=os.get_terminal_size(0)
    state={'pid':os.getpid(),'rows':size.lines,'columns':size.columns,'input':events.hex()}
    if state!=previous:
        with open(sys.argv[1]+'.tmp','w') as log: json.dump(state,log)
        os.replace(sys.argv[1]+'.tmp',sys.argv[1])
        previous=state
""")

    def run(action, *arguments, success=True):
        result = subprocess.run([BINARY, *action.split(), "-s", name, *map(str, arguments)],
                                env=env, capture_output=True, text=True, timeout=8)
        assert (result.returncode == 0) == success, (action, arguments, result)
        return result

    def panes():
        return tomllib.loads(run("pane list", "--toml").stdout)["panes"]

    def active():
        return next(p["id"] for p in panes() if p["active"])

    def record(label):
        path = root / f"{label}.json"
        return json.loads(path.read_text()) if path.exists() else None

    def wait_until(predicate, detail):
        deadline = time.monotonic() + 5
        while not predicate():
            if client:
                client.read(0.01)
            assert time.monotonic() < deadline, detail
            time.sleep(0.01)

    def command(label):
        return "exec python3 -u " + shlex.quote(str(probe)) + " " + shlex.quote(str(root / f"{label}.json"))

    def start(action, label, *arguments):
        pane = int(run(action, *arguments, "--command", command(label)).stdout)
        wait_until(lambda: record(label) is not None, f"probe {label} did not start")
        assert record(label)["pid"] == next(p["pid"] for p in panes() if p["id"] == pane)
        return pane

    def size(label, rows, columns):
        wait_until(lambda: (record(label)["rows"], record(label)["columns"]) == (rows, columns),
                   ("PTY size mismatch", label, rows, columns))

    def gone(pid):
        try:
            os.kill(pid, 0)
            return False
        except ProcessLookupError:
            return True

    def close(pane=None):
        target = active() if pane is None else pane
        pid = next(p["pid"] for p in panes() if p["id"] == target)
        arguments = ["-p", str(pane)] if pane is not None else []
        assert run("pane close", *arguments).stdout == ""
        assert all(p["id"] != target for p in panes())
        wait_until(lambda: gone(pid), f"closed child {pid} was not reaped")

    def wire(body):
        with socket.socket(socket.AF_UNIX) as peer:
            peer.settimeout(3)
            peer.connect(str(runtime / f"{name}.control"))
            peer.sendall(struct.pack("!I", len(body)) + body)
            response = bytearray()
            while len(response) < 4:
                chunk = peer.recv(4096)
                assert chunk, response
                response.extend(chunk)
            length = struct.unpack("!I", response[:4])[0]
            while len(response) < length + 4:
                chunk = peer.recv(4096)
                assert chunk, response
                response.extend(chunk)
            return tomllib.loads(response[4:].decode())

    def detach():
        global client
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None

    try:
        client = Session(extra_env=env, arguments=("new", name))
        client.expect(b"RUSTMUX_READY>")
        first = panes()[0]
        run("pane send-keys", "-p", first["id"], "--literal", "--enter", command("first"))
        wait_until(lambda: record("first") is not None, "first probe did not start")
        upper = start("pane split", "upper", "-p", first["id"])
        lower = start("pane split", "lower", "-p", upper, "--down")
        logs = start("window new", "logs", "--name", "logs")
        size("lower", 9, 38)
        wait_until(lambda: bytes.fromhex(record("lower")["input"]).endswith(b"\x1b[O"),
                   "setup focus-out did not arrive")
        baseline = {label: record(label) for label in ("first", "lower", "logs")}
        server_pid = (runtime / f"{name}.pid").read_bytes()
        close(upper)
        size("lower", 20, 38)
        assert active() == logs
        assert next(p for p in panes() if p["id"] == lower)["selected"]
        for label in baseline:
            assert record(label)["pid"] == baseline[label]["pid"]
            assert record(label)["input"] == baseline[label]["input"], "inactive close emitted focus bytes"
        close()  # remove the active sole-pane window
        assert active() == lower
        wait_until(lambda: record("lower")["input"] == baseline["lower"]["input"] + b"\x1b[I".hex(),
                   "fallback pane did not receive exactly one focus-in")
        client.send(b"x")
        wait_until(lambda: record("lower")["input"].endswith(b"x".hex()), "keyboard input reached wrong pane")
        assert record("first")["input"] == baseline["first"]["input"]
        client.send(b"\x02Z")
        size("lower", 20, 78)
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        for invalid in (upper, logs, 99999):
            assert "unknown runtime pane ID" in run("pane close", "-p", invalid, success=False).stderr
        assert not wire(b"action='close-pane'\npane='invalid'\n")["ok"]
        assert not wire(f"action='close-pane'\npane={lower}\nextra=true\n".encode())["ok"]
        assert panes() == before
        client.send(b"/close-check")
        expect_footer(client, b"Search /close-check")
        lower_input = record("lower")["input"]
        close(first["id"])  # close the hidden sibling of a zoomed pane
        expect_footer(client, b"LOCKED")
        size("lower", 20, 78)
        assert client.private_modes.get(1), "close changed survivor application cursor mode"
        assert active() == lower and record("lower")["input"] == lower_input
        client.send(b"\x02z")  # scripted closure must not enter interactive undo
        client.read(0.05)
        assert len(panes()) == 1 and active() == lower
        assert (runtime / f"{name}.pid").read_bytes() == server_pid
        detach()

        client = Session(extra_env=env, arguments=("attach", name))
        client.expect(b"CLOSE_PROBE_READY")
        middle = start("pane split", "middle", "-p", lower)
        bottom = start("pane split", "bottom", "-p", middle, "--down")
        run("pane select", "-p", middle)
        close()  # active traversal successor
        assert active() == bottom
        size("bottom", 20, 38)
        close()  # active traversal predecessor at the end
        assert active() == lower
        size("lower", 20, 78)
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        assert "final session pane" in run("pane close", success=False).stderr
        rejected = wire(b"action='close-pane'\n")
        assert not rejected["ok"] and "final session pane" in rejected["output"]
        assert panes() == before and not gone(before[0]["pid"])
        client.send(b"/final-check")
        expect_footer(client, b"Search /final-check")
        run("pane select", "-p", lower)
        expect_footer(client, b"LOCKED")
        detach()

        exited = int(run("window new", "--name", "exited", "--command", "printf CLOSED_EXITED; exit 7").stdout)
        wait_until(lambda: any(p["id"] == exited and p["output_complete"] for p in panes()), "job did not exit")
        assert next(p for p in panes() if p["id"] == exited)["exit_code"] == 7
        keep = start("window new", "keep", "--name", "keep")
        baseline = record("keep")
        close(exited)  # detached retained pane removes its inactive window
        assert active() == keep and record("keep") == baseline
        assert next(p["window"] for p in panes() if p["id"] == keep) == 2
        close()  # detached active-window fallback
        assert active() == lower
        before = panes()
        run("pane close", "-p", lower, success=False)
        assert panes() == before
        subprocess.run([BINARY, "save", name], env=env, capture_output=True, check=True, timeout=8)
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        saved = tomllib.loads(snapshot.read_text())
        assert len(saved["windows"]) == 1 and len(saved["windows"][0]["panes"]) == 1
        assert saved["windows"][0]["panes"][0]["command"] == command("lower")
        assert not saved["windows"][0]["layout"]["zoomed"]
        dead = {label: record(label)["pid"] for label in ("first", "upper", "logs", "middle", "bottom", "keep")}
        old_lower = record("lower")["pid"]
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, check=True, timeout=8)
        client = Session(extra_env=env, arguments=("attach", name, "--create"))
        client.expect(b"CLOSE_PROBE_READY")
        wait_until(lambda: record("lower")["pid"] != old_lower, "saved survivor was not restored")
        size("lower", 20, 78)
        assert len(panes()) == 1
        assert all(record(label)["pid"] == pid and gone(pid) for label, pid in dead.items()), "closed job was replayed"
        detach()
    finally:
        if client:
            client.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

print("script pane close: child cleanup, inactive/zoom focus, bounds, detached retention and saved survivors passed")

"""Control requests coexist with an attached client and preserve moved panes."""
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import time
import tomllib
from terminal_loop_support import BINARY, Session


with tempfile.TemporaryDirectory(prefix="rustmux-control-") as root:
    env = dict(os.environ, XDG_STATE_HOME=root, XDG_CONFIG_HOME=root,
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"control-{os.getpid()}"
    endpoint = Path(f"/tmp/rustmux-{os.geteuid()}/{name}.control")

    def command(action, *args, success=True):
        result = subprocess.run([BINARY, action, "-s", name, *map(str, args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert (result.returncode == 0) == success, (action, args, result)
        return result.stdout

    def panes():
        return tomllib.loads(command("list-panes", "--toml"))["panes"]

    def wait_text(pane, text):
        deadline = time.monotonic() + 6
        while True:
            # Keep consuming the attached frontend's frames; its ordinary
            # backpressure otherwise correctly pauses active-pane output reads.
            session.read(seconds=0.01)
            captured = command("capture-pane", "-p", pane, "--history")
            if text in captured:
                break
            assert time.monotonic() < deadline, (text, captured, panes())
            time.sleep(0.02)

    session = Session(extra_env=env, arguments=("new", name))
    stalled = None
    try:
        session.expect(b"RUSTMUX_READY>")
        assert endpoint.stat().st_mode & 0o777 == 0o600
        first = panes()[0]
        assert first["active"]
        # Neither idle controllers nor malformed frames can stop terminal input.
        stalled = socket.socket(socket.AF_UNIX)
        stalled.connect(str(endpoint))
        for packet in [struct.pack("!I", 65537), struct.pack("!I", 3) + b"???"]:
            with socket.socket(socket.AF_UNIX) as peer:
                peer.connect(str(endpoint))
                peer.sendall(packet)
        session.send(b"stty -echo; KEEP=preserved; printf 'PTY_%s\\n' ALIVE\n")
        session.expect(b"PTY_ALIVE")
        # Bypass CLI validation and fragment a wire request to check server-side
        # limits and incremental header/body assembly.
        body = (f'action="send-keys"\npane={first["id"]}\nbytes=[' +
                ','.join(['0'] * 4097) + ']\n').encode()
        header = struct.pack("!I", len(body))
        with socket.socket(socket.AF_UNIX) as peer:
            peer.settimeout(3)
            peer.connect(str(endpoint))
            peer.sendall(header[:2])
            time.sleep(0.01)
            peer.sendall(header[2:] + body[:100])
            time.sleep(0.01)
            peer.sendall(body[100:])
            response = bytearray()
            while len(response) < 4:
                chunk = peer.recv(4096)
                assert chunk, "server closed without rejecting input"
                response.extend(chunk)
            length = struct.unpack("!I", response[:4])[0]
            while len(response) < length + 4:
                chunk = peer.recv(4096)
                assert chunk
                response.extend(chunk)
            rejected = tomllib.loads(response[4:].decode())
            assert not rejected["ok"] and "4096" in rejected["output"], rejected
        moved = int(command("new-window", "--name", "logs"))
        split = int(command("split-pane", "-p", moved, "--down"))
        assert len({p["id"] for p in panes()}) == 3
        command("send-keys", "-p", first["id"], "--literal", "--enter", "printf 'CONTROL_%s\\n' \"$KEEP\"")
        wait_text(first["id"], "CONTROL_preserved")
        assert next(p for p in panes() if p["active"])["id"] == split
        before = panes()
        command("split-pane", "-p", 999999, success=False)
        command("send-keys", "-p", first["id"], "--literal", "x" * 4097, success=False)
        assert panes() == before
        assert int(command("join-pane", "-p", first["id"], "--to-pane", moved)) == first["id"]
        assert next(p for p in panes() if p["id"] == first["id"])["pid"] == first["pid"]
        assert int(command("break-pane", "-p", first["id"], "--name", "kept")) == first["id"]
        assert next(p for p in panes() if p["id"] == first["id"])["pid"] == first["pid"]
        command("send-keys", "-p", first["id"], "--literal", "--enter", "printf 'MOVED_%s\\n' \"$KEEP\"")
        wait_text(first["id"], "MOVED_preserved")
        session.send(b"\x02d")
        session.finish(0)
        command("send-keys", "-p", first["id"], "--literal", "--enter", "printf 'DETACHED_%s\\n' CONTROL")
        wait_text(first["id"], "DETACHED_CONTROL")
    finally:
        if stalled:
            stalled.close()
        session.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)
    assert not endpoint.exists()

print("attached/detached control, bounds, failure isolation and stable moved IDs passed")

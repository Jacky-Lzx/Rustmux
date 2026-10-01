"""Other-client detach, terminal restoration and acknowledged lease release."""
import fcntl
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import tempfile
import termios
import time
import tomllib
from terminal_loop_support import BINARY, Session


def run(env, *args, success=True):
    result = subprocess.run([BINARY, *args], env=dict(os.environ, **env),
                            capture_output=True, text=True, timeout=8)
    assert (result.returncode == 0) == success, (args, result)
    return result


def expect(session, *texts):
    deadline = time.monotonic() + 6
    while not all(text in session.output for text in texts):
        session.read()
        other = globals().get("client")
        if other is not None and other is not session:
            other.read(0)  # Drain the other real terminal while observing the manager.
        assert time.monotonic() < deadline, (texts, bytes(session.output[-2000:]))
    data = bytes(session.output)
    session.output.clear()
    session.frames.clear()
    return data


def resize(client):
    fcntl.ioctl(client.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 160, 0, 0))


def read_exact(stream, count):
    data = bytearray()
    while len(data) < count:
        chunk = stream.recv(count - len(data))
        assert chunk, "control endpoint closed before reply"
        data.extend(chunk)
    return data


with tempfile.TemporaryDirectory(prefix="rustmux-disconnect-") as temporary:
    root = Path(temporary)
    settings = root / "manager.toml"
    settings.write_text("save_scrollback=true\n")
    env = {"XDG_CONFIG_HOME": str(root / "config"), "XDG_STATE_HOME": str(root / "state"),
           "RUSTMUX_SHELL": "/bin/sh", "PS1": "RUSTMUX_READY> ", "ENV": "", "BASH_ENV": ""}
    name = f"drop-{os.getpid()}"
    current = f"operator-{os.getpid()}"
    saved_name = f"offline-{os.getpid()}"
    runtime = Path(f"/tmp/rustmux-{os.geteuid()}")
    state = root / "state/rustmux/main-human/sessions"
    client = picker = None
    stopped = None
    server_stopped = None
    try:
        client = Session(extra_env=env, arguments=("--config", str(settings), "new", name))
        client.expect(b"RUSTMUX_READY>")
        client.send(b"stty -echo; KEEP=survives; printf 'DETACH_READY\\n'\n")
        client.expect(b"DETACH_READY")
        panes = tomllib.loads(run(env, "list-panes", "-s", name, "--toml").stdout)["panes"]
        original_pid = (runtime / f"{name}.pid").read_bytes()
        run(env, "save", name)
        snapshot = state / f"{name}.toml"
        original_snapshot = snapshot.read_bytes()
        offline = state / f"{saved_name}.toml"
        offline.write_bytes(original_snapshot)
        offline.chmod(0o600)
        # A stale manager request must not act on a replacement server.
        with socket.socket(socket.AF_UNIX) as wire:
            wire.settimeout(3)
            wire.connect(str(runtime / f"{name}.control"))
            request = b"action='disconnect-session'\nserver_pid=1\n"
            wire.sendall(struct.pack("!I", len(request)) + request)
            length = struct.unpack("!I", read_exact(wire, 4))[0]
            reply = tomllib.loads(read_exact(wire, length).decode())
            assert not reply["ok"] and "server changed" in reply["output"], reply
        assert client.child.poll() is None
        picker = Session(extra_env=env, arguments=("--config", str(settings), "attach"))
        resize(picker)
        expect(picker, b"Session Manager")
        picker.send(("/" + name).encode())
        expect(picker, b"[ATTACHED]", b"<Ctrl-X> Disconnect")
        # The server can deliver Detach while this client is stopped, but the
        # manager must not report success until the displayed lease is released.
        os.kill(client.app_pid, signal.SIGSTOP)
        stopped = client.app_pid
        picker.send(b"\x18\x18")
        expect(picker, ("Disconnecting " + name).encode())
        picker.send(b"x")
        captured = expect(picker, ("Search: " + name + "x_").encode())
        assert ("Disconnected " + name).encode() not in captured
        run(env, "attach", name, success=False)  # Workspace lock blocks replacement.
        os.kill(stopped, signal.SIGCONT)
        stopped = None
        client.finish(0)
        client.close()
        client = None
        expect(picker, ("Disconnected " + name).encode())
        assert (runtime / f"{name}.pid").read_bytes() == original_pid
        assert snapshot.read_bytes() == original_snapshot
        after = tomllib.loads(run(env, "list-panes", "-s", name, "--toml").stdout)["panes"]
        assert (after[0]["id"], after[0]["pid"]) == (panes[0]["id"], panes[0]["pid"])
        picker.send(b"\x7f")
        expect(picker, b"[DETACHED]")
        picker.send(b"\x18")
        expect(picker, b"Disconnect failed: selected session")
        picker.send(b"\x1b")
        expect(picker, b"<dd> Kill")
        picker.send(b"/" + saved_name.encode())
        expect(picker, b"[SAVED]", b"1 SESSION")
        picker.send(b"\x18")
        expect(picker, b"Disconnect failed: selected session")
        picker.send(b"\x1b")
        expect(picker, b"<Enter>")
        picker.send(b"q")
        picker.finish(0)
        picker.close()
        picker = None

        # A manager opened from another session keeps its own return target and
        # hot-reloads the disconnect binding independently of the target server.
        client = Session(extra_env=env, arguments=("attach", name))
        client.expect(b"RUSTMUX_READY>")
        client.send(b"printf 'KEEP:%s\\n' ${KEEP}\n")
        client.expect(b"KEEP:survives")
        run(env, "new", current, "--detached")
        picker = Session(extra_env=env, arguments=("--config", str(settings), "attach", current))
        resize(picker)
        picker.expect(b"RUSTMUX_READY>")
        picker.send(b"\x02\x17")
        expect(picker, b"[CURRENT]")
        picker.send(b"\x18")
        expect(picker, b"Disconnect failed: current session")
        settings.write_text('[session_manager]\ndisconnect=["Ctrl g"]\n')
        picker.send(("/" + name).encode())
        expect(picker, b"<Ctrl-G> Disconnect")
        picker.send(b"\x18")  # Replaced default cannot disconnect.
        deadline = time.monotonic() + 0.2
        while time.monotonic() < deadline:
            picker.read(0.02)
        assert client.child.poll() is None
        picker.send(b"\x07")
        expect(picker, ("Disconnected " + name).encode())
        client.finish(0)
        client.close()
        client = None
        picker.send(b"\x1b")
        expect(picker, b"<dd> Kill")
        picker.send(b"q")
        picker.expect(b"RUSTMUX_READY>")
        picker.send(b"\x02d")
        picker.finish(0)
        picker.close()
        picker = None

        # Timeout is an explicit uncertain result; it never kills the stopped
        # client or its pane. Resume permits the already accepted detach to finish.
        client = Session(extra_env=env, arguments=("attach", name))
        client.expect(b"RUSTMUX_READY>")
        settings.write_text('[session_manager]\ndisconnect=["Ctrl x"]\n')
        picker = Session(extra_env=env, arguments=("--config", str(settings), "attach"))
        resize(picker)
        expect(picker, b"Session Manager")
        picker.send(("/" + name).encode())
        expect(picker, b"[ATTACHED]")
        os.kill(client.app_pid, signal.SIGSTOP)
        stopped = client.app_pid
        picker.send(b"\x18")
        expect(picker, b"Disconnect failed: client did not finish")
        assert client.child.poll() is None
        os.kill(stopped, signal.SIGCONT)
        stopped = None
        client.finish(0)
        client.close()
        client = None
        run(env, "show-config", "-s", name)
        assert (runtime / f"{name}.pid").read_bytes() == original_pid
        assert offline.read_bytes() == original_snapshot
        picker.send(b"\x1b")
        expect(picker, b"<Enter>")
        picker.send(b"q")
        picker.finish(0)
        picker.close()
        picker = None

        # Disconnect control can be processed before EOF from a client that
        # abruptly died. A failed Detach write must not end the named server.
        client = Session(extra_env=env, arguments=("attach", name))
        client.expect(b"RUSTMUX_READY>")
        server_pid = int(original_pid)
        os.kill(server_pid, signal.SIGSTOP)
        server_stopped = server_pid
        with socket.socket(socket.AF_UNIX) as wire:
            wire.settimeout(3)
            wire.connect(str(runtime / f"{name}.control"))
            request = f"action='disconnect-session'\nserver_pid={server_pid}\n".encode()
            wire.sendall(struct.pack("!I", len(request)) + request)
            os.kill(client.app_pid, signal.SIGKILL)
            client.child.wait(timeout=3)
            client.close()
            client = None
            os.kill(server_stopped, signal.SIGCONT)
            server_stopped = None
            length = struct.unpack("!I", read_exact(wire, 4))[0]
            reply = tomllib.loads(read_exact(wire, length).decode())
            assert reply["ok"] or "no attached client" in reply["output"], reply
        run(env, "show-config", "-s", name)
        assert (runtime / f"{name}.pid").read_bytes() == original_pid
        after = tomllib.loads(run(env, "list-panes", "-s", name, "--toml").stdout)["panes"]
        assert after[0]["pid"] == panes[0]["pid"]
    finally:
        if server_stopped:
            os.kill(server_stopped, signal.SIGCONT)
        if stopped:
            os.kill(stopped, signal.SIGCONT)
        if picker:
            picker.close()
        if client:
            client.close()
        for target in (name, current):
            subprocess.run([BINARY, "kill", target], env=dict(os.environ, **env),
                           capture_output=True, timeout=8)
        for target in (name, current, saved_name):
            (runtime / f"{target}.workspace").unlink(missing_ok=True)

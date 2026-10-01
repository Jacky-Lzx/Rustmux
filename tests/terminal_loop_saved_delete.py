"""Saved workspace deletion: confirmation, key reload, errors and live refresh."""
import fcntl
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import termios
import time

from terminal_loop_support import BINARY, Session


def command(env, *args):
    result = subprocess.run([BINARY, *args], env=dict(os.environ, **env),
                            capture_output=True, text=True, timeout=15)
    assert result.returncode == 0, (args, result)
    return result.stdout


def expect(session, *texts):
    deadline = time.monotonic() + 6
    while not all(text in session.output for text in texts):
        session.read()
        assert time.monotonic() < deadline, (texts, bytes(session.output[-2500:]))
    session.output.clear()
    session.frames.clear()


with tempfile.TemporaryDirectory(prefix="rustmux-delete-") as root:
    root = Path(root)
    config = root / "config" / "rustmux"
    config.mkdir(parents=True)
    settings = config / "config.toml"
    settings.write_text("")
    env = {"XDG_CONFIG_HOME": str(config.parent), "XDG_STATE_HOME": str(root / "state"),
           "RUSTMUX_SHELL": "/bin/sh", "ENV": "", "BASH_ENV": ""}
    name = f"delete-{os.getpid()}"
    other = f"keep-{os.getpid()}"
    state = root / "state" / "rustmux" / "main-human" / "sessions"
    path = state / f"{name}.toml"
    retained = state / f"{other}.toml"
    try:
        command(env, "new", name, "--detached")
        command(env, "save", name)
        command(env, "kill", name)
        before = path.read_bytes()
        retained.write_bytes(before)
        retained.chmod(0o600)
        session = Session(extra_env=env, arguments=("attach",))
        try:
            fcntl.ioctl(session.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 160, 0, 0))
            expect(session, b"Session Manager")
            session.send(("/" + name).encode())
            expect(session, b"<Enter> Restore", b"[SAVED]")
            session.send(b"\x1b")
            expect(session, b"<dd> Delete")
            session.send(b"d")
            expect(session, f"again to delete saved '{name}'".encode())
            assert path.read_bytes() == before
            session.send(b"x")
            expect(session, b"<dd> Delete")
            assert path.read_bytes() == before

            settings.write_text('[session_manager]\ndelete = ["Ctrl x"]\ndisconnect = []\n')
            expect(session, b"<Ctrl-X twice> Delete")
            session.send(b"dd")  # The old shortcut is disabled.
            time.sleep(0.1)
            assert path.read_bytes() == before
            lock_path = Path(f"/tmp/rustmux-{os.geteuid()}/{name}.workspace")
            with lock_path.open("r+") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                session.send(b"\x18\x18")
                expect(session, b"Delete failed")
                assert path.read_bytes() == before
                assert name in command(env, "ls").splitlines()
                fcntl.flock(lock, fcntl.LOCK_UN)
            session.send(b"\x18")
            expect(session, f"Press Ctrl-X again to delete saved '{name}'".encode())
            session.send(b"\x18")
            expect(session, f"Deleted {name}".encode())
            assert not path.exists()
            assert name not in command(env, "ls").splitlines()
            assert retained.read_bytes() == before
            session.send(("/" + name).encode())
            expect(session, b"0 SESSIONS")  # The open manager refreshed too.
            session.send(b"\x1b")
            expect(session, b"Session Manager")
            session.send(("/" + other).encode())
            expect(session, b"[SAVED]", b"<Enter> Restore")
            session.send(b"\x1b")
            expect(session, b"<Ctrl-X twice> Delete")
            session.send(b"\x18\x18")
            expect(session, f"Deleted {other}".encode())
            assert not retained.exists()
            assert other not in command(env, "ls").splitlines()
            session.send(b"q")
            session.finish(0)
        finally:
            session.close()
    finally:
        # Only this test's server may be stopped if setup failed.
        subprocess.run([BINARY, "kill", name], env=dict(os.environ, **env),
                       capture_output=True, timeout=10)
        for target in (name, other):
            Path(f"/tmp/rustmux-{os.geteuid()}/{target}.workspace").unlink(missing_ok=True)

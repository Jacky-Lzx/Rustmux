"""Rename active/current/detached sessions without losing processes or leases."""
import fcntl
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import termios
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_bar


def command(env, *args, success=True):
    result = subprocess.run([BINARY, *args], env=dict(os.environ, **env),
                            capture_output=True, text=True, timeout=12)
    assert (result.returncode == 0) == success, (args, result)
    return result


def expect(session, *texts):
    deadline = time.monotonic() + 6
    while not all(text in session.output for text in texts):
        session.read()
        assert time.monotonic() < deadline, (texts, bytes(session.output[-2500:]))
    session.output.clear()
    session.frames.clear()


def resize(session):
    fcntl.ioctl(session.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 160, 0, 0))


def edit(session, old, new):
    session.send(b"\x7f" * len(old) + new.encode() + b"\r")


def panes(env, name):
    return tomllib.loads(command(env, "list-panes", "-s", name, "--toml").stdout)["panes"]


with tempfile.TemporaryDirectory(prefix="rustmux-live-rename-") as root:
    root = Path(root)
    config = root / "config" / "rustmux"
    config.mkdir(parents=True)
    settings = config / "config.toml"
    settings.write_text("save_scrollback = true\nscrollback_lines = 100\n")
    env = {"XDG_CONFIG_HOME": str(config.parent), "XDG_STATE_HOME": str(root / "state"),
           "RUSTMUX_SHELL": "/bin/sh", "PS1": "RUSTMUX_READY> ", "ENV": "", "BASH_ENV": ""}
    old = f"live-{os.getpid()}"
    new = f"changed-{os.getpid()}"
    final = f"current-{os.getpid()}"
    detached = f"detached-{os.getpid()}"
    keep = f"keep-{os.getpid()}"
    runtime = Path(f"/tmp/rustmux-{os.geteuid()}")
    state = root / "state" / "rustmux" / "main-human" / "sessions"
    saved = lambda name: state / f"{name}.toml"
    client = None
    picker = None
    try:
        client = Session(extra_env=env, arguments=("new", old))
        client.expect(b"RUSTMUX_READY>")
        client.send(b"stty -echo; KEEP=retained; printf 'LIVE_%s\\n' BEFORE\n")
        client.expect(b"LIVE_BEFORE")
        original_panes = panes(env, old)
        original_pid = (runtime / f"{old}.pid").read_bytes()
        original_lock = (runtime / f"{old}.lock").stat().st_ino
        command(env, "save", old)
        before = saved(old).read_bytes()
        saved(keep).write_bytes(before + b"\n# Collision sentinel.\n")
        saved(keep).chmod(0o600)
        kept = saved(keep).read_bytes()
        original_last = (runtime / f"{old}.last").read_bytes()

        # Stale wire controllers cannot rename a replacement server.
        with socket.socket(socket.AF_UNIX) as wire:
            wire.settimeout(3)
            wire.connect(str(runtime / f"{old}.control"))
            request = f"action='rename-session'\nsource='{old}'\nname='{new}'\nserver_pid=1\n".encode()
            wire.sendall(struct.pack("!I", len(request)) + request)
            header = bytearray()
            while len(header) < 4:
                chunk = wire.recv(4 - len(header))
                assert chunk, "control endpoint closed before reply"
                header.extend(chunk)
            length = struct.unpack("!I", header)[0]
            data = bytearray()
            while len(data) < length:
                chunk = wire.recv(length - len(data))
                assert chunk, "control endpoint closed during reply"
                data.extend(chunk)
            reply = tomllib.loads(data.decode())
            assert not reply["ok"] and "server changed" in reply["output"], reply
        assert (runtime / f"{old}.sock").exists() and not (runtime / f"{new}.sock").exists()

        picker = Session(extra_env=env, arguments=("attach",))
        resize(picker)
        expect(picker, b"Session Manager")
        picker.send(("/" + old).encode())
        expect(picker, b"[ATTACHED]", b"<Ctrl-R> Rename")
        picker.send(b"\x12")
        expect(picker, ("Rename session: " + old + "_").encode())
        edit(picker, old, keep)
        expect(picker, b"Rename failed")
        assert saved(old).read_bytes() == before and saved(keep).read_bytes() == kept
        assert (runtime / f"{old}.pid").read_bytes() == original_pid
        edit(picker, keep, new)
        expect(picker, ("Renamed " + old + " to " + new).encode())
        assert not saved(old).exists() and saved(new).read_bytes() == before
        assert (runtime / f"{new}.pid").read_bytes() == original_pid
        assert (runtime / f"{new}.lock").stat().st_ino == original_lock
        assert (runtime / f"{new}.last").read_bytes() == original_last
        for extension in ("sock", "pid", "lock", "last", "save", "control"):
            assert not (runtime / f"{old}.{extension}").exists(), extension
            assert (runtime / f"{new}.{extension}").exists(), extension
        assert panes(env, new)[0]["pid"] == original_panes[0]["pid"]
        assert panes(env, new)[0]["id"] == original_panes[0]["id"]
        expect_bar(client, new.encode())
        client.send(b"printf 'KEEP:%s\\n' ${KEEP}\n")
        client.expect(b"KEEP:retained")
        busy = command(env, "attach", new, success=False)
        assert "already has an attached client" in busy.stderr, busy
        command(env, "show-config", "-s", new)
        command(env, "show-config", "-s", old, success=False)
        command(env, "save", new)
        assert saved(new).exists() and not saved(old).exists()
        picker.send(b"q")
        picker.finish(0)
        picker.close()
        picker = None
        client.send(b"\x02\x17")
        expect(client, b"[CURRENT]", new.encode())
        client.send(b"q")
        client.expect(b"RUSTMUX_READY>")
        expect_bar(client, new.encode())
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None

        # Rename the session that opened the manager, then cancel back to the
        # new name. Save follows the current session rather than selected rows.
        client = Session(extra_env=env, arguments=("attach", new))
        resize(client)
        client.expect(b"RUSTMUX_READY>")
        client.send(b"\x02\x17")
        expect(client, b"[CURRENT]")
        client.send(b"\x12")
        expect(client, ("Rename session: " + new + "_").encode())
        edit(client, new, final)
        expect(client, ("Renamed " + new + " to " + final).encode(), b"[CURRENT]")
        client.send(b"\x01")
        expect(client, ("Saved " + final).encode())
        assert saved(final).exists() and not saved(new).exists()
        client.send(b"q")
        client.expect(b"RUSTMUX_READY>")
        expect_bar(client, final.encode())
        client.send(b"printf 'CURRENT_KEEP:%s\\n' ${KEEP}\n")
        client.expect(b"CURRENT_KEEP:retained")
        assert panes(env, final)[0]["pid"] == original_panes[0]["pid"]
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None

        # A detached rename uses the same event loop and starts no new panes.
        picker = Session(extra_env=env, arguments=("attach",))
        resize(picker)
        expect(picker, b"Session Manager")
        picker.send(("/" + final).encode())
        expect(picker, b"[DETACHED]")
        picker.send(b"\x12")
        expect(picker, b"Rename session:")
        edit(picker, final, detached)
        expect(picker, ("Renamed " + final + " to " + detached).encode())
        picker.send(b"q")
        picker.finish(0)
        picker.close()
        picker = None
        previous_save = saved(detached).stat().st_mtime_ns
        settings.write_text("save_scrollback = true\nautosave_interval_seconds = 1\n")
        deadline = time.monotonic() + 5
        while saved(detached).stat().st_mtime_ns == previous_save:
            assert time.monotonic() < deadline, "autosave did not follow rename"
            time.sleep(0.05)
        assert not any(saved(name).exists() for name in (old, new, final))
        command(env, "new", old, "--detached")  # Old alias is safely reusable.
        reused_pid = (runtime / f"{old}.pid").read_bytes()
        assert reused_pid != original_pid
        command(env, "kill", detached)
        for extension in ("sock", "pid", "lock", "last", "save", "control"):
            assert not (runtime / f"{detached}.{extension}").exists(), extension
        assert (runtime / f"{old}.pid").read_bytes() == reused_pid
        command(env, "show-config", "-s", old)
        assert saved(detached).exists() and saved(keep).read_bytes() == kept
    finally:
        if picker is not None:
            picker.close()
        if client is not None:
            client.close()
        for name in (old, new, final, detached):
            subprocess.run([BINARY, "kill", name], env=dict(os.environ, **env),
                           capture_output=True, timeout=12)
        for name in (old, new, final, detached, keep):
            (runtime / f"{name}.workspace").unlink(missing_ok=True)

"""Real PTY save/restart/restore, detached saves, failure isolation and autosave."""
import os
import fcntl
from pathlib import Path
import shlex
import socket
import struct
import subprocess
import tempfile
import termios
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_bar, expect_footer


def command(env, *arguments, success=True):
    result = subprocess.run([BINARY, *arguments], env=dict(os.environ, **env),
                            capture_output=True, text=True, timeout=15)
    assert (result.returncode == 0) == success, (arguments, result)
    return result


def wait_until(predicate, detail):
    deadline = time.monotonic() + 8
    while not predicate():
        assert time.monotonic() < deadline, detail
        time.sleep(0.02)


def expect_picker(session, *texts):
    # Picker frames use absolute writes, rather than the pane renderer's frame
    # delimiters used by Session.expect(). Inspect their raw output instead.
    deadline = time.monotonic() + 8
    while not all(text in session.output for text in texts):
        session.read()
        assert time.monotonic() < deadline, (texts, bytes(session.output[-2000:]))
    session.output.clear()
    session.frames.clear()


with tempfile.TemporaryDirectory(prefix="rustmux-snapshots-") as root:
    root = Path(root)
    config = root / "config" / "rustmux"
    config.mkdir(parents=True)
    settings = config / "config.toml"
    settings.write_text("save_scrollback = true\nsave_scrollback_colors = true\nscrollback_lines = 100\n")
    working = root / "saved cwd"
    working.mkdir()
    env = {"XDG_CONFIG_HOME": str(config.parent), "XDG_STATE_HOME": str(root / "state"),
           "RUSTMUX_SHELL": "/bin/sh", "PS1": "RUSTMUX_READY> ", "ENV": "", "BASH_ENV": ""}
    name = f"snapshot-{os.getpid()}"
    path = root / "state" / "rustmux" / "main-human" / "sessions" / f"{name}.toml"
    endpoint = Path(f"/tmp/rustmux-{os.geteuid()}/{name}.save")

    def new():
        return Session(extra_env=env, arguments=("new", name))

    # All panes and focus survive restoration, but shell variables/PIDs do not.
    session = new()
    try:
        session.expect(b"RUSTMUX_READY>")
        session.send(("stty -echo; KEEP=old; cd " + shlex.quote(str(working)) +
                      "; printf '\\033[31mSAVED_%s\\033[0m\\n' LEFT\n").encode())
        session.expect(b"SAVED_LEFT")
        session.send(b"\x02%")
        session.expect(b"RUSTMUX_READY>")
        session.send(b"stty -echo; printf 'SAVED_%s\\n' RIGHT\n")
        session.expect(b"SAVED_RIGHT")
        session.send(b"\x02c")
        session.expect(b"RUSTMUX_READY>")
        session.send(b"\x02,logs\r")
        expect_bar(session, b"logs")
        session.send(b"stty -echo; KEEP=old; printf 'SAVED_%s\\n' LOGS\n")
        session.expect(b"SAVED_LOGS")
        fcntl.ioctl(session.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        session.send(b"printf 'SAVED_SIZE:%s\\n' \"$(stty size)\"\n")
        session.expect(b"SAVED_SIZE:26 98")
        command(env, "save-session", name)
        saved = tomllib.loads(path.read_text())
        assert (saved["rows"], saved["columns"]) == (30, 100), saved
        assert saved["active_window"] == 1 and len(saved["windows"]) == 2, saved
        assert len(saved["windows"][0]["panes"]) == 2, saved
        assert saved["windows"][0]["layout"]["active"] == 1, saved
        assert saved["windows"][1]["name"] == "logs", saved
        assert "SAVED_LOGS" in path.read_text()
        assert any("\x1b[" in row["text"] for window in saved["windows"]
                   for pane in window["panes"] for row in pane["history"]), "styled history was not encoded"
        assert path.stat().st_mode & 0o777 == 0o600
        assert path.parent.stat().st_mode & 0o777 == 0o700

        # Idle/malformed save clients cannot stall PTYs. Concurrent valid saves
        # complete without requiring a second interactive attachment.
        stalled = socket.socket(socket.AF_UNIX)
        stalled.connect(str(endpoint))
        try:
            session.send(b"printf 'INPUT_%s\\n' ALIVE\n")
            session.expect(b"INPUT_ALIVE")
            jobs = [subprocess.Popen([BINARY, "save-session", name], env=dict(os.environ, **env),
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE) for _ in range(3)]
            for job in jobs:
                output, error = job.communicate(timeout=15)
                assert job.returncode == 0, (output, error)
        finally:
            stalled.close()
        malformed = socket.socket(socket.AF_UNIX)
        try:
            malformed.connect(str(endpoint))
            malformed.settimeout(2)
            malformed.sendall(b"X")
            assert malformed.recv(64) == b"", "malformed save request was not discarded"
        finally:
            malformed.close()
        session.send(b"\x02d")
        session.finish(0)
    finally:
        session.close()
    command(env, "save", name)  # detached save
    before_restart = path.read_bytes()
    command(env, "kill", name)
    assert path.read_bytes() == before_restart, "default-off autosave changed the manual snapshot"
    assert not endpoint.exists()

    assert command(env, "ls").stdout.splitlines().count(name) == 1
    listing = command(env, "ls", "--long").stdout
    assert "SAVED" in next(line for line in listing.splitlines() if line.startswith(name + " ")), listing
    # A second saved workspace guarantees the manager opens instead of using
    # its single-session shortcut, independent of other running test sessions.
    other_path = path.with_name(f"other-{os.getpid()}.toml")
    other_path.write_bytes(before_restart)
    other_path.chmod(0o600)

    session = Session(extra_env=env, arguments=("attach",))
    try:
        expect_picker(session, b"Session Manager")
        session.send(("/" + name).encode())
        expect_picker(session, b"<Enter> Restore", b"[SAVED]")
        session.send(b"\x1b")
        expect_picker(session, b"<Enter> Restore  <a> New")
        session.send(b"dx\r")  # Another key cancels saved-workspace deletion.
        session.expect(b"RUSTMUX_READY>")
        assert command(env, "ls").stdout.splitlines().count(name) == 1
        row = next(line for line in command(env, "ls", "--long").stdout.splitlines() if line.startswith(name + " "))
        assert "ATTACHED" in row and "SAVED" not in row, row
        expect_bar(session, b"logs")
        assert b"SAVED_LOGS" not in b"".join(session.last_rows), "saved output replaced the fresh live screen"
        session.send(b"stty -echo; printf 'FRESH:%s\\n' ${KEEP-unset}\n")
        session.expect(b"FRESH:unset")
        session.send(b"\x02[g")
        expect_footer(session, b"HISTORY")
        session.expect(b"SAVED_LOGS")
        session.send(b"q\x021")
        session.send(b"stty -echo; printf 'RIGHT_SIZE:%s\\n' \"$(stty size)\"\n")
        session.expect(b"RIGHT_SIZE:20 38")
        session.send(("\x02hstty -echo; [ \"$PWD\" = " + shlex.quote(str(working.resolve())) +
                      " ] && printf 'LEFT_CWD_%s\\n' OK\n").encode())
        session.expect(b"LEFT_CWD_OK")
        # Restore text reflows when the outer terminal changes width.
        session.send(b"\x02[g")
        expect_footer(session, b"HISTORY")
        session.expect(b"SAVED_LEFT")
        session.send(b"q\x02d")
        session.finish(0)
    finally:
        session.close()
        if endpoint.with_suffix(".sock").exists():
            command(env, "kill", name)
        other_path.unlink()

    # Disabling saved history still restores the layout and starts fresh shells.
    settings.write_text("save_scrollback = false\n")
    session = new()
    try:
        session.expect(b"RUSTMUX_READY>")
        expect_bar(session, b"logs")
        session.send(b"\x02[g")
        expect_footer(session, b"HISTORY")
        assert b"SAVED_LOGS" not in b"".join(session.physical_rows)
        session.send(b"q\x02d")
        session.finish(0)
    finally:
        session.close()
        command(env, "kill", name)

    # Missing working directories fall back; invalid snapshots fail before a
    # server starts and leave the saved evidence untouched.
    working.rmdir()
    session = new()
    try:
        session.expect(b"RUSTMUX_READY>")
        session.send(b"\x021\x02hstty -echo; printf 'MISSING_%s\\n' FALLBACK\n")
        session.expect(b"MISSING_FALLBACK")
        session.send(b"\x02d")
        session.finish(0)
    finally:
        session.close()
        command(env, "kill", name)
    good = path.read_bytes()
    path.write_text("version = 999\n")
    invalid = command(env, "new", name, "--detached", success=False)
    assert path.read_text() == "version = 999\n"
    assert name in command(env, "list").stdout.splitlines()
    assert not endpoint.with_suffix(".sock").exists(), "invalid restoration launched a server"
    path.write_bytes(good)

    # A failed disk save reports an error and preserves the live server.
    command(env, "new", name, "--detached")
    path.chmod(0o644)
    failure = command(env, "save", name, success=False)
    assert "unsafe snapshot" in failure.stderr, failure
    assert name in command(env, "list").stdout.splitlines()
    session = Session(extra_env=env, arguments=("attach", name))
    try:
        session.expect(b"RUSTMUX_READY>")
        expect_footer(session, b"Save failed:")
        session.send(b"stty -echo; printf 'FAILED_SAVE_%s\\n' ALIVE\n")
        session.expect(b"FAILED_SAVE_ALIVE")
        session.send(b"\x02d")
        session.finish(0)
    finally:
        session.close()
    assert name in command(env, "list").stdout.splitlines(), "a save failure killed the session on detach"
    path.chmod(0o600)
    command(env, "save", name)
    command(env, "kill", name)

    # Opt-in autosave writes while detached and checkpoints detach/shutdown.
    name = f"autosave-{os.getpid()}"
    path = path.parent / f"{name}.toml"
    settings.write_text("autosave_interval_seconds = 1\nsave_scrollback = true\n")
    session = new()
    try:
        session.expect(b"RUSTMUX_READY>")
        session.send(b"stty -echo; printf 'AUTO_%s\\n' BEFORE; (sleep 2; printf 'AUTO_%s\\n' DETACHED) &\n")
        session.expect(b"AUTO_BEFORE")
        session.send(b"\x02d")
        session.finish(0)
        assert path.exists() and "AUTO_BEFORE" in path.read_text()
        wait_until(lambda: "AUTO_DETACHED" in path.read_text(), "detached autosave did not capture new output")
    finally:
        session.close()
        command(env, "kill", name)
    assert "AUTO_DETACHED" in path.read_text()

print("snapshot save/restart/restore, detached saves and autosave passed")

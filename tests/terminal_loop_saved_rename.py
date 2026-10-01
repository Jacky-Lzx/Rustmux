"""Saved rename: editing, collision/race safety, key reload and restoration."""
import fcntl
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import termios
import time

from terminal_loop_support import BINARY, Session, expect_bar, expect_footer


def command(env, *args):
    result = subprocess.run([BINARY, *args], env=dict(os.environ, **env),
                            capture_output=True, text=True, timeout=15)
    assert result.returncode == 0, (args, result)
    return result.stdout


def expect(session, *texts):
    deadline = time.monotonic() + 6
    while not all(text in session.output for text in texts):
        session.read()
        assert time.monotonic() < deadline, (texts, bytes(session.output[-3000:]))
    session.output.clear()
    session.frames.clear()


def resize(session, rows):
    fcntl.ioctl(session.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, 160, 0, 0))


def replace_name(session, previous, next_name):
    session.send(b"\x7f" * len(previous) + next_name.encode())
    expect(session, next_name.encode() + b"_")


with tempfile.TemporaryDirectory(prefix="rustmux-rename-") as root:
    root = Path(root)
    config = root / "config" / "rustmux"
    config.mkdir(parents=True)
    settings = config / "config.toml"
    settings.write_text("save_scrollback = true\nscrollback_lines = 100\n")
    env = {"XDG_CONFIG_HOME": str(config.parent), "XDG_STATE_HOME": str(root / "state"),
           "RUSTMUX_SHELL": "/bin/sh", "PS1": "RUSTMUX_READY> ", "ENV": "", "BASH_ENV": ""}
    old = f"rename-{os.getpid()}"
    new = f"renamed-{os.getpid()}"
    keep = f"keep-{os.getpid()}"
    live = f"live-rename-{os.getpid()}"
    state = root / "state" / "rustmux" / "main-human" / "sessions"
    source = state / f"{old}.toml"
    target = state / f"{new}.toml"
    retained = state / f"{keep}.toml"
    runtime = Path(f"/tmp/rustmux-{os.geteuid()}")
    try:
        session = Session(extra_env=env, arguments=("new", old))
        try:
            session.expect(b"RUSTMUX_READY>")
            session.send(b"stty -echo; KEEP=old; printf 'RENAME_%s\\n' HISTORY\n")
            session.expect(b"RENAME_HISTORY")
            session.send(b"\x02%")
            session.expect(b"RUSTMUX_READY>")
            session.send(b"stty -echo; printf 'RENAME_%s\\n' RIGHT\n")
            session.expect(b"RENAME_RIGHT")
            command(env, "save", old)
            session.send(b"\x02d")
            session.finish(0)
        finally:
            session.close()
        command(env, "kill", old)
        before = source.read_bytes()
        retained.write_bytes(before + b"\n# Existing destination must survive.\n")
        retained.chmod(0o600)
        kept = retained.read_bytes()

        # Cancel preserves both the original snapshot and the search query.
        session = Session(extra_env=env, arguments=("attach",))
        try:
            resize(session, 24)
            expect(session, b"Session Manager")
            session.send(("/" + old).encode())
            expect(session, b"[SAVED]", b"<Ctrl-R> Rename")
            session.send(b"\x12")
            expect(session, ("Rename session: " + old + "_").encode(), b"<Enter> Rename")
            resize(session, 6)
            expect(session, ("Rename: " + old + "_").encode())
            session.send(b"\x1b")
            expect(session, ("Search: " + old + "_").encode())
            resize(session, 24)
            session.send(b"\x12")
            expect(session, b"Rename session:")
            replace_name(session, old, keep)
            session.send(b"\r")
            expect(session, b"Rename failed", ("Rename session: " + keep + "_").encode())
            assert source.read_bytes() == before and retained.read_bytes() == kept
            # Empty confirmation gives an inline error without closing the editor.
            session.send(b"\x7f" * len(keep) + b"\r")
            expect(session, b"Invalid name:", b"Rename session: _")
            session.send(b"\x1b")
            expect(session, b"Search:")
            session.send(b"\x1b")
            expect(session, b"Session Manager")
            session.send(b"q")
            session.finish(0)
        finally:
            session.close()

        # Source/target runtime changes are rechecked at confirmation; the
        # worker refuses live targets and keeps the editor usable for retries.
        session = Session(extra_env=env, arguments=("attach",))
        try:
            resize(session, 24)
            expect(session, b"Session Manager")
            session.send(("/" + old).encode())
            expect(session, b"[SAVED]")
            session.send(b"\x12")
            expect(session, b"Rename session:")
            replace_name(session, old, live)
            command(env, "new", live, "--detached")
            session.send(b"\r")
            expect(session, b"Rename failed")
            assert source.read_bytes() == before
            command(env, "kill", live)
            replace_name(session, live, new)
            command(env, "new", old, "--detached")
            session.send(b"\r")
            expect(session, b"Rename failed")
            assert source.read_bytes() == before and not target.exists()
            command(env, "kill", old)
            with (runtime / f"{new}.workspace").open("a+") as lock:
                os.fchmod(lock.fileno(), 0o600)
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                session.send(b"\r")
                expect(session, b"Rename failed")
                assert source.read_bytes() == before and not target.exists()
                fcntl.flock(lock, fcntl.LOCK_UN)
            # Reload keeps editor text; d/r/a remain text, even when bound.
            settings.write_text('save_scrollback = true\n[session_manager]\nrename=["r"]\n')
            time.sleep(0.65)
            session.send(b"\x7f" * len(new) + b"drar")
            expect(session, b"Rename session: drar_")
            replace_name(session, "drar", new)
            session.send(b"\r\r")  # Repeated confirmation queues one operation.
            expect(session, ("Renamed " + old + " to " + new).encode())
            assert not source.exists() and target.read_bytes() == before
            assert retained.read_bytes() == kept
            names = command(env, "ls").splitlines()
            assert old not in names and new in names and keep in names
            session.send(b"/")
            expect(session, b"Search:")
            session.send(old.encode())
            expect(session, b"0 SESSIONS")
            session.send(b"\x1b")
            expect(session, b"Session Manager")
            session.send(("/" + new).encode())
            expect(session, b"[SAVED]")
            # r is search text; return to the table before using it as Rename.
            session.send(b"\x1b")
            expect(session, b"Session Manager")
            session.send(b"\x12")  # The replaced Ctrl-R no longer opens an editor.
            session.send(b"r")
            expect(session, ("Rename session: " + new + "_").encode())
            session.send(b"\x1b")
            expect(session, b"Session Manager")
            session.send(b"q")
            session.finish(0)
        finally:
            session.close()

        # Restore under the new name through the manager's fork path. Split
        # layout and saved history survive; live shells begin afresh.
        session = Session(extra_env=env, arguments=("attach",))
        try:
            resize(session, 24)
            expect(session, b"Session Manager")
            session.send(("/" + new).encode())
            expect(session, b"[SAVED]", b"<Enter> Restore")
            session.send(b"\r")
            session.expect(b"RUSTMUX_READY>")
            expect_bar(session, new.encode())
            session.send(b"\x02[g")
            expect_footer(session, b"HISTORY")
            session.expect(b"RENAME_RIGHT")
            session.send(b"q\x02hstty -echo; printf 'FRESH:%s\\n' ${KEEP-unset}\n")
            session.expect(b"FRESH:unset")
            session.send(b"\x02[g")
            expect_footer(session, b"HISTORY")
            session.expect(b"RENAME_HISTORY")
            session.send(b"q\x02\x17")
            expect(session, b"Session Manager")
            session.send(b"r")
            expect(session, ("Rename session: " + new + "_").encode())
            session.send(b"\r")
            expect(session, ("Renamed " + new + " to " + new).encode())
            session.send(b"q")
            session.expect(b"RUSTMUX_READY>")
            session.send(b"\x02d")
            session.finish(0)
        finally:
            session.close()
    finally:
        for name in (old, new, live):
            subprocess.run([BINARY, "kill", name], env=dict(os.environ, **env),
                           capture_output=True, timeout=10)
        for name in (old, new, keep, live):
            (runtime / f"{name}.workspace").unlink(missing_ok=True)

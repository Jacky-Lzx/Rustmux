"""Configured attachment/reset modes leave explicit Locked bindings intact."""
import fcntl
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import termios
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_bar, expect_footer


with tempfile.TemporaryDirectory(prefix="rustmux-default-mode-") as temporary:
    root = Path(temporary)
    config = root / "selected.toml"

    def write_mode(mode):
        config.write_text(f'''default_mode="{mode}"
[keybinds.normal]
esc={{actions=[{{action="switch-mode",mode="locked"}}]}}
''')

    # Every implemented mode initializes the actual decoder, not just its label.
    # An unnamed foreground session cannot start session-only manager/detach actions.
    for mode in ("locked", "normal", "pane", "resize", "move", "tab", "session"):
        write_mode(mode)
        s = Session(arguments=("--config", str(config)))
        try:
            s.expect(b"RUSTMUX_READY>")
            expect_footer(s, ("locked" if mode == "session" else mode).upper().encode())
            if mode != "locked" and mode != "session":
                s.send(b"\x1b")
                expect_footer(s, b"LOCKED")
            s.send(b"printf 'INPUT_%s\\n' SURVIVED\n")
            s.expect(b"INPUT_SURVIVED")
            s.send(b"exit 0\n")
            s.finish(0)
        finally:
            s.close()

    # Reset after a prompt or focused process exit follows the configured policy.
    # Ordinary action chains and History exits explicitly targeting Locked do not.
    write_mode("normal")
    s = Session(arguments=("--config", str(config)), lifetime=35)
    try:
        s.expect(b"RUSTMUX_READY>")
        expect_footer(s, b"NORMAL")
        s.send(b"c")
        expect_bar(s, b"2 shell")
        expect_footer(s, b"LOCKED")
        s.send(b"\x02,")
        expect_footer(s, b"RENAME")
        s.send(b"\x15renamed\r")
        expect_bar(s, b"2 renamed")
        expect_footer(s, b"NORMAL")
        s.send(b",")
        expect_footer(s, b"RENAME")
        s.send(b"\x1b")
        expect_footer(s, b"NORMAL")
        s.send(b"?")
        s.expect(b"Shortcut Help")
        s.send(b"c")
        expect_bar(s, b"3 shell")
        expect_footer(s, b"LOCKED")
        s.send(b"exit 0\n")
        expect_footer(s, b"NORMAL")
        s.send(b"?")
        s.expect(b"Shortcut Help")
        s.send(b"\x1b")
        expect_footer(s, b"LOCKED")
        s.send(b"\x02[")
        expect_footer(s, b"HISTORY")
        s.send(b"q")
        expect_footer(s, b"LOCKED")
        s.send(b"\x02[")
        expect_footer(s, b"HISTORY")
        fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 26, 82, 0, 0))
        expect_footer(s, b"NORMAL")
        s.send(b"\x1b")
        expect_footer(s, b"LOCKED")
        s.send(b"exit 0\n")
        expect_bar(s, b"1 shell")
        expect_footer(s, b"NORMAL")
        s.send(b"\x1b")
        expect_footer(s, b"LOCKED")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()

    name = f"default-mode-{os.getpid()}"
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    other = root / "other.toml"
    other.write_text('default_mode="normal"\n')
    client = None

    def run(action, *args):
        target = [name] if action in ("new", "kill") else ["-s", name]
        result = subprocess.run([BINARY, action, *target, *map(str, args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, result
        return result

    def status():
        return tomllib.loads(run("show-config").stdout)

    def wait(predicate):
        deadline = time.monotonic() + 6
        while True:
            if client:
                client.read(seconds=0.01)
            current = status()
            if predicate(current):
                return current
            assert time.monotonic() < deadline, current
            time.sleep(0.01)

    def attach(mode):
        global client
        client = Session(extra_env=env, arguments=("--config", str(other), "attach", name), lifetime=35)
        client.expect(b"RUSTMUX_READY>")
        expect_footer(client, mode)

    def detach():
        global client
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None

    try:
        write_mode("session")
        run("new", "--detached", "--config", config)
        attach(b"SESSION")
        # Attached reload waits for Locked, preserving the mode currently in use.
        write_mode("pane")
        pending = wait(lambda c: c["pending"])
        assert pending["settings"]["default_mode"] == "session"
        expect_footer(client, b"SESSION")
        client.send(b"\x1b")
        expect_footer(client, b"LOCKED")
        changed = wait(lambda c: c["settings"]["default_mode"] == "pane" and not c["pending"])
        expect_footer(client, b"LOCKED")
        detach()
        attach(b"PANE")  # Server policy wins over the attaching client's config.
        client.send(b"\x1b")
        expect_footer(client, b"LOCKED")
        client.send(b"\x02\x17")
        # The client-owned manager uses its own full-screen output rather than
        # the server's pane-frame protocol reconstructed by Session.expect().
        deadline = time.monotonic() + 6
        while b"[CURRENT]" not in client.output:
            client.read()
            assert time.monotonic() < deadline, bytes(client.output[-2000:])
        client.send(b"q")
        expect_footer(client, b"PANE")
        client.send(b"\x1b")
        expect_footer(client, b"LOCKED")
        detach()
        # Detached reload changes the next attachment, and invalid updates retain it.
        write_mode("resize")
        updated = wait(lambda c: c["settings"]["default_mode"] == "resize")
        assert updated["generation"] > changed["generation"]
        write_mode("history")
        failed = wait(lambda c: "error" in c)
        assert failed["settings"]["default_mode"] == "resize"
        assert failed["generation"] == updated["generation"]
        # Reload errors replace the entire footer, including the mode label.
        # Recover the same policy before asserting its visible attachment state.
        write_mode("resize")
        recovered = wait(lambda c: "error" not in c)
        assert recovered["generation"] == updated["generation"]
        attach(b"RESIZE")
        client.send(b"\x1b")
        expect_footer(client, b"LOCKED")
        client.send(b"printf 'SERVER_%s\\n' SURVIVED\n")
        client.expect(b"SERVER_SURVIVED")
        detach()
    finally:
        if client:
            client.close()
        run("kill")

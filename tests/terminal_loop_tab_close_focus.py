"""Configured close focus at startup and after attached/detached reloads."""
import os
from pathlib import Path
import subprocess
import tempfile
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_bar


with tempfile.TemporaryDirectory(prefix="rustmux-close-focus-") as directory:
    root = Path(directory)
    config = root / "config.toml"
    for policy in ("left", "right"):
        config.write_text(f'tab_close_focus="{policy}"\n')
        session = Session(arguments=("--config", str(config)), lifetime=30)
        try:
            session.expect(b"RUSTMUX_READY> ")
            for label in (b"A", b"B", b"C"):
                if label != b"A":
                    session.send(b"\x02c")
                    session.expect(b"RUSTMUX_READY> ")
                session.send(b"\x02," + label + b"\r")
                session.send(b"WIN=" + label + b"\n")
                session.expect(b"RUSTMUX_READY> ")
            session.send(b"\x022\x02&")
            session.expect(b"Close window? Type yes:")
            session.send(b"yes\r")
            expect_bar(session, b"2 C")
            expected = b"A" if policy == "left" else b"C"
            session.send(b"printf '\\n%s:%s\\n' FIRST $WIN\n")
            session.expect(b"FIRST:" + expected)

            # Recreate a middle tab and change the policy without moving focus.
            session.send(b"\x021\x02c")
            session.expect(b"RUSTMUX_READY> ")
            session.send(b"\x02,B\r")
            expect_bar(session, b"2 B")
            other = "right" if policy == "left" else "left"
            config.write_text(f'tab_close_focus="{other}"\n')
            deadline = time.monotonic() + 1.2
            while time.monotonic() < deadline:
                session.read(0.02)
            # Closing the final pane of B must use the reloaded policy too.
            session.send(b"\x02x")
            session.expect(b"Close pane? Type yes:")
            session.send(b"yes\r")
            expect_bar(session, b"2 C")
            expected = b"C" if other == "right" else b"A"
            session.send(b"printf '\\n%s:%s\\n' RELOADED $WIN\n")
            session.expect(b"RELOADED:" + expected)

            # Natural process exit from a middle tab uses the same policy.
            session.send(b"\x021\x02c")
            session.expect(b"RUSTMUX_READY> ")
            session.send(b"\x02,B\r")
            expect_bar(session, b"2 B")
            session.send(b"exit\n")
            expect_bar(session, b"2 C")
            session.send(b"printf '\\n%s:%s\\n' EXITED $WIN\n")
            session.expect(b"EXITED:" + expected)
        finally:
            session.close()

    # A detached named server uses the setting before its first attachment and
    # updates it through reload, including OS/machine override resolution.
    name = f"close-focus-{os.getpid()}"
    env = dict(os.environ, RUSTMUX_SHELL="/bin/sh", XDG_STATE_HOME=str(root / "state"))

    def run(*args):
        result = subprocess.run([BINARY, *args], env=env, capture_output=True,
                                text=True, timeout=8)
        assert result.returncode == 0, result
        return result.stdout

    def panes():
        return tomllib.loads(run("pane", "list", "-s", name, "--toml"))["panes"]

    def active():
        return next(p["id"] for p in panes() if p["active"])

    try:
        config.write_text('tab_close_focus="left"\n')
        run("new", name, "--detached", "--config", str(config))
        a = active()
        run("window", "new", "-s", name)
        b = active()
        run("window", "new", "-s", name)
        c = active()
        run("window", "select", "-s", name, "-w", "2")
        run("window", "close", "-s", name)
        assert active() == a, panes()
        run("window", "new", "-s", name)
        run("window", "select", "-s", name, "-w", "2")
        # A root value overridden by the current OS must reach the model.
        target_os = "macos" if os.uname().sysname == "Darwin" else "linux"
        config.write_text(f'tab_close_focus="left"\n[overrides.os.{target_os}]\n'
                          'tab_close_focus="right"\n')
        deadline = time.monotonic() + 5
        while True:
            report = tomllib.loads(run("config", "show", "-s", name))
            if report["settings"]["tab_close_focus"] == "right":
                break
            assert time.monotonic() < deadline, report
            time.sleep(0.02)
        assert active() == c, panes()
        last = panes()[-1]["id"]
        run("window", "close", "-s", name)
        assert active() == last, panes()
        assert all(p["id"] != b for p in panes())
    finally:
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

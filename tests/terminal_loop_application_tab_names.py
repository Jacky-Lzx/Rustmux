"""Automatic tabs follow real foreground jobs, independent of OSC title output."""
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_bar, expect_bar_without

with tempfile.TemporaryDirectory(prefix="rustmux-application-tabs-") as temporary:
    root = Path(temporary)
    config = root / "config/rustmux/config.toml"
    config.parent.mkdir(parents=True)
    # Omit tab_name to exercise the actual application-mode default.
    config.write_text("autosave_interval_seconds=0\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root / "config"),
               XDG_STATE_HOME=str(root / "state"), RUSTMUX_SHELL="/bin/sh",
               PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"application-tabs-{os.getpid()}"
    client = None
    # macOS /bin/sh reports its underlying Bash executable through proc_name.
    shell_name = "bash" if sys.platform == "darwin" else "sh"

    def command(*args):
        result = subprocess.run([BINARY, *map(str, args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, result
        return result.stdout

    def panes():
        return tomllib.loads(command("pane", "list", "-s", name, "--toml"))["panes"]

    def expect_names(names):
        deadline = time.monotonic() + 5
        while True:
            actual = [pane["window_name"] for pane in panes()]
            if actual == names:
                return
            if client:
                client.read(0.05)
            else:
                time.sleep(0.05)
            assert time.monotonic() < deadline, (actual, names)

    try:
        client = Session(extra_env=env, arguments=("new", name), lifetime=40)
        client.expect(b"RUSTMUX_READY>")
        client.send(b"stty -echo; printf '\\033]2;~/D/P/R/Rustmux\\007'\n")
        expect_bar(client, f"1 {shell_name}".encode())
        client.read(0.35)
        assert b"~/D/P/R/Rustmux" not in client.last_rows[0], client.last_rows
        client.send(b"sleep 1\n")
        expect_bar(client, b"1 sleep")
        expect_bar(client, f"1 {shell_name}".encode())

        # Focus and background tab updates work without title or prompt markers.
        client.send(b"\x02%")
        client.expect(b"RUSTMUX_READY>")
        client.send(b"sleep 2\n")
        expect_bar(client, b"1 sleep")
        client.send(b"\x02h")
        expect_bar(client, f"1 {shell_name}".encode())
        client.send(b"\x02l")
        expect_bar(client, b"1 sleep")
        client.send(b"\x02c")
        client.expect(b"RUSTMUX_READY>")
        expect_bar(client, f"2 {shell_name}".encode())
        expect_bar(client, b"1 sleep")
        expect_bar(client, f"1 {shell_name}".encode())

        # Explicit names survive jobs; clearing a rename restores automatic mode.
        client.send(b"\x02,custom\r")
        expect_bar(client, b"2 custom")
        client.send(b"sleep 1\n")
        client.read(0.4)
        assert b"2 custom" in client.last_rows[0], client.last_rows
        client.send(b"\x02,\r")
        expect_bar(client, b"2 sleep")
        expect_bar(client, f"2 {shell_name}".encode())
        expect_names([shell_name, shell_name, shell_name])

        # Reload changes labels without new pane output, and diagnostics report it.
        client.send(b"printf '\\033]2;directory title\\007'\n")
        config.write_text('tab_name="title"\n')
        expect_bar(client, b"2 directory title")
        config.write_text('tab_name="application"\n')
        expect_bar(client, f"2 {shell_name}".encode())
        status = tomllib.loads(command("config", "show", "-s", name))
        assert status["settings"]["tab_name"] == "application", status
        config.write_text('tab_name="invalid"\n')
        deadline = time.monotonic() + 5
        while not tomllib.loads(command("config", "show", "-s", name)).get("error"):
            client.read(0.05)
            assert time.monotonic() < deadline
        expect_bar(client, f"2 {shell_name}".encode())
        config.write_text('tab_name="application"\n')

        # The same live label is available through script control while detached.
        client.send(b"sleep 1.5\n")
        expect_bar(client, b"2 sleep")
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None
        expect_names([shell_name, shell_name, "sleep"])
        expect_names([shell_name, shell_name, shell_name])
        command("save", name)
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        assert all(w["name"] == "" for w in tomllib.loads(snapshot.read_text())["windows"])
        command("kill", name)
        client = Session(extra_env=env, arguments=("attach", name, "--create"), lifetime=20)
        expect_bar(client, f"2 {shell_name}".encode())
        expect_names([shell_name, shell_name, shell_name])
        client.send(b"\x02d")
        client.finish(0)
    finally:
        if client:
            client.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

    # Exercise the user's exact applications when installed; portable CI uses sh/sleep above.
    fish, yazi = shutil.which("fish"), shutil.which("yazi")
    if fish and yazi:
        wrapper = root / "start-fish"
        wrapper.write_text(f"#!/bin/sh\nexec {shlex.quote(fish)} --no-config\n")
        wrapper.chmod(0o700)
        empty = root / "empty"
        empty.mkdir()
        client = Session(shell=str(wrapper), extra_env={
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_STATE_HOME": str(root / "state"),
        }, lifetime=20)
        try:
            expect_bar(client, b"1 fish")
            client.send(f"cd {shlex.quote(str(empty))}; {shlex.quote(yazi)}\n".encode())
            expect_bar(client, b"1 yazi")
            client.read(0.35)
            assert str(empty).encode() not in client.last_rows[0], client.last_rows
            client.send(b"q")
            expect_bar(client, b"1 fish")
            client.send(b"\x02c")
            expect_bar(client, b"2 fish")
            client.send(b"exit\n")
            expect_bar_without(client, b"2 fish")
            client.send(b"exit\n")
            client.finish(0)
        finally:
            client.close()
        print("real Fish -> Yazi -> Fish and a new Fish tab passed")

print("application tab defaults, jobs, focus, background, explicit names, reload and restore passed")

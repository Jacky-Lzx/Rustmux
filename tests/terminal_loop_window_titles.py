"""Automatic tab titles follow focused panes without replacing explicit names."""
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import tomllib

from terminal_loop_support import BINARY, Session, expect_bar, expect_bar_without


def set_title(client, title, code=2, terminator="\\007", delay=0):
    command = f"printf '\\033]{code};%s{terminator}' {shlex.quote(title)}\n"
    if delay:
        command = f"sleep {delay}; " + command
    client.send(command.encode())


client = Session(lifetime=30)
try:
    client.expect(b"RUSTMUX_READY>")
    client.send(b"stty -echo\n")
    set_title(client, "left pane")
    expect_bar(client, b"1 left pane")
    set_title(client, "\u7f16\u8f91\u5668", code=0, terminator="\\033\\\\")
    expect_bar(client, "1 \u7f16\u8f91\u5668".encode())
    set_title(client, "   ")
    expect_bar(client, b"1 shell")
    set_title(client, "left pane")
    expect_bar(client, b"1 left pane")
    client.send(b"\x02%")
    client.expect(b"RUSTMUX_READY>")
    set_title(client, "right pane")
    expect_bar(client, b"1 right pane")
    client.send(b"\x02h")
    expect_bar(client, b"1 left pane")
    client.send(b"\x02l\x02!")
    expect_bar(client, b"1 left pane")
    expect_bar(client, b"2 right pane")

    # An inactive tab must update even when the displayed pane has no new output.
    client.send(b"\x021")
    set_title(client, "background changed", delay=0.3)
    client.send(b"\x022")
    expect_bar(client, b"1 background changed")
    expect_bar(client, b"2 right pane")

    # A literal explicit 'shell' is distinct from the automatic fallback.
    client.send(b"\x02,shell\r")
    expect_bar(client, b"2 shell")
    set_title(client, "named pane changed")
    client.send(b"printf 'NAMED_TITLE_DONE\\n'\n")
    client.expect(b"NAMED_TITLE_DONE")
    assert b"2 shell" in client.last_rows[0], client.last_rows
    assert b"2 named pane changed" not in client.last_rows[0], client.last_rows
    client.send(b"\x02,\r")
    expect_bar(client, b"2 named pane changed")

    # Cancelling a draft keeps the automatic mode and reveals the latest title.
    set_title(client, "changed during rename", delay=0.3)
    client.send(b"\x02,draft")
    expect_bar(client, b"2 draft")
    client.read(0.5)
    client.send(b"\x07")
    expect_bar(client, b"2 changed during rename")
    expect_bar_without(client, b"2 draft")
    client.send(b"exit\n")
    expect_bar_without(client, b"2 changed during rename")
    client.send(b"exit\n")
    client.finish(0)
finally:
    client.close()


with tempfile.TemporaryDirectory(prefix="rustmux-tab-titles-") as temporary:
    root = Path(temporary)
    config = root / "config/rustmux/config.toml"
    config.parent.mkdir(parents=True)
    config.write_text('tab_name="title"\n' + "autosave_interval_seconds=0\nsave_scrollback=false\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root / "config"),
               XDG_STATE_HOME=str(root / "state"), RUSTMUX_SHELL="/bin/sh",
               PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"tab-titles-{os.getpid()}"
    client = None

    def command(*args):
        result = subprocess.run([BINARY, *map(str, args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, result
        return result.stdout

    def panes():
        return tomllib.loads(command("pane", "list", "-s", name, "--toml"))["panes"]

    def detach():
        global client
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None

    try:
        client = Session(extra_env=env, arguments=("new", name), lifetime=30)
        client.expect(b"RUSTMUX_READY>")
        set_title(client, "initial automatic")
        expect_bar(client, b"1 initial automatic")
        command("window", "rename", "-s", name, "shell")
        expect_bar(client, b"1 shell")
        command("window", "new", "-s", name)
        client.expect(b"RUSTMUX_READY>")
        set_title(client, "script automatic")
        expect_bar(client, b"2 script automatic")
        assert [p["window_name"] for p in panes()] == ["shell", "script automatic"]
        detach()
        command("save", name)
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        state = tomllib.loads(snapshot.read_text())
        assert [w["name"] for w in state["windows"]] == ["shell", ""], state
        command("kill", name)
        client = Session(extra_env=env, arguments=("attach", name, "--create"), lifetime=30)
        client.expect(b"RUSTMUX_READY>")
        set_title(client, "restored automatic")
        expect_bar(client, b"2 restored automatic")
        client.send(b"\x021")
        set_title(client, "restored fixed")
        client.send(b"printf 'RESTORED_TITLE_DONE\\n'\n")
        client.expect(b"RESTORED_TITLE_DONE")
        assert [p["window_name"] for p in panes()] == ["shell", "restored automatic"]

        # Explicit script creation also pins the name even if it is 'shell'.
        command("window", "new", "-s", name, "--name", "shell")
        client.expect(b"RUSTMUX_READY>")
        set_title(client, "explicit script title")
        client.send(b"printf 'SCRIPT_TITLE_DONE\\n'\n")
        client.expect(b"SCRIPT_TITLE_DONE")
        assert panes()[-1]["window_name"] == "shell"
        assert b"3 shell" in client.last_rows[0], client.last_rows
        detach()
    finally:
        if client:
            client.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

print("automatic tab titles, focus, background updates, explicit names and restore passed")

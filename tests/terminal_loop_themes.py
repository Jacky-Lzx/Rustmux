"""Interface colors reload atomically on real attached/detached servers and pickers."""
import os
from pathlib import Path
import subprocess
import tempfile
import time
import tomllib
from terminal_loop_support import BINARY, Session, expect_footer

with tempfile.TemporaryDirectory(prefix="rustmux-themes-") as temporary:
    root = Path(temporary)
    selected = root / "server.toml"
    client = root / "client.toml"
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"themes-{os.getpid()}"
    default_name = f"themes-default-{os.getpid()}"
    session = None

    def run(*args):
        result = subprocess.run([BINARY, *map(str, args)], env=env, capture_output=True,
                                text=True, timeout=8)
        assert result.returncode == 0, (args, result)
        return result.stdout

    def status(target=name):
        return tomllib.loads(run("show-config", "-s", target))

    def wait(predicate, target=name):
        deadline = time.monotonic() + 5
        while True:
            if session:
                session.read(0.01)
            current = status(target)
            if predicate(current):
                return current
            assert time.monotonic() < deadline, current
            time.sleep(0.01)

    def expect_raw(*fragments):
        deadline = time.monotonic() + 5
        while not all(fragment in session.output for fragment in fragments):
            session.read(0.02)
            assert time.monotonic() < deadline, (fragments, bytes(session.output[-3000:]))
        data = bytes(session.output)
        session.output.clear()
        session.frames.clear()
        return data

    def background(r, g, b):
        return f"48;2;{r};{g};{b}".encode()

    def foreground(r, g, b):
        return f"38;2;{r};{g};{b}".encode()

    def write(preset="light", overrides=""):
        selected.write_text(f"[theme]\npreset='{preset}'\n" + ("[theme.colors]\n" + overrides if overrides else ""))

    def detach():
        global session
        session.send(b"\x02d")
        session.finish(0)
        session.close()
        session = None

    try:
        write()
        client.write_text("[theme]\npreset='mocha'\n")
        run("new", name, "--detached", "--config", selected)
        panes = tomllib.loads(run("list-panes", "-s", name, "--toml"))["panes"]
        original = panes[0]
        run("send-keys", "-s", name, "--literal", "--enter",
            "stty -echo; KEEP=preserved; printf '\\033[31;44mTHEME_CHILD\\033[0m\\n'")
        session = Session(extra_env=env, arguments=("--config", str(client), "attach", name))
        raw = session.expect(b"THEME_CHILD")
        assert background(245, 246, 250) in raw  # Server owns attached interface colors.
        assert foreground(205, 0, 0) in raw and background(0, 0, 238) in raw  # Child ANSI stays independent.
        capture = run("capture-pane", "-s", name, "--history")
        write(overrides="background='#010203'\naccent='#040506'\nkey='#070809'\npurple='#0a0b0c'\n")
        changed = wait(lambda c: c["settings"]["theme"]["background"] == "#010203")
        expect_raw(background(1, 2, 3), foreground(4, 5, 6))
        assert run("capture-pane", "-s", name, "--history") == capture
        assert tomllib.loads(run("list-panes", "-s", name, "--toml"))["panes"][0]["pid"] == original["pid"]
        selected.write_text("remain_on_exit=true\n[theme.colors]\naccent='#bad'\n")
        failed = wait(lambda c: "error" in c)
        assert failed["generation"] == changed["generation"]
        assert failed["settings"] == changed["settings"]  # Reject the entire file.
        expect_raw(b"Config reload failed")
        write(overrides="background='#010203'\naccent='#040506'\nkey='#070809'\npurple='#0a0b0c'\n")
        wait(lambda c: "error" not in c)
        session.send(b"\x02?")
        expect_raw(b"Shortcut Help", background(1, 2, 3), foreground(7, 8, 9), foreground(10, 11, 12))
        write(overrides="background='#111213'\nkey='#141516'\n")
        pending = wait(lambda c: c["pending"])
        assert pending["settings"]["theme"]["background"] == "#010203"
        session.send(b"q")
        wait(lambda c: c["settings"]["theme"]["background"] == "#111213")
        expect_raw(background(17, 18, 19))
        session.send(b"\x02[")
        expect_footer(session, b"HISTORY")
        session.send(b"q")
        expect_footer(session, b"LOCKED")
        detach()
        write("mocha")
        wait(lambda c: c["settings"]["theme"]["background"] == "#1e1e2e")
        session = Session(extra_env=env, arguments=("--config", str(client), "attach", name))
        raw = session.expect(b"THEME_CHILD")
        assert background(30, 30, 46) in raw
        run("send-keys", "-s", name, "--literal", "--enter", "printf 'KEEP_%s\\n' \"$KEEP\"")
        session.expect(b"KEEP_preserved")
        detach()
        # A separate picker owns its client's theme and reloads even during search editing.
        client.write_text("[theme]\npreset='light'\n[theme.colors]\nbackground='#212223'\nborder='#242526'\nsurface_highlight='#272829'\n")
        session = Session(extra_env=env, arguments=("--config", str(client), "attach", name))
        session.expect(b"KEEP_preserved")
        session.send(b"\x02\x17")
        expect_raw(b"Session Manager", background(33, 34, 35), foreground(36, 37, 38), background(39, 40, 41))
        session.send(b"/themes")
        expect_raw(b"Search: themes_")
        client.write_text("[theme]\npreset='light'\n[theme.colors]\nbackground='#313233'\n")
        expect_raw(b"Search: themes_", background(49, 50, 51))
        client.write_text("[theme]\npreset='bad'\n")
        expect_raw(b"Config reload failed", background(49, 50, 51))
        session.send(b"\x1b")  # Clear search.
        expect_raw(b"Session Manager")
        session.send(b"\x1b")  # Leave picker.
        session.expect(b"KEEP_preserved")
        detach()
        assert status()["settings"]["theme"]["background"] == "#1e1e2e"
        # Deleting a discovered config resets theme defaults on the detached server.
        discovered = root / "rustmux/config.toml"
        discovered.parent.mkdir()
        discovered.write_text("[theme]\npreset='light'\n")
        run("new", default_name, "--detached")
        assert status(default_name)["settings"]["theme"]["background"] == "#f5f6fa"
        discovered.unlink()
        wait(lambda c: c["settings"]["theme"]["background"] == "#1e1e2e", default_name)
    finally:
        if session:
            session.close()
        for target in (name, default_name):
            subprocess.run([BINARY, "kill", target], env=env, capture_output=True, timeout=8)

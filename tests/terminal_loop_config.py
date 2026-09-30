"""Explicit config selection through foreground, named sessions and their manager."""
import os
import subprocess
import tempfile
import time
from terminal_loop_support import BINARY, Session, expect_bar, expect_footer


def config(prefix):
    return f'''[keybinds.locked]
"Ctrl {prefix}" = {{ actions = [{{ action = "switch-mode", mode = "normal" }}] }}
[keybinds.normal]
enter = {{ actions = [{{ action = "switch-mode", mode = "history" }}] }}
[keybinds.history]
q = {{ actions = [{{ action = "switch-mode", mode = "locked" }}] }}
'''


def check_config(session, prefix):
    session.send(prefix)
    expect_footer(session, b"NORMAL")
    session.send(b"\r")
    expect_footer(session, b"HISTORY")
    session.send(b"q")
    expect_footer(session, b"LOCKED")


def expect_manager_text(session, text):
    deadline = time.monotonic() + 3
    while text not in session.output:
        session.read()
        assert time.monotonic() < deadline, bytes(session.output[-2000:])


def create_in_manager(session, prefix, name):
    session.output.clear()
    session.send(prefix + b"\x17")
    expect_manager_text(session, b"Session Manager")
    session.send(b"a")
    expect_manager_text(session, b"New session:")
    session.output.clear()
    session.send(name.encode() + b"\r")
    session.expect(b"RUSTMUX_READY>")
    expect_bar(session, f"Rustmux ({name})".encode())


with tempfile.TemporaryDirectory(prefix="rustmux-config-") as directory:
    default_dir = os.path.join(directory, "default")
    os.makedirs(os.path.join(default_dir, "rustmux"))
    with open(os.path.join(default_dir, "rustmux", "config.toml"), "w") as file:
        file.write("this is invalid TOML")
    selected = os.path.join(directory, "dev config.toml")
    alternate = os.path.join(directory, "client config.toml")
    with open(selected, "w") as file:
        file.write(config("a"))
    with open(alternate, "w") as file:
        file.write(config("z"))
    env = {"XDG_CONFIG_HOME": default_dir, "EXPECTED_XDG_CONFIG_HOME": default_dir}

    for path in (os.path.join(directory, "missing.toml"),
                 os.path.join(default_dir, "rustmux", "config.toml")):
        result = subprocess.run([BINARY, "--config", path], capture_output=True, text=True)
        assert result.returncode == 1, result
        assert path in result.stderr, result.stderr
        assert "could not read" in result.stderr or "invalid" in result.stderr, result.stderr
    # Management commands do not depend on a readable configuration file.
    result = subprocess.run([BINARY, "list", "-c", os.path.join(directory, "missing.toml")],
                            capture_output=True, text=True)
    assert result.returncode == 0, result

    session = Session(arguments=("--config", selected), extra_env=env)
    try:
        session.expect(b"RUSTMUX_READY>")
        check_config(session, b"\x01")
        session.send(b"test \"$XDG_CONFIG_HOME\" = \"$EXPECTED_XDG_CONFIG_HOME\" && printf 'CONFIG_ENV_%s\\n' OK\n")
        session.expect(b"CONFIG_ENV_OK")
        session.send(b"exit 0\n")
        session.finish(0)
    finally:
        session.close()

    names = [f"config-{os.getpid()}-{suffix}" for suffix in ("original", "new", "attach")]
    session = None
    try:
        # A relative path with spaces also works after the subcommand.
        session = Session(arguments=("new", names[0], "--config", os.path.relpath(selected)),
                          extra_env=env)
        session.expect(b"RUSTMUX_READY>")
        check_config(session, b"\x01")
        create_in_manager(session, b"\x01", names[1])
        check_config(session, b"\x01")
        session.send(b"exit 0\n")
        session.finish(0)
        session.close()
        session = None

        # Attaching preserves the server's settings, but new sessions use the
        # attaching client's selection rather than the invalid default file.
        session = Session(arguments=("-c", alternate, "attach", names[0]), extra_env=env)
        session.expect(b"RUSTMUX_READY>")
        check_config(session, b"\x01")
        create_in_manager(session, b"\x01", names[2])
        check_config(session, b"\x1a")
        session.send(b"exit 0\n")
        session.finish(0)
    finally:
        if session is not None:
            session.close()
        for name in names:
            subprocess.run([BINARY, "kill", name], capture_output=True, timeout=5)

"""OS/machine overlays retain server identity through reloads and client attaches."""
import os
from pathlib import Path
import subprocess
import tempfile
import time
import tomllib
from terminal_loop_support import BINARY, Session

with tempfile.TemporaryDirectory(prefix="rustmux-overrides-") as temporary:
    root = Path(temporary)
    selected = root / "selected.toml"
    os_name = "macos" if os.uname().sysname == "Darwin" else "linux"
    name = f"overrides-{os.getpid()}"
    server_env = dict(os.environ, RUSTMUX_MACHINE="server", RUSTMUX_SHELL="/bin/sh",
                      SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="",
                      XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"))
    client_env = dict(server_env, RUSTMUX_MACHINE="client")
    session = None

    def write(machine=True, remove=False):
        selected.write_text(f'''remain_on_exit=true
scrollback_lines=100
[theme]
preset="light"
[overrides.os.{os_name}]
scrollback_lines=15
[overrides.os.{os_name}.theme.colors]
accent="#040506"
[overrides.machine.client]
scrollback_lines=99
''' + ('''[overrides.machine.server]
scrollback_lines=7
[overrides.machine.server.theme.colors]
background="#010203"
''' if machine else "") + ('''[overrides.machine.server.keybinds.normal]
c=[]
''' if remove else ""))

    def run(*args, env=client_env):
        result = subprocess.run([BINARY, *map(str, args)], cwd=root, env=env,
                                capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, result
        return result.stdout

    def status():
        return tomllib.loads(run("config", "show", "-s", name))

    def panes():
        return tomllib.loads(run("pane", "list", "-s", name, "--toml"))["panes"]

    def wait(predicate):
        end = time.monotonic() + 6
        while True:
            if session:
                session.read(0.01)
            current = status()
            if predicate(current):
                return current
            assert time.monotonic() < end, current
            time.sleep(0.03)

    write()
    try:
        run("new", name, "-d", "--config", selected, env=server_env)
        initial = status()
        assert initial["selection"]["environment"]["machine"] == "server", initial
        assert initial["settings"]["scrollback_lines"] == 7, initial
        assert initial["settings"]["theme"]["background"] == "#010203", initial
        first = panes()[0]
        run("pane", "send-keys", "-s", name, "-p", first["id"], "--literal", "--enter",
            "export OVERLAY_PRESERVED=yes; echo $$ > retained-pid")
        client_check = tomllib.loads(run("config", "check", "--config", selected, "--toml", "--strict"))
        assert client_check["settings"]["scrollback_lines"] == 99, client_check

        session = Session(extra_env=client_env, arguments=("attach", name), lifetime=25)
        session.expect(b"RUSTMUX_READY>")
        assert status()["selection"]["environment"]["machine"] == "server"
        write(remove=True)
        current = wait(lambda c: "new_window_key" not in c)
        assert current["settings"]["scrollback_lines"] == 7
        session.send(b"\x02c")
        session.read(0.15)
        assert len(panes()) == 1
        session.send(b"\x02")  # Return to LOCKED using the literal-prefix operation.
        session.read(0.05)

        selected.write_text("[overrides.machine.server]\nscrollback_lines='invalid'\n")
        current = wait(lambda c: "error" in c)
        assert current["settings"]["scrollback_lines"] == 7, current
        assert current["settings"]["theme"]["background"] == "#010203", current
        write(machine=False)
        current = wait(lambda c: "error" not in c and c["settings"]["scrollback_lines"] == 15)
        assert current["selection"]["sources"]["scrollback_lines"] == f"overrides.os.{os_name}", current
        assert current["settings"]["theme"]["background"] == "#f5f6fa", current
        assert current["settings"]["theme"]["accent"] == "#040506", current
        assert panes()[0]["pid"] == first["pid"]
        run("pane", "send-keys", "-s", name, "-p", first["id"], "--literal", "--enter",
            'printf "%s" "$OVERLAY_PRESERVED" > retained-variable')
        end = time.monotonic() + 3
        while not (root / "retained-variable").exists():
            session.read(0.01)
            assert time.monotonic() < end
        assert (root / "retained-variable").read_text() == "yes"
        session.send(b"\x02d")
        session.finish(0)
        session.close()
        session = None
        write()
        wait(lambda c: c["settings"]["scrollback_lines"] == 7)
        session = Session(extra_env=client_env, arguments=("attach", name), lifetime=12)
        session.expect(b"RUSTMUX_READY>")
        assert status()["selection"]["environment"]["machine"] == "server"
        assert panes()[0]["pid"] == first["pid"]
        session.send(b"\x02d")
        session.finish(0)
        session.close()
        session = None
    finally:
        if session:
            session.close()
        subprocess.run([BINARY, "kill", name], env=server_env, capture_output=True, timeout=8)

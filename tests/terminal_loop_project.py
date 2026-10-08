"""Project validation, detached startup and explicit-command snapshot restoration."""
import os
from pathlib import Path
import subprocess
import tempfile
import time
import tomllib
from terminal_loop_support import BINARY

with tempfile.TemporaryDirectory(prefix="rustmux-project-") as root:
    root = Path(root)
    (root / "src").mkdir()
    env = dict(os.environ, XDG_STATE_HOME=str(root / "state"), XDG_CONFIG_HOME=str(root / "config"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"project-{os.getpid()}"
    layout = root / "project.toml"
    runs = root / "src" / "runs"
    snapshot = root / "state" / "rustmux" / "main-human" / "sessions" / f"{name}.toml"

    def run(*args, success=True):
        result = subprocess.run([BINARY, *map(str, args)], env=env, capture_output=True,
                                text=True, timeout=10)
        assert (result.returncode == 0) == success, (args, result)
        return result

    def panes():
        return tomllib.loads(run("pane", "list", "-s", name, "--toml").stdout)["panes"]

    def wait_runs(count):
        deadline = time.monotonic() + 6
        while not runs.exists() or len(runs.read_text().splitlines()) != count:
            assert time.monotonic() < deadline, (count, runs.read_text() if runs.exists() else None)
            time.sleep(0.02)

    # Neither a late invalid cwd nor impossible final geometry launches early commands.
    prefix = "[[windows]]\nname='dev'\n[[windows.panes]]\ncwd='src'\ncommand=\"printf 'run\\n' >> runs; exec /bin/sh -i\"\n"
    for suffix in ["[[windows.panes]]\ncwd='missing'\n", "[[windows.panes]]\n" * 10,
                   "[[windows.panes]]\nunknown=true\n"]:
        layout.write_text(prefix + suffix)
        run("new", name, "--detached", "--layout", layout, success=False)
        assert not runs.exists()
        assert not Path(f"/tmp/rustmux-{os.geteuid()}/{name}.sock").exists()

    layout.write_text(prefix + "[[windows.panes]]\ncwd='src'\nsplit='down'\n[[windows]]\nname='logs'\n[[windows.panes]]\n")
    try:
        run("new", name, "--detached", "--layout", layout)
        wait_runs(1)
        entries = panes()
        assert len(entries) == 3
        assert [p["window_name"] for p in entries] == ["dev", "dev", "logs"]
        assert entries[0]["directory"] == str((root / "src").resolve())
        assert entries[0]["active"] and not entries[1]["active"]
        run("new", name, "--detached", "--layout", layout, success=False)
        assert len(runs.read_text().splitlines()) == 1, "layout commands ran in an existing session"
        run("save-session", name)
        saved = tomllib.loads(snapshot.read_text())
        assert saved["windows"][0]["panes"][0]["command"].startswith("printf")
        assert "command" not in saved["windows"][0]["panes"][1]
        run("kill", name)
        # Restore works without the source project file and reruns only recorded commands.
        layout.unlink()
        run("new", name, "--detached")
        wait_runs(2)
        assert len(panes()) == 3
        run("kill", name)
        # A new explicit layout overrides even a corrupt saved snapshot.
        snapshot.write_text("version = 999\n")
        layout.write_text(prefix)
        run("new", name, "--detached", "--layout", layout)
        wait_runs(3)
        assert len(panes()) == 1
    finally:
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

print("project validation, detached commands, relative cwd and snapshot replay passed")

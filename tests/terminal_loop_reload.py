"""Configuration changes apply on the live server, without restarting existing PTYs."""
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import tomllib
from terminal_loop_support import BINARY, Session, expect_footer

with tempfile.TemporaryDirectory(prefix="rustmux-reload-") as temporary:
    root = Path(temporary)
    selected = root / "selected.toml"
    shell = root / "new-shell.sh"
    shell.write_text("#!/bin/sh\nexport RELOADED=YES\nexport PS1='RUSTMUX_READY> '\nprintf 'RELOADED_SHELL\\n'\nexec /bin/sh -i\n")
    shell.chmod(0o700)
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="", SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"reload-{os.getpid()}"
    session = None

    def write(key="c", lines=1000, executable="/bin/sh", bell=False, autosave=0, save=False, retain=True):
        selected.write_text(f'''shell="{executable}"
remain_on_exit={str(retain).lower()}
scrollback_lines={lines}
autosave_interval_seconds={autosave}
save_scrollback={str(save).lower()}
save_scrollback_colors={str(save).lower()}
[notifications]
long_command_bell={str(bell).lower()}
command_duration_seconds=1
[shortcuts]
new_window="{key}"
''')

    def run(action, *args, success=True):
        target = [name] if action in ("new", "kill", "save-session") else ["-s", name]
        result = subprocess.run([BINARY, action, *target, *map(str, args)], env=env,
                                cwd=root, capture_output=True, text=True, timeout=8)
        assert (result.returncode == 0) == success, (action, args, result)
        return result

    def status():
        return tomllib.loads(run("show-config").stdout)

    def panes():
        return tomllib.loads(run("list-panes", "--toml").stdout)["panes"]

    def wait(predicate):
        deadline = time.monotonic() + 5
        while True:
            if session:
                session.read(seconds=0.01)
            current = status()
            if predicate(current):
                return current
            assert time.monotonic() < deadline, current
            time.sleep(0.01)

    def send(pane, text):
        run("send-keys", "-p", pane, "--literal", "--enter", text)

    def capture(pane):
        if session:
            session.read(seconds=0.01)
        return run("capture-pane", "-p", pane, "--history").stdout

    def wait_capture(pane, text):
        deadline = time.monotonic() + 5
        while text not in capture(pane):
            assert time.monotonic() < deadline, capture(pane)
            time.sleep(0.01)

    def drain_for(seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            session.read(min(0.05, deadline - time.monotonic()))

    try:
        write()
        # Relative paths are anchored at startup and remain the server's source.
        run("new", "--detached", "--config", "selected.toml")
        original = panes()[0]
        first = original["id"]
        assert Path(status()["path"]).resolve() == selected.resolve()
        send(first, "stty -echo; KEEP=preserved; i=0; while [ $i -lt 100 ]; do printf 'OLD_%s\\n' $i; i=$((i+1)); done")
        wait_capture(first, "OLD_99")
        write("N", 0, shell)
        changed = wait(lambda c: c.get("new_window_key") == "N" and c["settings"]["scrollback_lines"] == 0)
        assert changed["generation"] > 0 and "error" not in changed
        assert panes()[0]["pid"] == original["pid"]
        assert "OLD_0" in capture(first)  # Existing history capacity is preserved.
        second = int(run("new-window").stdout)
        wait_capture(second, "RELOADED_SHELL")
        send(second, "stty -echo; i=0; while [ $i -lt 100 ]; do printf 'NEW_%s\\n' $i; i=$((i+1)); done")
        wait_capture(second, "NEW_99")
        assert "NEW_0\n" not in capture(second)  # New pane's history is disabled.
        send(first, "printf 'VALUE_%s\\n' \"$KEEP\"")
        wait_capture(first, "VALUE_preserved")
        selected.write_text("scrollback_lines='invalid'")
        failed = wait(lambda c: "error" in c)
        assert failed["generation"] == changed["generation"]
        assert failed["settings"] == changed["settings"]
        write("N", 0, shell)
        wait(lambda c: "error" not in c)
        # Entering NORMAL defers a whole update until the old binding completes.
        session = Session(extra_env=env, arguments=("--config", str(root / "unused.toml"), "attach", name))
        session.expect(b"RUSTMUX_READY>")
        session.send(b"\x02")
        expect_footer(session, b"NORMAL")
        write("P", 0, shell)
        pending = wait(lambda c: c["pending"])
        assert pending["new_window_key"] == "N"
        before = len(panes())
        session.send(b"N")
        wait(lambda c: c["new_window_key"] == "P")
        assert len(panes()) == before + 1
        session.send(b"\x02P")
        deadline = time.monotonic() + 5
        while len(panes()) != before + 2:
            session.read(seconds=0.01)
            assert time.monotonic() < deadline
        active = next(p["id"] for p in panes() if p["active"])
        paste_file = root / "paste.bin"
        send(active, f"stty -echo -icanon min 1 time 0; cat > '{paste_file}'")
        time.sleep(0.1)
        session.send(b"\x1b[200~HELLO\x02Q")
        write("Q", 0, shell)
        wait(lambda c: c["pending"])
        assert status()["new_window_key"] == "P"
        session.send(b"\x1b[201~")
        wait(lambda c: c["new_window_key"] == "Q")
        payload = b"\x1b[200~HELLO\x02Q\x1b[201~"
        deadline = time.monotonic() + 5
        while not paste_file.exists() or paste_file.read_bytes() != payload:
            session.read(seconds=0.01)
            assert time.monotonic() < deadline, paste_file.read_bytes() if paste_file.exists() else None
        assert len(panes()) == before + 2
        session.send(b"\x02d")
        session.finish(0)
        session.close()
        session = None
        # Invalid entry-policy changes reject other settings in the same file.
        accepted = status()
        write("V", 20, shell)
        selected.write_text(selected.read_text().replace("remain_on_exit=true", "remain_on_exit=false") +
                            '\n[keybinds.locked]\n"Ctrl a"={actions=[{action="switch-mode",mode="normal"}]}\n')
        refused = wait(lambda c: "restart" in c.get("error", ""))
        assert refused["settings"] == accepted["settings"]
        assert refused["new_window_key"] == "Q"
        selected.unlink()
        wait(lambda c: "could not read" in c.get("error", ""))
        assert status()["settings"] == accepted["settings"]
        # Updated persistence options affect subsequent manual and automatic saves.
        write("V", 20, shell, autosave=1, save=True)
        wait(lambda c: c["settings"]["autosave_interval_seconds"] == 1 and "error" not in c)
        snapshot = root / "state" / "rustmux" / "main-human" / "sessions" / f"{name}.toml"
        deadline = time.monotonic() + 5
        while not snapshot.exists():
            assert time.monotonic() < deadline
            time.sleep(0.02)
        saved = tomllib.loads(snapshot.read_text())
        assert any(p.get("history") for w in saved["windows"] for p in w["panes"])
        write("V", 20, shell, save=False)
        wait(lambda c: not c["settings"]["save_scrollback"])
        run("save-session")
        saved = tomllib.loads(snapshot.read_text())
        assert all(not p.get("history") for w in saved["windows"] for p in w["panes"])
        # Respawn uses the updated shell, and disabling retention also removes
        # already drained panes whose policy comes from the session default.
        retained = int(run("new-window").stdout)
        retained_pid = next(p["pid"] for p in panes() if p["id"] == retained)
        send(retained, "exit 7")
        wait(lambda _: any(p["id"] == retained and p.get("exit_code") == 7 and p["output_complete"] for p in panes()))
        run("respawn-pane", "-p", retained)
        wait_capture(retained, "RELOADED_SHELL")
        assert next(p["pid"] for p in panes() if p["id"] == retained) != retained_pid
        send(retained, "exit 8")
        wait(lambda _: any(p["id"] == retained and p.get("exit_code") == 8 and p["output_complete"] for p in panes()))
        write("V", 20, shell, retain=False)
        wait(lambda c: not c["settings"]["remain_on_exit"] and all(p["id"] != retained for p in panes()))
        write("V", 20, shell)
        wait(lambda c: c["settings"]["remain_on_exit"])
        # Reconnect adopts settings applied while detached; notification changes
        # update the existing pane rather than just future pane construction.
        active = int(run("new-window").stdout)
        session = Session(extra_env=env, arguments=("attach", name))
        session.expect(b"RUSTMUX_READY>")
        send(active, "stty -echo; printf 'NOTIFY_%s\\n' READY")
        session.expect(b"NOTIFY_READY")
        session.output.clear()
        probe = "printf '\\033]133;C\\033\\\\'; sleep 1.2; printf '\\033]133;D;0\\033\\\\DONE_PROBE\\n'"
        send(active, probe)
        notification_output = session.expect(b"DONE_PROBE")
        session.read(0.2)
        assert b"\x07" not in notification_output + session.output
        write("V", 20, shell, bell=True)
        wait(lambda c: c["settings"]["long_command_bell"])
        send(active, "printf '\\033[2J\\033[H'")
        session.read(0.15)
        session.output.clear()
        send(active, probe.replace("DONE_PROBE", "ENABLED_PROBE"))
        notification_output = session.expect(b"ENABLED_PROBE")
        session.read(0.2)
        assert b"\x07" in notification_output + session.output, notification_output + session.output
        session.send(b"\x02d")
        session.finish(0)
        session.close()
        session = None
        assert next(p for p in panes() if p["id"] == first)["pid"] == original["pid"]
    finally:
        if session:
            session.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

    # Default-source deletion resets built-ins in a foreground session too.
    default = root / "rustmux" / "config.toml"
    default.parent.mkdir()
    default.write_text('shell="/bin/sh"\nremain_on_exit=true\n[shortcuts]\nnew_window="N"\n')
    session = Session(extra_env=env)
    try:
        session.expect(b"RUSTMUX_READY>")
        default.write_text(f'shell="{shell}"\nremain_on_exit=true\n[shortcuts]\nnew_window="V"\n')
        drain_for(1.1)
        session.send(b"\x02V")
        session.expect(b"RELOADED_SHELL")
        default.unlink()
        drain_for(1.1)
        session.send(b"\x02c")
        session.expect(b"RUSTMUX_READY>")
        session.send(b"printf 'DEFAULT_%s\\n' \"${RELOADED-no}\"\n")
        session.expect(b"DEFAULT_no")
        os.kill(session.app_pid, signal.SIGTERM)
        session.finish(128 + signal.SIGTERM)
    finally:
        session.close()

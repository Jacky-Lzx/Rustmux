"""Raw output clients remain independent of the UI and report bounded-tail loss."""
import base64
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import tomllib
from terminal_loop_support import BINARY, Session

with tempfile.TemporaryDirectory(prefix="rustmux-output-") as temporary:
    root = Path(temporary)
    config = root / "rustmux" / "config.toml"
    config.parent.mkdir()
    config.write_text("remain_on_exit = true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"output-{os.getpid()}"
    clients = []
    session = None

    def run(action, *args, success=True):
        target = [name] if action in ("new", "kill") else ["-s", name]
        result = subprocess.run([BINARY, *action.split(), *target, *map(str, args)],
                                env=env, capture_output=True, timeout=8)
        assert (result.returncode == 0) == success, (action, args, result)
        return result

    def panes():
        return tomllib.loads(run("pane list", "--toml").stdout.decode())["panes"]

    def read(pane, after=None):
        args = ("--after", after) if after is not None else ()
        return tomllib.loads(run("pane read-output", "-p", pane, *args).stdout.decode())

    def send(pane, command):
        run("pane send-keys", "-p", pane, "--literal", "--enter", command)

    def wait(predicate):
        deadline = time.monotonic() + 5
        while not predicate():
            if session:
                session.read(seconds=0.01)
            assert time.monotonic() < deadline
            time.sleep(0.01)

    def client(action, *args, stdout=subprocess.PIPE):
        process = subprocess.Popen([BINARY, *action.split(), "-s", name, *map(str, args)],
                                   env=env, stdout=stdout, stderr=subprocess.PIPE)
        clients.append(process)
        return process

    try:
        run("new", "--detached")
        first = panes()[0]["id"]
        send(first, "PS1=; stty -echo; printf 'READY_RAW\\n'")
        wait(lambda: b"READY_RAW\r\n" in base64.b64decode(read(first, 0)["bytes_base64"]))
        cursor = read(first)["next"]
        log = root / "pane.raw"
        logger = client("pane log", "-p", first, "--after", cursor, "--output", log)
        subscriber = client("pane subscribe", "--after", cursor)  # Omitted ID pins focus.
        wait(log.exists)
        time.sleep(0.15)
        second = int(run("window new", "--name", "other").stdout)
        send(second, "printf 'OTHER_PANE\\n'")
        run("pane break", "-p", first, "--name", "moved")
        send(first, "printf '\\033[31mRAW_BYTES\\377\\033[0m\\n'; printf '\\344'; sleep 0.1; printf '\\270\\255\\n'")
        raw = b"\x1b[31mRAW_BYTES\xff\x1b[0m\r\n\xe4\xb8\xad\r\n"
        wait(lambda: raw in log.read_bytes())
        assert log.stat().st_mode & 0o777 == 0o600
        assert read(first, cursor)["pane"] == first
        run("pane log", "-p", first, "--output", log, success=False)
        symlink = root / "symlink"
        symlink.symlink_to(log)
        run("pane log", "-p", first, "--output", symlink, success=False)
        send(first, "exec /usr/bin/printf FINAL_RAW")
        logger_output, logger_error = logger.communicate(timeout=5)
        output, error = subscriber.communicate(timeout=5)
        assert logger.returncode == subscriber.returncode == 0, (logger_error, error)
        raw += b"FINAL_RAW"
        assert log.read_bytes() == output == raw, (log.read_bytes(), output)
        completed = read(first, cursor)
        assert completed["complete"] and completed["dropped"] == 0
        assert base64.b64decode(completed["bytes_base64"]) == raw
        run("pane read-output", "-p", first, "--after", completed["next"] + 1, success=False)
        # Respawn clears old bytes but advances generation without reusing cursors.
        run("pane respawn", "-p", first, "--command", "printf RESTARTED; exit 0")
        wait(lambda: read(first)["complete"])
        restarted = read(first, completed["next"])
        assert restarted["generation"] == completed["generation"] + 1
        assert base64.b64decode(restarted["bytes_base64"]) == b"RESTARTED"
        # A stopped reader cannot block the server. On resuming, loss is explicit.
        cursor = read(second)["next"]
        slow = client("pane subscribe", "-p", second, "--after", cursor)
        time.sleep(0.15)
        slow.send_signal(signal.SIGSTOP)
        send(second, "stty -echo; head -c 150000 /dev/zero; printf BIG_DONE")
        wait(lambda: read(second, cursor)["dropped"] > 0)
        assert len(base64.b64decode(read(second, cursor)["bytes_base64"])) <= 65536
        slow.send_signal(signal.SIGCONT)
        _, error = slow.communicate(timeout=5)
        assert slow.returncode != 0 and b"lost" in error, error
        # An attached frontend remains usable alongside output clients.
        session = Session(extra_env=env, arguments=("attach", name))
        session.read(seconds=0.2)
        # First is dead, select the other via a new window and capture on that pane.
        live = int(run("window new").stdout)
        ui_log = root / "ui.raw"
        ui_logger = client("pane log", "-p", live, "--output", ui_log)
        wait(ui_log.exists)
        send(live, "printf 'UI_STILL_ALIVE\\n'")
        session.expect(b"UI_STILL_ALIVE")
        session.send(b"\x02d")
        session.finish(0)
        session.close()
        session = None
        send(live, "printf 'DETACHED_LOG_ALIVE\\n'")
        wait(lambda: b"DETACHED_LOG_ALIVE\r\n" in ui_log.read_bytes())
        send(live, "exec /usr/bin/printf LOG_END")
        _, error = ui_logger.communicate(timeout=5)
        assert ui_logger.returncode == 0, error
        assert b"UI_STILL_ALIVE\r\n" in ui_log.read_bytes()
        assert ui_log.read_bytes().endswith(b"LOG_END")
        # Retention is a checked prerequisite, including per-pane false overrides.
        interrupted = client("pane subscribe", "-p", second)
        time.sleep(0.15)
        run("kill")
        _, error = interrupted.communicate(timeout=5)
        assert interrupted.returncode != 0, error
        config.write_text("remain_on_exit = false\n")
        run("new", "--detached")
        ordinary = panes()[0]["id"]
        read(ordinary)
        run("pane subscribe", "-p", ordinary, success=False)
        rejected = root / "rejected.raw"
        run("pane log", "-p", ordinary, "--output", rejected, success=False)
        assert not rejected.exists()
        run("kill")
        config.write_text("remain_on_exit = true\n")
        layout = root / "no-retain.toml"
        layout.write_text("[[windows]]\nname='ordinary'\n[[windows.panes]]\nremain_on_exit=false\n")
        run("new", "--detached", "--layout", layout)
        run("pane subscribe", "-p", panes()[0]["id"], success=False)
    finally:
        if session:
            session.close()
        for process in clients:
            if process.poll() is None:
                process.send_signal(signal.SIGCONT)
                process.kill()
            process.communicate(timeout=5)
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

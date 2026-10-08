"""Script selection updates display/input/zoom while retaining process identity."""
import os
from pathlib import Path
import shlex
import socket
import struct
import subprocess
import tempfile
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_bar, expect_footer


with tempfile.TemporaryDirectory(prefix="rustmux-focus-control-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("remain_on_exit=true\nsave_scrollback=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"focus-control-{os.getpid()}"
    report_name = f"focus-report-{os.getpid()}"
    runtime = Path(f"/tmp/rustmux-{os.geteuid()}")
    client = None

    def run(action, *args, success=True, session_name=None):
        result = subprocess.run([BINARY, *action.split(), "-s", session_name or name, *map(str, args)],
                                env=env, capture_output=True, text=True, timeout=8)
        assert (result.returncode == 0) == success, (action, args, result)
        return result

    def panes(session_name=None):
        return tomllib.loads(run("pane list", "--toml", session_name=session_name).stdout)["panes"]

    def active(session_name=None):
        return next(p["id"] for p in panes(session_name) if p["active"])

    def wait_until(predicate, detail):
        deadline = time.monotonic() + 6
        while not predicate():
            if client:
                client.read(0.01)
            assert time.monotonic() < deadline, detail
            time.sleep(0.01)

    def input_to(pane, command):
        run("pane send-keys", "-p", pane, "--literal", "--enter", command)

    def wait_text(pane, text):
        wait_until(lambda: text in run("pane capture", "-p", pane, "--history").stdout, text)

    def identities():
        return {p["id"]: p["pid"] for p in panes()}

    def detach():
        global client
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None

    def wire_request(body):
        with socket.socket(socket.AF_UNIX) as wire:
            wire.settimeout(3)
            wire.connect(str(runtime / f"{name}.control"))
            wire.sendall(struct.pack("!I", len(body)) + body)
            response = bytearray()
            while len(response) < 4:
                chunk = wire.recv(4096)
                assert chunk, response
                response.extend(chunk)
            length = struct.unpack("!I", response[:4])[0]
            while len(response) < length + 4:
                chunk = wire.recv(4096)
                assert chunk, response
                response.extend(chunk)
            return tomllib.loads(response[4:].decode())

    try:
        client = Session(extra_env=env, arguments=("new", name))
        client.expect(b"RUSTMUX_READY>")
        first = active()
        input_to(first, "stty -echo; KEEP=first; printf 'FIRST_READY\\n'")
        wait_text(first, "FIRST_READY")
        right = int(run("pane split", "-p", first).stdout)
        input_to(right, "stty -echo; KEEP=right; printf 'RIGHT_READY\\n'")
        wait_text(right, "RIGHT_READY")
        logs = int(run("window new", "--name", "logs").stdout)
        input_to(logs, "stty -echo; KEEP=logs; printf 'LOGS_READY\\n'")
        wait_text(logs, "LOGS_READY")
        original = identities()
        server_pid = (runtime / f"{name}.pid").read_bytes()

        # A pane ID finds its window, selects its leaf, redraws and routes the
        # real terminal's subsequent input to that same unchanged shell.
        assert run("pane select", "-p", first).stdout == ""
        assert active() == first
        client.expect(b"FIRST_READY")
        client.send(b"printf 'TYPED:%s\\n' $KEEP\n")
        client.expect(b"TYPED:first")
        assert "TYPED:first" not in run("pane capture", "-p", logs).stdout
        run("pane select", "-p", logs)
        assert active() == logs
        run("window select", "-w", 1)
        assert active() == first, "window selection discarded remembered pane focus"
        # Re-selecting the same window preserves last-window bookkeeping.
        run("window select", "-w", 1)
        client.send(b"\x02\t")
        wait_until(lambda: active() == logs, "last-window shortcut did not retain the prior window")
        run("pane select", "-p", first)
        client.send(b"\x02>")
        wait_until(lambda: next(p["window"] for p in panes() if p["id"] == first) == 2,
                   "window reordering did not complete")
        run("window select", "-w", 1)
        assert active() == logs, "window number was interpreted as a stable window ID"
        assert identities() == original
        detach()

        # Selecting a hidden pane transfers zoom and synchronizes both child
        # PTYs. Invalid commands leave history overlays and focus unchanged.
        client = Session(extra_env=env, arguments=("attach", name))
        client.expect(b"LOGS_READY")
        run("pane select", "-p", first)
        client.expect(b"FIRST_READY")
        client.send(b"\x02Z")
        # Interactive input and script requests use independent sockets. Wait
        # for the layout frame before asking a child to observe its new size.
        wait_until(lambda: b"RIGHT_READY" not in b"".join(client.last_rows),
                   "zoom frame did not hide the sibling")
        input_to(first, "printf 'ZOOM_FIRST:%s\\n' \"$(stty size)\"")
        wait_text(first, "ZOOM_FIRST:20 78")
        run("pane select", "-p", right)
        input_to(right, "printf 'ZOOM_RIGHT:%s KEEP:%s\\n' \"$(stty size)\" $KEEP")
        wait_text(right, "ZOOM_RIGHT:20 78 KEEP:right")
        input_to(first, "printf 'HIDDEN_FIRST:%s\\n' \"$(stty size)\"")
        wait_text(first, "HIDDEN_FIRST:20 38")
        client.send(b"\x02Z")
        wait_until(lambda: b"FIRST_READY" in b"".join(client.last_rows) and
                   b"RIGHT_READY" in b"".join(client.last_rows),
                   "unzoom frame did not restore both panes")
        input_to(right, "printf 'TILED_RIGHT:%s\\n' \"$(stty size)\"")
        wait_text(right, "TILED_RIGHT:20 38")
        run("pane select", "-p", first)
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        assert "unknown runtime pane ID" in run("pane select", "-p", 999999, success=False).stderr
        assert "unknown window number" in run("window select", "-w", 17, success=False).stderr
        # Server validation also rejects zero when CLI validation is bypassed.
        rejected = wire_request(b"action='select-window'\nwindow=0\n")
        assert not rejected["ok"] and "unknown window number" in rejected["output"], rejected
        assert panes() == before
        # Entering search proves the overlay still consumes input; a cached old
        # footer alone would not catch a failed request accidentally dismissing it.
        client.send(b"/focus-check")
        expect_footer(client, b"Search /focus-check")
        run("pane select", "-p", logs)
        client.expect(b"LOGS_READY")
        expect_footer(client, b"LOCKED")
        # Clear the old prefix mode on a successful script selection.
        client.send(b"\x02")
        expect_footer(client, b"NORMAL")
        run("window select", "-w", 2)
        expect_footer(client, b"LOCKED")
        client.send(b"printf 'AFTER_MODE:%s\\n' $KEEP\n")
        client.expect(b"AFTER_MODE:first")
        assert len(panes()) == 3 and identities() == original
        assert (runtime / f"{name}.pid").read_bytes() == server_pid
        detach()

        # Detached selection applies to the next attachment and manual snapshot.
        run("pane select", "-p", right)
        assert active() == right
        saved = subprocess.run([BINARY, "save", name], env=env, capture_output=True, timeout=8)
        assert saved.returncode == 0, saved
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        state = tomllib.loads(snapshot.read_text())
        assert state["active_window"] == 1 and state["windows"][1]["layout"]["active"] == 1, state
        client = Session(extra_env=env, arguments=("attach", name))
        client.expect(b"RIGHT_READY")
        assert active() == right and identities() == original
        client.send(b"printf 'DETACHED_FOCUS:%s\\n' $KEEP\n")
        client.expect(b"DETACHED_FOCUS:right")
        # Exited retained panes remain valid focus targets without respawning.
        ended = int(run("window new", "--name", "ended").stdout)
        input_to(ended, "exit 7")
        wait_until(lambda: next(p for p in panes() if p["id"] == ended)["output_complete"],
                   "retained pane did not exit")
        ended_pid = identities()[ended]
        run("pane select", "-p", first)
        run("pane select", "-p", ended)
        assert active() == ended and identities()[ended] == ended_pid
        wait_until(lambda: b"exited 7" in client.last_frame, "retained pane status did not redraw")
        run("window select", "-w", 2)
        assert active() == first
        detach()

        # Raw applications receive blur/focus through the ordinary event path;
        # modes follow the selected screen, and selecting the same pane emits no
        # duplicate focus event. Probe input is recorded separately per process.
        client = Session(extra_env=env, arguments=("new", report_name))
        client.expect(b"RUSTMUX_READY>")
        probe_a = active(report_name)
        probe = """import os,sys,tty
tty.setraw(0)
label,path,application=sys.argv[1:]
os.write(1,b'\\x1b[?1004h'+(b'\\x1b[?1h' if application=='yes' else b'\\x1b[?1l')+b'\\x1b[2J\\x1b[H'+label.encode())
with open(path,'wb',buffering=0) as log:
    while True:
        data=os.read(0,1024)
        if not data: break
        log.write(data)
"""

        def launch_probe(pane, label, application):
            path = root / (label + ".events")
            command = "exec python3 -u -c " + shlex.quote(probe) + " " + label + " " + shlex.quote(str(path)) + " " + application
            run("pane send-keys", "-p", pane, "--literal", "--enter", command, session_name=report_name)
            client.expect(label.encode())
            wait_until(path.exists, "probe event file was not created")
            return path

        events_a = launch_probe(probe_a, "PROBE_A", "yes")
        probe_b = int(run("window new", "--name", "probe", session_name=report_name).stdout)
        events_b = launch_probe(probe_b, "PROBE_B", "no")
        wait_until(lambda: b"\x1b[O" in events_a.read_bytes(), "initial blur missing")
        baseline_a, baseline_b = events_a.read_bytes(), events_b.read_bytes()
        run("pane select", "-p", probe_a, session_name=report_name)
        client.expect(b"PROBE_A")
        wait_until(lambda: events_a.read_bytes() == baseline_a + b"\x1b[I" and
                   events_b.read_bytes() == baseline_b + b"\x1b[O", "script focus events missing")
        assert client.private_modes.get(1), "application cursor mode did not follow pane selection"
        run("pane select", "-p", probe_a, session_name=report_name)
        client.send(b"x")
        wait_until(lambda: events_a.read_bytes() == baseline_a + b"\x1b[Ix", "same-pane selection emitted a duplicate focus event")
        run("window select", "-w", 2, session_name=report_name)
        client.expect(b"PROBE_B")
        wait_until(lambda: events_a.read_bytes() == baseline_a + b"\x1b[Ix\x1b[O" and
                   events_b.read_bytes() == baseline_b + b"\x1b[O\x1b[I", "window focus events missing")
        assert not client.private_modes.get(1), "normal cursor mode did not follow window selection"
        detach()
    finally:
        if client:
            client.close()
        for target in (name, report_name):
            subprocess.run([BINARY, "kill", target], env=env, capture_output=True, timeout=8)
            (runtime / f"{target}.workspace").unlink(missing_ok=True)

print("script pane/window focus, input routing, zoom, overlays, detached state and focus reporting passed")

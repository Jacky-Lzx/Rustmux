"""Script zoom changes real PTYs and focus without replacing pane processes."""
import json
import os
from pathlib import Path
import shlex
import socket
import struct
import subprocess
import tempfile
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_footer


with tempfile.TemporaryDirectory(prefix="rustmux-pane-zoom-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("remain_on_exit=true\nsave_scrollback=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"pane-zoom-{os.getpid()}"
    runtime = Path(f"/tmp/rustmux-{os.geteuid()}")
    client = None
    probe = root / "probe.py"
    probe.write_text("""import json,os,select,sys,tty
tty.setraw(0)
path,label,application=sys.argv[1:]
os.write(1,b'\\x1b[?1004h'+(b'\\x1b[?1h' if application=='yes' else b'\\x1b[?1l')+b'\\x1b[2J\\x1b[H'+label.encode())
events=b''
previous=None
while True:
    if select.select([0],[],[],0.01)[0]:
        data=os.read(0,1024)
        if not data: break
        events+=data
    size=os.get_terminal_size(0)
    state={'pid':os.getpid(),'rows':size.lines,'columns':size.columns,'input':events.hex()}
    if state!=previous:
        with open(path+'.tmp','w') as log: json.dump(state,log)
        os.replace(path+'.tmp',path)
        previous=state
""")

    def run(action, *arguments, success=True):
        result = subprocess.run([BINARY, action, "-s", name, *map(str, arguments)],
                                env=env, capture_output=True, text=True, timeout=8)
        assert (result.returncode == 0) == success, (action, arguments, result)
        return result

    def panes():
        return tomllib.loads(run("list-panes", "--toml").stdout)["panes"]

    def active():
        return next(p["id"] for p in panes() if p["active"])

    def record(label):
        path = root / f"{label}.json"
        return json.loads(path.read_text()) if path.exists() else None

    def wait_until(predicate, detail):
        deadline = time.monotonic() + 5
        while not predicate():
            if client:
                client.read(0.01)
            assert time.monotonic() < deadline, detail
            time.sleep(0.01)

    def command(label, application="yes"):
        return "exec python3 -u " + shlex.quote(str(probe)) + " " + shlex.quote(str(root / f"{label}.json")) \
            + " " + label.upper() + "_ZOOM_READY " + application

    def start(action, label, *arguments, application="yes"):
        pane = int(run(action, *arguments, "--command", command(label, application)).stdout)
        wait_until(lambda: record(label) is not None, f"probe {label} did not start")
        assert record(label)["pid"] == next(p["pid"] for p in panes() if p["id"] == pane)
        return pane

    def size(label, rows, columns):
        wait_until(lambda: (record(label)["rows"], record(label)["columns"]) == (rows, columns),
                   ("PTY size mismatch", label, rows, columns))

    def zoom(pane=None, state=None):
        arguments = ["-p", str(pane)] if pane is not None else []
        if state is not None:
            arguments.append("--on" if state else "--off")
        assert run("zoom-pane", *arguments).stdout == ""

    def wire(body):
        with socket.socket(socket.AF_UNIX) as peer:
            peer.settimeout(3)
            peer.connect(str(runtime / f"{name}.control"))
            peer.sendall(struct.pack("!I", len(body)) + body)
            response = bytearray()
            while len(response) < 4:
                chunk = peer.recv(4096)
                assert chunk, response
                response.extend(chunk)
            length = struct.unpack("!I", response[:4])[0]
            while len(response) < length + 4:
                chunk = peer.recv(4096)
                assert chunk, response
                response.extend(chunk)
            return tomllib.loads(response[4:].decode())

    def detach():
        global client
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None

    try:
        layout = root / "startup.toml"
        layout.write_text("[[windows]]\nname='shell'\n[[windows.panes]]\ncommand="
                          + json.dumps(command("first")) + "\n")
        client = Session(extra_env=env, arguments=("new", name, "--layout", str(layout)))
        client.expect(b"FIRST_ZOOM_READY")
        first = active()
        wait_until(lambda: record("first") is not None, "first probe did not start")
        right = start("split-pane", "right", "-p", first, application="no")
        bottom = start("split-pane", "bottom", "-p", right, "--down")
        solo = start("new-window", "solo", "--name", "solo", application="no")
        client.expect(b"SOLO_ZOOM_READY")
        size("first", 20, 38)
        size("right", 9, 38)
        size("bottom", 9, 38)
        size("solo", 20, 78)
        baseline_solo = record("solo")
        for state in (True, True, False, False, None, None):
            zoom(state=state)  # one pane already fills its window
            assert record("solo") == baseline_solo
        wait_until(lambda: record("first")["input"].endswith(b"\x1b[O".hex()),
                   "setup blur did not arrive")
        baseline_first = record("first")["input"]
        ids = {p["id"]: p["pid"] for p in panes()}
        server_pid = (runtime / f"{name}.pid").read_bytes()
        zoom(first, True)  # cross-window target selects the pane and its window
        assert active() == first
        size("first", 20, 78)
        size("right", 9, 38)
        size("bottom", 9, 38)
        client.expect(b"FIRST_ZOOM_READY")
        wait_until(lambda: record("first")["input"] == baseline_first + b"\x1b[I".hex()
                   and record("solo")["input"] == baseline_solo["input"] + b"\x1b[O".hex(),
                   "cross-window focus events missing or duplicated")
        assert client.private_modes.get(1), "application cursor mode did not follow zoom target"
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before_input = record("first")["input"]
        zoom(first, True)  # repeated explicit state still refreshes overlays
        expect_footer(client, b"LOCKED")
        size("first", 20, 78)
        assert record("first")["input"] == before_input
        client.send(b"\x02")
        expect_footer(client, b"NORMAL")
        zoom(state=True)
        expect_footer(client, b"LOCKED")
        assert record("first")["input"] == before_input
        before_right = record("right")["input"]
        zoom(right, True)  # already zoomed: transfer full view to hidden sibling
        assert active() == right
        size("right", 20, 78)
        size("first", 20, 38)
        size("bottom", 9, 38)
        client.expect(b"RIGHT_ZOOM_READY")
        wait_until(lambda: record("first")["input"] == before_input + b"\x1b[O".hex()
                   and record("right")["input"] == before_right + b"\x1b[I".hex(),
                   "in-window focus events missing or duplicated")
        assert not client.private_modes.get(1), "normal cursor mode did not follow zoom target"
        client.send(b"x")
        wait_until(lambda: record("right")["input"].endswith(b"x".hex()), "input reached wrong pane")
        assert record("first")["input"] == before_input + b"\x1b[O".hex()
        assert {p["id"]: p["pid"] for p in panes()} == ids
        assert run("close-pane", "-p", solo).stdout == ""
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        before_input = record("right")["input"]
        for invalid in (solo, 999999):
            assert "unknown runtime pane ID" in run("zoom-pane", "-p", invalid, "--on", success=False).stderr
        run("zoom-pane", "--on", "--off", success=False)
        for body in (b"action='zoom-pane'\npane='invalid'\n",
                     b"action='zoom-pane'\nzoom=1\n",
                     f"action='zoom-pane'\npane={first}\nzoom=true\nextra=true\n".encode()):
            assert not wire(body)["ok"]
        assert panes() == before
        client.send(b"/zoom-check")
        expect_footer(client, b"Search /zoom-check")
        zoom(state=False)
        expect_footer(client, b"LOCKED")
        size("right", 9, 38)
        size("first", 20, 38)
        assert active() == right and record("right")["input"] == before_input
        wait_until(lambda: all(marker in b"".join(client.last_rows)
                               for marker in (b"FIRST_ZOOM_READY", b"RIGHT_ZOOM_READY")),
                   "unzoom did not display both tiled panes")
        zoom(state=False)
        assert record("right")["input"] == before_input
        zoom()  # omitted state toggles rather than acting like --off
        size("right", 20, 78)
        zoom()
        size("right", 9, 38)
        assert record("right")["input"] == before_input
        assert (runtime / f"{name}.pid").read_bytes() == server_pid
        detach()

        # Detached controls preserve process identities and record exact view state.
        other = start("new-window", "other", "--name", "other")
        other_bottom = start("split-pane", "other-bottom", "-p", other, "--down")
        zoom(bottom, True)
        size("bottom", 20, 78)
        zoom(other_bottom, False)  # --off also selects a cross-window target
        assert active() == other_bottom
        size("other-bottom", 9, 78)
        zoom(bottom, True)
        assert active() == bottom
        survivors = {p["id"]: p["pid"] for p in panes()}
        ended = int(run("new-window", "--name", "ended", "--command", "printf RETAINED_ZOOM; exit 7").stdout)
        wait_until(lambda: any(p["id"] == ended and p["output_complete"] for p in panes()),
                   "retained pane did not exit")
        ended_pid = next(p["pid"] for p in panes() if p["id"] == ended)
        start("split-pane", "ended-live", "-p", ended)
        for state in (True, True, False):
            zoom(ended, state)
            entry = next(p for p in panes() if p["id"] == ended)
            assert active() == ended and entry["pid"] == ended_pid and entry["exit_code"] == 7
            assert entry["exited"] and entry["output_complete"], "zoom respawned retained child"
        run("close-window")
        zoom(bottom, True)
        assert {p["id"]: p["pid"] for p in panes()} == survivors
        subprocess.run([BINARY, "save", name], env=env, capture_output=True, check=True, timeout=8)
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        saved = tomllib.loads(snapshot.read_text())
        assert saved["active_window"] == 0 and len(saved["windows"]) == 2
        assert saved["windows"][0]["layout"]["zoomed"]
        assert saved["windows"][0]["layout"]["active"] == 2
        assert not saved["windows"][1]["layout"]["zoomed"]
        labels = ("first", "right", "bottom", "other", "other-bottom")
        old = {label: record(label)["pid"] for label in labels}
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, check=True, timeout=8)
        client = Session(extra_env=env, arguments=("attach", name, "--create"))
        client.expect(b"BOTTOM_ZOOM_READY")
        wait_until(lambda: all(record(label)["pid"] != pid for label, pid in old.items()),
                   "saved startup jobs did not restart")
        assert len(panes()) == 5
        assert next(p["pid"] for p in panes() if p["active"]) == record("bottom")["pid"]
        size("bottom", 20, 78)
        size("first", 20, 38)
        size("right", 9, 38)
        size("other-bottom", 9, 78)
        detach()
    finally:
        if client:
            client.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

print("script pane zoom: explicit/toggle state, PTY geometry, modes/focus, retained jobs and saved view passed")

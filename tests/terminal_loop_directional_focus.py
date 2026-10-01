"""Directional script selection uses tiled geometry and normal focus transitions."""
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


with tempfile.TemporaryDirectory(prefix="rustmux-directional-focus-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("remain_on_exit=true\nsave_scrollback=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"directional-focus-{os.getpid()}"
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
            + " " + label.upper() + "_FOCUS_READY " + application

    def start(action, label, *arguments, application="yes"):
        pane = int(run(action, *arguments, "--command", command(label, application)).stdout)
        wait_until(lambda: record(label) is not None, f"probe {label} did not start")
        assert record(label)["pid"] == next(p["pid"] for p in panes() if p["id"] == pane)
        if client:
            client.expect(label.upper().encode() + b"_FOCUS_READY")
        return pane

    def size(label, rows, columns):
        wait_until(lambda: (record(label)["rows"], record(label)["columns"]) == (rows, columns),
                   ("PTY size mismatch", label, rows, columns))

    def focus(direction, origin=None, success=True):
        arguments = ["-p", str(origin)] if origin is not None else []
        result = run("select-pane", *arguments, "--direction", direction, success=success)
        if success:
            assert result.stdout == ""
        return result

    labels = {}

    def step(direction, destination, origin=None):
        previous = labels[active()]
        before = {label: record(label)["input"] for label in labels.values()}
        focus(direction, origin)
        expected = dict(before)
        if previous != destination:
            expected[previous] += b"\x1b[O".hex()
            expected[destination] += b"\x1b[I".hex()
        wait_until(lambda: all(record(label)["input"] == data for label, data in expected.items()),
                   ("incorrect focus reports", previous, destination, expected))
        assert labels[active()] == destination
        wait_until(lambda: bool(client.private_modes.get(1)) == (destination in ("first", "bottom")),
                   ("cursor mode did not follow selection", destination))
        return expected

    def identities():
        return {p["id"]: p["pid"] for p in panes()}

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
        layout.write_text("[[windows]]\nname='main'\n[[windows.panes]]\ncommand="
                          + json.dumps(command("first")) + "\n")
        client = Session(extra_env=env, arguments=("new", name, "--layout", str(layout)))
        client.expect(b"FIRST_FOCUS_READY")
        first = active()
        wait_until(lambda: record("first") is not None, "first probe did not start")
        top = start("split-pane", "top", "-p", first, application="no")
        bottom = start("split-pane", "bottom", "-p", top, "--down")
        other = start("new-window", "other", "--name", "other", application="no")
        labels.update({first: "first", top: "top", bottom: "bottom", other: "other"})
        size("first", 20, 38)
        size("top", 9, 38)
        size("bottom", 9, 38)
        wait_until(lambda: record("bottom")["input"].endswith(b"\x1b[O".hex()),
                   "setup focus-out did not arrive")
        original = identities()
        server_pid = (runtime / f"{name}.pid").read_bytes()
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        assert "no pane in that direction" in focus("left", success=False).stderr
        assert "no pane in that direction" in focus("down", bottom, success=False).stderr
        assert panes() == before, "failed inactive origin changed remembered/global focus"
        client.send(b"/single-focus-check")
        expect_footer(client, b"Search /single-focus-check")
        before_events = step("right", "top", first)  # geometric neighbor in inactive window
        expect_footer(client, b"LOCKED")
        assert identities() == original
        client.send(b"x")
        wait_until(lambda: record("top")["input"] == before_events["top"] + b"x".hex(),
                   "physical input did not reach the directional target")
        assert record("first")["input"] == before_events["first"], "origin incorrectly received input/focus"
        step("down", "bottom")
        step("up", "top")
        step("left", "first")
        step("right", "top")
        size("first", 20, 38)
        size("top", 9, 38)
        size("bottom", 9, 38)
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before_events = step("right", "top", first)  # already selected neighbor: no duplicate reports
        expect_footer(client, b"LOCKED")
        client.send(b"y")
        wait_until(lambda: record("top")["input"] == before_events["top"] + b"y".hex(),
                   "repeated neighbor selection emitted duplicate focus")
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        before_records = {label: record(label) for label in labels.values()}
        assert "no pane in that direction" in focus("left", first, success=False).stderr
        assert "no pane in that direction" in focus("down", bottom, success=False).stderr
        assert "unknown runtime pane ID" in focus("left", 99999, success=False).stderr
        for body in (b"action='select-pane-direction'\n",
                     b"action='select-pane-direction'\ndirection='next'\n",
                     b"action='select-pane-direction'\npane='invalid'\ndirection='left'\n",
                     f"action='select-pane-direction'\npane={first}\ndirection='right'\nextra=true\n".encode()):
            assert not wire(body)["ok"]
        assert panes() == before and all(record(label) == old for label, old in before_records.items())
        client.send(b"/edge-focus-check")
        expect_footer(client, b"Search /edge-focus-check")
        step("left", "first")
        expect_footer(client, b"LOCKED")
        run("zoom-pane", "--on")
        size("first", 20, 78)
        step("right", "top")
        size("top", 20, 78)
        size("first", 20, 38)
        step("down", "bottom")
        size("bottom", 20, 78)
        size("top", 9, 38)
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        assert "no pane in that direction" in focus("right", success=False).stderr
        assert panes() == before
        client.send(b"/zoom-focus-check")
        expect_footer(client, b"Search /zoom-focus-check")
        step("right", "top", first)  # hidden explicit origin uses its tiled rectangle
        expect_footer(client, b"LOCKED")
        size("top", 20, 78)
        size("bottom", 9, 38)
        client.send(b"\x02")
        expect_footer(client, b"NORMAL")
        step("down", "bottom")
        expect_footer(client, b"LOCKED")
        assert identities() == original
        assert (runtime / f"{name}.pid").read_bytes() == server_pid
        detach()

        # Detached changes preserve the client lease and apply sizes before reattachment.
        run("select-pane", "-p", other)
        focus("right", first)
        assert active() == top
        size("top", 20, 78)
        size("bottom", 9, 38)
        ended = int(run("new-window", "--name", "retained", "--command", "printf RETAINED_FOCUS; exit 7").stdout)
        wait_until(lambda: any(p["id"] == ended and p["output_complete"] for p in panes()),
                   "retained job did not exit")
        ended_pid = identities()[ended]
        live = start("split-pane", "live", "-p", ended, application="no")
        focus("left")
        entry = next(p for p in panes() if p["id"] == ended)
        assert active() == ended and entry["pid"] == ended_pid and entry["exit_code"] == 7
        assert entry["exited"] and entry["output_complete"]
        assert "RETAINED_FOCUS" in run("capture-pane", "-p", ended).stdout
        run("close-pane", "-p", live)
        before = panes()
        assert "unknown runtime pane ID" in focus("left", live, success=False).stderr
        assert panes() == before
        focus("down", top)
        assert active() == bottom
        size("bottom", 20, 78)
        subprocess.run([BINARY, "save", name], env=env, capture_output=True, check=True, timeout=8)
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        saved = tomllib.loads(snapshot.read_text())
        assert saved["active_window"] == 0 and len(saved["windows"]) == 3
        assert saved["windows"][0]["layout"]["active"] == 2 and saved["windows"][0]["layout"]["zoomed"]
        old = {label: record(label)["pid"] for label in labels.values()}
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, check=True, timeout=8)
        client = Session(extra_env=env, arguments=("attach", name, "--create"))
        client.expect(b"BOTTOM_FOCUS_READY")
        wait_until(lambda: all(record(label)["pid"] != pid for label, pid in old.items()),
                   "saved startup jobs did not restart")
        assert len(panes()) == 5
        assert next(p["pid"] for p in panes() if p["active"]) == record("bottom")["pid"]
        size("bottom", 20, 78)
        size("first", 20, 38)
        size("top", 9, 38)
        detach()
    finally:
        if client:
            client.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

print("directional focus: geometric origins, focus/modes/input, zoom, failure isolation, detached and restored selection passed")

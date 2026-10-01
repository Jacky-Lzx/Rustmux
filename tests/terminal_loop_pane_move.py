"""Directional script moves use geometric neighbors without moving focus."""
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


with tempfile.TemporaryDirectory(prefix="rustmux-pane-move-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("remain_on_exit=true\nsave_scrollback=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"pane-move-{os.getpid()}"
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
            + " " + label.upper() + "_MOVE_READY " + application

    editor = root / "editor.sh"
    editor.write_text("#!/bin/sh\n" + command("editor") + "\n")
    editor.chmod(0o700)
    env.update(VISUAL="", EDITOR=str(editor))

    def start(action, label, *arguments, application="yes"):
        pane = int(run(action, *arguments, "--command", command(label, application)).stdout)
        wait_until(lambda: record(label) is not None, f"probe {label} did not start")
        assert record(label)["pid"] == next(p["pid"] for p in panes() if p["id"] == pane)
        if client:
            client.expect(label.upper().encode() + b"_MOVE_READY")
        return pane

    def size(label, rows, columns):
        wait_until(lambda: (record(label)["rows"], record(label)["columns"]) == (rows, columns),
                   ("PTY size mismatch", label, rows, columns))

    def move(direction, source=None, success=True):
        arguments = ["-p", str(source)] if source is not None else []
        result = run("move-pane", *arguments, "--direction", direction, success=success)
        if success:
            assert result.stdout == ""
        return result

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
        client.expect(b"FIRST_MOVE_READY")
        first = active()
        wait_until(lambda: record("first") is not None, "first probe did not start")
        top = start("split-pane", "top", "-p", first, application="no")
        bottom = start("split-pane", "bottom", "-p", top, "--down")
        other = start("new-window", "other", "--name", "other", application="no")
        size("first", 20, 38)
        size("top", 9, 38)
        size("bottom", 9, 38)
        wait_until(lambda: record("bottom")["input"].endswith(b"\x1b[O".hex()),
                   "setup focus-out did not arrive")
        original = identities()
        server_pid = (runtime / f"{name}.pid").read_bytes()
        before_inputs = {label: record(label)["input"] for label in ("first", "top", "bottom", "other")}
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        assert "no pane in that direction" in move("left", success=False).stderr
        client.send(b"/single-move-check")
        expect_footer(client, b"Search /single-move-check")
        move("right", first)  # overlapping right neighbors: the aligned upper slot wins
        expect_footer(client, b"LOCKED")
        size("first", 9, 38)
        size("top", 20, 38)
        size("bottom", 9, 38)
        assert active() == other and identities() == original
        assert next(p for p in panes() if p["id"] == bottom)["selected"]
        assert all(record(label)["input"] == old for label, old in before_inputs.items()), "inactive move emitted focus"
        move("left", first)
        size("first", 20, 38)
        size("top", 9, 38)
        run("select-pane", "-p", bottom)
        client.expect(b"BOTTOM_MOVE_READY")
        wait_until(lambda: record("bottom")["input"].endswith(b"\x1b[I".hex()),
                   "selected pane did not receive focus-in")
        before_inputs = {label: record(label)["input"] for label in ("first", "top", "bottom")}
        move("left")  # active bottom exchanges with the large left slot
        size("bottom", 20, 38)
        size("first", 9, 38)
        assert active() == bottom and identities() == original
        assert all(record(label)["input"] == old for label, old in before_inputs.items()), "active move emitted focus"
        assert client.private_modes.get(1), "move changed active application cursor mode"
        client.send(b"x")
        wait_until(lambda: record("bottom")["input"] == before_inputs["bottom"] + b"x".hex(),
                   "input did not follow active identity after move")
        assert record("first")["input"] == before_inputs["first"]
        move("down", top)
        move("up", top)  # vertical inverse; processes keep their contents
        for pane, label in ((first, "first"), (top, "top"), (bottom, "bottom")):
            assert label.upper() + "_MOVE_READY" in run("capture-pane", "-p", pane).stdout
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        before_records = {label: record(label) for label in ("first", "top", "bottom")}
        assert "no pane in that direction" in move("left", success=False).stderr
        assert "no pane in that direction" in move("right", first, success=False).stderr
        assert "unknown runtime pane ID" in move("left", 99999, success=False).stderr
        for body in (b"action='move-pane'\n", b"action='move-pane'\ndirection='next'\n",
                     b"action='move-pane'\npane='invalid'\ndirection='left'\n",
                     f"action='move-pane'\npane={first}\ndirection='left'\nextra=true\n".encode()):
            assert not wire(body)["ok"]
        assert panes() == before and all(record(label) == old for label, old in before_records.items())
        client.send(b"/edge-move-check")
        expect_footer(client, b"Search /edge-move-check")
        move("left", first)  # inactive source exchanges with active bottom, preserving that focus
        expect_footer(client, b"LOCKED")
        size("first", 20, 38)
        size("bottom", 9, 38)
        assert active() == bottom
        assert all(record(label)["input"] == old["input"] for label, old in before_records.items())
        run("zoom-pane", "--on")
        size("bottom", 20, 78)
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        for source in (None, first):
            assert "zoomed pane layout" in move("left", source, success=False).stderr
        assert panes() == before and (record("bottom")["rows"], record("bottom")["columns"]) == (20, 78)
        client.send(b"/zoom-move-check")
        expect_footer(client, b"Search /zoom-move-check")
        run("zoom-pane", "--off")
        expect_footer(client, b"LOCKED")
        size("bottom", 9, 38)
        client.send(b"\x02")
        expect_footer(client, b"NORMAL")
        move("up")
        expect_footer(client, b"LOCKED")
        assert active() == bottom and identities() == original
        assert (runtime / f"{name}.pid").read_bytes() == server_pid
        detach()

        run("close-pane", "-p", other)
        before = panes()
        assert "unknown runtime pane ID" in move("left", other, success=False).stderr
        assert panes() == before
        move("right", first)  # detached: active neighbor bottom changes slots without focus change
        size("bottom", 20, 38)
        size("first", 9, 38)
        assert active() == bottom
        main_ids = identities()
        ended = int(run("new-window", "--name", "retained", "--command", "printf RETAINED_MOVE; exit 7").stdout)
        wait_until(lambda: any(p["id"] == ended and p["output_complete"] for p in panes()),
                   "retained job did not exit")
        ended_pid = identities()[ended]
        live = start("split-pane", "live", "-p", ended)
        lower = start("split-pane", "lower", "-p", live, "--down")
        ids = identities()
        move("right", ended)
        move("down", ended)
        size("live", 20, 38)
        size("lower", 9, 38)
        entry = next(p for p in panes() if p["id"] == ended)
        assert active() == lower and entry["pid"] == ended_pid and entry["exit_code"] == 7
        assert entry["exited"] and entry["output_complete"]
        assert "RETAINED_MOVE" in run("capture-pane", "-p", ended).stdout
        assert identities() == ids and all(identities()[pane] == pid for pane, pid in main_ids.items())
        run("select-pane", "-p", bottom)
        subprocess.run([BINARY, "save", name], env=env, capture_output=True, check=True, timeout=8)
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        saved = tomllib.loads(snapshot.read_text())
        assert saved["active_window"] == 0 and len(saved["windows"]) == 2
        assert all(w["layout"]["active"] == 2 and not w["layout"]["zoomed"] for w in saved["windows"])
        labels = ("first", "top", "bottom", "live", "lower")
        old = {label: record(label)["pid"] for label in labels}
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, check=True, timeout=8)
        client = Session(extra_env=env, arguments=("attach", name, "--create"))
        client.expect(b"BOTTOM_MOVE_READY")
        wait_until(lambda: all(record(label)["pid"] != pid for label, pid in old.items()),
                   "saved startup jobs did not restart")
        assert len(panes()) == 6
        assert next(p["pid"] for p in panes() if p["active"]) == record("bottom")["pid"]
        size("bottom", 20, 38)
        size("first", 9, 38)
        size("top", 9, 38)
        size("live", 20, 38)
        size("lower", 9, 38)
        # Interactive splits can create a normal neighbor beside a temporary editor.
        client.send(b"\x02E")
        client.expect(b"EDITOR_MOVE_READY")
        editor_id = active()
        wait_until(lambda: record("editor") is not None, "editor probe did not start")
        assert "temporary editor panes" in move("right", success=False).stderr
        client.send(b"\x02%")
        client.expect(b"RUSTMUX_READY>")
        normal = active()
        assert normal != editor_id
        size("editor", 20, 38)
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        for direction, source in (("right", editor_id), ("left", normal)):
            assert "temporary editor panes" in move(direction, source, success=False).stderr
        assert panes() == before
        client.send(b"/editor-move-check")
        expect_footer(client, b"Search /editor-move-check")
        run("close-pane", "-p", normal)
        run("close-pane", "-p", editor_id)
        detach()
    finally:
        if client:
            client.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

print("script pane move: geometry, ownership/focus, edge/zoom failure isolation, detached and saved positions passed")

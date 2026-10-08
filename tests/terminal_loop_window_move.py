"""Script window reordering preserves actual children, focus, layouts and history."""
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


with tempfile.TemporaryDirectory(prefix="rustmux-window-move-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("remain_on_exit=true\nsave_scrollback=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"window-move-{os.getpid()}"
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
        result = subprocess.run([BINARY, *action.split(), "-s", name, *map(str, arguments)],
                                env=env, capture_output=True, text=True, timeout=8)
        assert (result.returncode == 0) == success, (action, arguments, result)
        return result

    def panes():
        return tomllib.loads(run("pane list", "--toml").stdout)["panes"]

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

    def start(action, label, *arguments, application="yes"):
        pane = int(run(action, *arguments, "--command", command(label, application)).stdout)
        wait_until(lambda: record(label) is not None, f"probe {label} did not start")
        assert record(label)["pid"] == next(p["pid"] for p in panes() if p["id"] == pane)
        if client:
            # The child records its state before the server necessarily parses
            # its focus-reporting sequence. Wait for that actual screen first.
            client.expect(label.upper().encode() + b"_MOVE_READY")
        return pane

    def size(label, rows, columns):
        wait_until(lambda: (record(label)["rows"], record(label)["columns"]) == (rows, columns),
                   ("PTY size mismatch", label, rows, columns))

    def move(direction, window=None):
        arguments = ["-w", str(window)] if window is not None else []
        assert run("window move", *arguments, "--direction", direction).stdout == ""

    def order(expected):
        current = panes()
        for number, anchor in enumerate(expected, 1):
            assert next(p["window"] for p in current if p["id"] == anchor) == number, (expected, current)
        assert {p["window"] for p in current} == set(range(1, len(expected) + 1))

    def without_positions():
        return {p["id"]: {k: v for k, v in p.items() if k != "window"} for p in panes()}

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
        layout.write_text("[[windows]]\nname='first'\n[[windows.panes]]\ncommand="
                          + json.dumps(command("first")) + "\n")
        client = Session(extra_env=env, arguments=("new", name, "--layout", str(layout)))
        client.expect(b"FIRST_MOVE_READY")
        first = active()
        wait_until(lambda: record("first") is not None, "first probe did not start")
        baseline = record("first")
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        move("left")  # one window: successful no-op still refreshes overlays
        expect_footer(client, b"LOCKED")
        for direction in ("left", "right"):
            move(direction, 1)
            order([first])
            assert record("first") == baseline
        first_right = start("pane split", "first-right", "-p", first)
        run("pane zoom", "--on")
        size("first-right", 20, 78)
        second = start("window new", "second", "--name", "second", application="no")
        second_bottom = start("pane split", "second-bottom", "-p", second, "--down", application="no")
        run("pane zoom", "--on")
        size("second-bottom", 20, 78)
        # Duplicate labels are metadata; numeric targeting follows display order.
        third = int(run("window new", "--name", "first", "--command", "printf RETAINED_MOVE; exit 7").stdout)
        wait_until(lambda: any(p["id"] == third and p["output_complete"] for p in panes()),
                   "retained job did not exit")
        third_live = start("pane split", "third", "-p", third)
        run("pane select", "-p", second_bottom)
        client.expect(b"SECOND-BOTTOM_MOVE_READY")
        wait_until(lambda: record("second-bottom")["input"].endswith(b"\x1b[I".hex())
                   and record("third")["input"].endswith(b"\x1b[O".hex()),
                   "setup focus events did not arrive")
        labels = ("first", "first-right", "second", "second-bottom", "third")
        baseline = {label: record(label) for label in labels}
        contents = without_positions()
        server_pid = (runtime / f"{name}.pid").read_bytes()
        order([first, second, third])
        # Inactive and active moves cross both edges and retain the selected pane.
        for direction, target, expected in [
            ("left", 1, [second, third, first]),
            ("right", 2, [second, first, third]),
            ("right", 3, [third, second, first]),
            ("left", 1, [second, first, third]),
            ("left", None, [first, third, second]),
            ("right", None, [second, first, third]),
            ("right", None, [first, second, third]),
            ("left", None, [second, first, third]),
        ]:
            move(direction, target)
            order(expected)
            assert active() == second_bottom and without_positions() == contents
            assert all(record(label) == old for label, old in baseline.items()), "reordering changed PTY state or focus"
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        assert "unknown window number" in run("window move", "-w", "65535", "--direction", "left", success=False).stderr
        for arguments in (("--direction", "up"), (), ("-w", "0", "--direction", "right")):
            run("window move", *arguments, success=False)
        for body in (b"action='move-window'\nwindow=0\ndirection='left'\n",
                     b"action='move-window'\nwindow='invalid'\ndirection='left'\n",
                     b"action='move-window'\nwindow=1\ndirection='up'\n",
                     b"action='move-window'\nwindow=1\n",
                     b"action='move-window'\nwindow=1\ndirection='right'\nextra=true\n"):
            assert not wire(body)["ok"]
        assert panes() == before
        client.send(b"/move-window-check")
        expect_footer(client, b"Search /move-window-check")
        move("left", 2)  # accepted inactive move dismisses History
        expect_footer(client, b"LOCKED")
        order([first, second, third])
        client.send(b"\x02")
        expect_footer(client, b"NORMAL")
        move("left")
        expect_footer(client, b"LOCKED")
        order([second, first, third])
        assert all(record(label) == old for label, old in baseline.items())
        assert not client.private_modes.get(1), "reordering changed active application cursor mode"
        client.send(b"x")
        wait_until(lambda: record("second-bottom")["input"] == baseline["second-bottom"]["input"] + b"x".hex(),
                   "physical input did not reach the unchanged active pane")
        assert all(record(label) == baseline[label] for label in labels if label != "second-bottom")
        # Remembered windows follow identity, not their new numeric position.
        client.send(b"\x02\t")
        wait_until(lambda: active() == third_live, "last-window history was lost by reordering")
        client.expect(b"THIRD_MOVE_READY")
        assert client.private_modes.get(1)
        client.send(b"\x02\t")
        wait_until(lambda: active() == second_bottom, "last-window return selected the wrong pane")
        client.expect(b"SECOND-BOTTOM_MOVE_READY")
        wait_until(lambda: record("second-bottom")["input"].endswith(b"x\x1b[O\x1b[I".hex()),
                   "last-window return focus did not settle")
        assert not client.private_modes.get(1)
        assert (runtime / f"{name}.pid").read_bytes() == server_pid
        detach()

        before = without_positions()
        baseline = {label: record(label) for label in labels}
        move("right", 3)  # detached inactive edge wrap
        order([third, second, first])
        move("right")  # detached active adjacent move
        order([third, first, second])
        assert active() == second_bottom and without_positions() == before
        assert all(record(label) == old for label, old in baseline.items())
        subprocess.run([BINARY, "save", name], env=env, capture_output=True, check=True, timeout=8)
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        saved = tomllib.loads(snapshot.read_text())
        assert saved["active_window"] == 2
        assert [w["name"] for w in saved["windows"]] == ["first", "first", "second"]
        assert [w["layout"]["zoomed"] for w in saved["windows"]] == [False, True, True]
        assert all(w["layout"]["active"] == 1 and len(w["panes"]) == 2 for w in saved["windows"])
        old = {label: record(label)["pid"] for label in labels}
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, check=True, timeout=8)
        client = Session(extra_env=env, arguments=("attach", name, "--create"))
        client.expect(b"SECOND-BOTTOM_MOVE_READY")
        wait_until(lambda: all(record(label)["pid"] != pid for label, pid in old.items()),
                   "saved startup processes did not restart")
        current = panes()
        assert len(current) == 6
        selected = next(p for p in current if p["active"])
        assert selected["window"] == 3 and selected["pid"] == record("second-bottom")["pid"]
        size("first-right", 20, 78)
        size("second-bottom", 20, 78)
        size("third", 20, 38)
        detach()
    finally:
        if client:
            client.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

print("script window move: active/inactive wrapping, stable children/focus/history and saved order passed")

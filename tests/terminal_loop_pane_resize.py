"""Targeted geometry changes reach real PTYs without changing focus or ownership."""
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


with tempfile.TemporaryDirectory(prefix="rustmux-pane-resize-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("save_scrollback=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"pane-resize-{os.getpid()}"
    runtime = Path(f"/tmp/rustmux-{os.geteuid()}")
    client = None
    probe = root / "probe.py"
    probe.write_text("""import json,os,select,sys,tty
tty.setraw(0)
os.write(1,b'\\x1b[?1004h\\x1b[?1h\\x1b[2J\\x1b[HRESIZE_PROBE_READY')
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
        with open(sys.argv[1]+'.tmp','w') as log: json.dump(state,log)
        os.replace(sys.argv[1]+'.tmp',sys.argv[1])
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

    def wait_until(predicate, detail):
        deadline = time.monotonic() + 5
        while not predicate():
            if client:
                client.read(0.01)
            assert time.monotonic() < deadline, detail
            time.sleep(0.01)

    def record(label):
        path = root / f"{label}.json"
        return json.loads(path.read_text()) if path.exists() else None

    def start_probe(pane, label):
        run("send-keys", "-p", pane, "--literal", "--enter",
            "exec python3 -u " + shlex.quote(str(probe)) + " " + shlex.quote(str(root / f"{label}.json")))
        wait_until(lambda: record(label) is not None, f"probe {label} did not start")
        # Publishing the child log does not prove the server has parsed DECSET
        # 1004. Await its following marker before setup can change focus.
        wait_until(lambda: "RESIZE_PROBE_READY" in run("capture-pane", "-p", pane).stdout,
                   f"server did not parse probe {label}'s focus-reporting setup")

    def sizes(expected):
        def matches():
            return all(record(label) and (record(label)["rows"], record(label)["columns"]) == size
                       for label, size in expected.items())
        wait_until(matches, ("PTY size mismatch", expected,
                             {label: record(label) for label in expected}))

    def resize(direction, cells=None, pane=None, success=True):
        arguments = ["--direction", direction]
        if cells is not None:
            arguments += ["--cells", str(cells)]
        if pane is not None:
            arguments += ["-p", str(pane)]
        result = run("resize-pane", *arguments, success=success)
        if success:
            assert result.stdout == ""
        return result

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
        client = Session(extra_env=env, arguments=("new", name))
        client.expect(b"RUSTMUX_READY>")
        left = active()
        start_probe(left, "left")
        upper = int(run("split-pane", "-p", left).stdout)
        start_probe(upper, "upper")
        lower = int(run("split-pane", "-p", upper, "--down").stdout)
        start_probe(lower, "lower")
        logs = int(run("new-window", "--name", "logs").stdout)
        start_probe(logs, "logs")
        sizes({"left": (20, 38), "upper": (9, 38), "lower": (9, 38), "logs": (20, 78)})
        # Child state and server capture can precede the frontend mode update.
        # Establish the outer-terminal baseline before checking resize keeps it.
        wait_until(lambda: client.private_modes.get(1),
                   "frontend did not enable application cursor mode")
        before = panes()
        # Wait for all intentional setup focus events before recording a baseline.
        wait_until(lambda: bytes.fromhex(record("lower")["input"]).endswith(b"\x1b[O"),
                   "inactive window did not receive focus-out")
        baseline = {label: record(label) for label in ("left", "upper", "lower", "logs")}
        resize("right", 3, lower)
        resize("down", 2, lower)
        sizes({"left": (20, 41), "upper": (11, 35), "lower": (7, 35), "logs": (20, 78)})
        assert panes() == before, "inactive resize changed focus, order or process metadata"
        client.send(b"x")
        wait_until(lambda: record("logs")["input"] == baseline["logs"]["input"] + b"x".hex(),
                   "physical input was not routed to active logs pane")
        for label in ("left", "upper", "lower"):
            assert record(label)["input"] == baseline[label]["input"], "resize emitted a focus event"
        assert all(record(label)["pid"] == baseline[label]["pid"] for label in baseline)
        assert client.private_modes.get(1), "resize changed application cursor mode"
        run("select-window", "-w", 1)
        assert active() == lower, "inactive resize changed remembered pane"
        resize("up")  # omitted target and one-cell default
        sizes({"upper": (10, 35), "lower": (8, 35)})
        client.send(b"\x02\t")
        wait_until(lambda: active() == logs, "resize changed last-window history")
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        for direction, cells, pane in (("right", 0, lower), ("right", 1, 99999),
                                       ("up", 1, left), ("right", 1, logs)):
            resize(direction, cells, pane, success=False)
        for body in (f"action='resize-pane'\npane={lower}\ndirection='right'\ncells=0\n".encode(),
                     b"action='resize-pane'\ndirection='diagonal'\ncells=1\n",
                     b"action='resize-pane'\ndirection='right'\n"):
            assert not wire(body)["ok"]
        assert panes() == before
        client.send(b"/resize-check")
        expect_footer(client, b"Search /resize-check")
        resize("right", 1, lower)
        expect_footer(client, b"LOCKED")
        sizes({"left": (20, 42), "upper": (10, 34), "lower": (8, 34)})
        detach()

        client = Session(extra_env=env, arguments=("attach", name))
        client.expect(b"RESIZE_PROBE_READY")
        run("select-window", "-w", 1)
        client.send(b"\x02Z")
        wait_until(lambda: record("lower")["columns"] == 78, "zoom did not resize active pane")
        assert "zoomed" in resize("left", 1, lower, success=False).stderr
        sizes({"lower": (20, 78)})
        client.send(b"\x02Z")
        sizes({"lower": (8, 34)})
        run("select-window", "-w", 2)
        resize("right", 65535, lower)
        sizes({"upper": (10, 1), "lower": (8, 1), "left": (20, 75)})
        assert "cannot move" in resize("right", 1, lower, success=False).stderr
        resize("left", 33, lower)
        sizes({"upper": (10, 34), "lower": (8, 34), "left": (20, 42)})
        before = panes()
        detach()
        resize("down", 1, lower)
        sizes({"upper": (11, 34), "lower": (7, 34), "logs": (20, 78)})
        assert panes() == before
        subprocess.run([BINARY, "save", name], env=env, capture_output=True, check=True, timeout=8)
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        saved = tomllib.loads(snapshot.read_text())
        assert saved["active_window"] == 1 and saved["windows"][0]["layout"]["active"] == 2
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, check=True, timeout=8)
        client = Session(extra_env=env, arguments=("attach", name, "--create"))
        client.expect(b"RUSTMUX_READY>")
        restored = panes()
        assert next(p["window"] for p in restored if p["active"]) == 2
        for item, label in zip(restored, ("restored-left", "restored-upper", "restored-lower", "restored-logs")):
            start_probe(item["id"], label)
        sizes({"restored-left": (20, 42), "restored-upper": (11, 34),
               "restored-lower": (7, 34), "restored-logs": (20, 78)})
        detach()
    finally:
        if client:
            client.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

print("script pane resize: nested/inactive PTYs, focus, bounds, detached mutation and restore passed")

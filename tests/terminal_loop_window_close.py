"""Script window closure removes all owned panes and preserves survivor state."""
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


with tempfile.TemporaryDirectory(prefix="rustmux-window-close-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("remain_on_exit=true\nsave_scrollback=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"window-close-{os.getpid()}"
    runtime = Path(f"/tmp/rustmux-{os.geteuid()}")
    client = None
    probe = root / "probe.py"
    probe.write_text("""import json,os,select,sys,tty
tty.setraw(0)
os.write(1,b'\\x1b[?1004h\\x1b[?1h\\x1b[2J\\x1b[HWINDOW_CLOSE_READY')
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

    def command(label):
        return "exec python3 -u " + shlex.quote(str(probe)) + " " + shlex.quote(str(root / f"{label}.json"))

    def start(action, label, *arguments):
        pane = int(run(action, *arguments, "--command", command(label)).stdout)
        wait_until(lambda: record(label) is not None, f"probe {label} did not start")
        assert record(label)["pid"] == next(p["pid"] for p in panes() if p["id"] == pane)
        return pane

    def size(label, rows, columns):
        wait_until(lambda: (record(label)["rows"], record(label)["columns"]) == (rows, columns),
                   ("PTY size mismatch", label, rows, columns))

    def gone(pid):
        try:
            os.kill(pid, 0)
            return False
        except ProcessLookupError:
            return True

    def close(window=None):
        before = panes()
        number = window if window is not None else next(p["window"] for p in before if p["active"])
        removed = [p for p in before if p["window"] == number]
        assert removed
        arguments = ["-w", str(window)] if window is not None else []
        assert run("close-window", *arguments).stdout == ""
        assert all(p["id"] not in {old["id"] for old in removed} for p in panes())
        for pane in removed:
            wait_until(lambda: gone(pane["pid"]), f"closed child {pane['pid']} was not reaped")

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

    def final_guard():
        before = panes()
        for arguments in ((), ("-w", "1")):
            assert "final session window" in run("close-window", *arguments, success=False).stderr
        rejected = wire(b"action='close-window'\n")
        assert not rejected["ok"] and "final session window" in rejected["output"]
        assert panes() == before
        assert all(not gone(p["pid"]) for p in before)

    try:
        layout = root / "startup.toml"
        layout.write_text("[[windows]]\nname='shell'\n[[windows.panes]]\ncommand="
                          + json.dumps(command("first")) + "\n")
        client = Session(extra_env=env, arguments=("new", name, "--layout", str(layout)))
        client.expect(b"WINDOW_CLOSE_READY")
        first = active()
        wait_until(lambda: record("first") is not None, "first probe did not start")
        right = start("split-pane", "right", "-p", first)
        client.send(b"\x02Z")
        size("right", 20, 78)
        removed_a = start("new-window", "removed-a", "--name", "removed")
        start("split-pane", "removed-b", "-p", removed_a, "--down")
        exited = int(run("split-pane", "--command", "printf WINDOW_EXITED; exit 7").stdout)
        wait_until(lambda: any(p["id"] == exited and p["output_complete"] for p in panes()),
                   "retained job did not exit")
        assert next(p for p in panes() if p["id"] == exited)["exit_code"] == 7
        keep = start("new-window", "keep", "--name", "keep")
        keep_bottom = start("split-pane", "keep-bottom", "-p", keep, "--down")
        # Window numbers follow the reordered bar, rather than creation order.
        client.send(b"\x02<")
        wait_until(lambda: next(p["window"] for p in panes() if p["id"] == keep) == 2,
                   "window reordering did not complete")
        wait_until(lambda: record("right")["input"].endswith(b"\x1b[O".hex()),
                   "setup focus-out did not arrive")
        baseline = {label: record(label) for label in ("first", "right", "keep", "keep-bottom")}
        server_pid = (runtime / f"{name}.pid").read_bytes()
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        assert "unknown window number" in run("close-window", "-w", "65535", success=False).stderr
        for invalid in ("0", "-1", "65536", "invalid"):
            run("close-window", "-w", invalid, success=False)
        for body in (b"action='close-window'\nwindow=0\n",
                     b"action='close-window'\nwindow='invalid'\n",
                     b"action='close-window'\nwindow=3\nextra=true\n"):
            assert not wire(body)["ok"]
        assert panes() == before
        client.send(b"/close-window-check")
        expect_footer(client, b"Search /close-window-check")
        close(3)  # inactive multi-pane window includes a retained exited child
        expect_footer(client, b"LOCKED")
        assert active() == keep_bottom
        for label, old in baseline.items():
            assert record(label) == old, "inactive closure changed survivor process, size or focus"
        close()  # active multi-pane window falls back to remembered zoomed pane
        assert active() == right
        wait_until(lambda: record("right")["input"] == baseline["right"]["input"] + b"\x1b[I".hex(),
                   "fallback did not receive exactly one focus-in")
        size("right", 20, 78)
        assert client.private_modes.get(1), "window close changed application cursor mode"
        client.send(b"x")
        wait_until(lambda: record("right")["input"].endswith(b"x".hex()), "input routed to wrong survivor")
        assert record("first") == baseline["first"]
        client.send(b"\x02z")
        client.read(0.05)
        assert len(panes()) == 2 and active() == right, "script closure entered undo storage"
        assert (runtime / f"{name}.pid").read_bytes() == server_pid
        detach()

        client = Session(extra_env=env, arguments=("attach", name))
        client.expect(b"WINDOW_CLOSE_READY")
        middle = start("new-window", "middle", "--name", "middle")
        start("split-pane", "middle-right", "-p", middle)
        last = start("new-window", "last", "--name", "last")
        last_bottom = start("split-pane", "last-bottom", "-p", last, "--down")
        run("select-window", "-w", "2")
        wait_until(lambda: record("last-bottom")["input"].endswith(b"\x1b[O".hex()),
                   "last-window focus-out did not arrive")
        before_input = record("last-bottom")["input"]
        close()  # active middle window chooses its successor and remembered pane
        assert active() == last_bottom
        wait_until(lambda: record("last-bottom")["input"] == before_input + b"\x1b[I".hex(),
                   "successor did not receive exactly one focus-in")
        size("last-bottom", 9, 78)
        close()  # trailing active window chooses its predecessor
        assert active() == right
        size("right", 20, 78)
        client.send(b"\x02\t")
        client.read(0.05)
        assert active() == right, "last-window selected a removed window"
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        final_guard()  # final window is protected even though it has two panes
        client.send(b"/final-window-check")
        expect_footer(client, b"Search /final-window-check")
        run("select-pane", "-p", right)
        expect_footer(client, b"LOCKED")
        detach()

        final_guard()
        detached = start("new-window", "detached", "--name", "detached")
        exited = int(run("split-pane", "-p", detached, "--down", "--command", "exit 9").stdout)
        wait_until(lambda: any(p["id"] == exited and p["output_complete"] for p in panes()),
                   "detached retained job did not exit")
        keeper = start("new-window", "keeper", "--name", "keeper")
        baseline = record("keeper")
        close(2)
        assert active() == keeper and record("keeper") == baseline
        assert next(p["window"] for p in panes() if p["id"] == keeper) == 2
        close()
        assert active() == right
        final_guard()
        subprocess.run([BINARY, "save", name], env=env, capture_output=True, check=True, timeout=8)
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        saved = tomllib.loads(snapshot.read_text())
        assert len(saved["windows"]) == 1 and len(saved["windows"][0]["panes"]) == 2
        window = saved["windows"][0]
        assert [p["command"] for p in window["panes"]] == [command("first"), command("right")]
        assert window["layout"]["zoomed"] and window["layout"]["active"] == 1
        removed_labels = ("removed-a", "removed-b", "keep", "keep-bottom", "middle",
                          "middle-right", "last", "last-bottom", "detached", "keeper")
        dead = {label: record(label)["pid"] for label in removed_labels}
        old = {label: record(label)["pid"] for label in ("first", "right")}
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, check=True, timeout=8)
        client = Session(extra_env=env, arguments=("attach", name, "--create"))
        client.expect(b"WINDOW_CLOSE_READY")
        wait_until(lambda: all(record(label)["pid"] != pid for label, pid in old.items()),
                   "saved survivor processes did not restart")
        size("right", 20, 78)
        assert len(panes()) == 2
        assert next(p["pid"] for p in panes() if p["active"]) == record("right")["pid"]
        assert all(record(label)["pid"] == pid and gone(pid) for label, pid in dead.items()), "closed job was replayed"
        detach()
    finally:
        if client:
            client.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

print("script window close: group cleanup, reordered targets, focus fallback, final-window guard and restore passed")

"""Script window metadata changes preserve focus/processes and survive restore."""
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


with tempfile.TemporaryDirectory(prefix="rustmux-window-rename-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("save_scrollback=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"window-rename-{os.getpid()}"
    probe_name = f"rename-focus-{os.getpid()}"
    runtime = Path(f"/tmp/rustmux-{os.geteuid()}")
    client = None

    def run(action, *arguments, success=True, target=None):
        result = subprocess.run([BINARY, action, "-s", target or name, *map(str, arguments)],
                                env=env, capture_output=True, text=True, timeout=8)
        assert (result.returncode == 0) == success, (action, arguments, result)
        return result

    def panes(target=None):
        return tomllib.loads(run("list-panes", "--toml", target=target).stdout)["panes"]

    def active(target=None):
        return next(p["id"] for p in panes(target) if p["active"])

    def without_names():
        return [{k: v for k, v in p.items() if k != "window_name"} for p in panes()]

    def wait_until(predicate, detail):
        deadline = time.monotonic() + 6
        while not predicate():
            if client:
                client.read(0.01)
            assert time.monotonic() < deadline, detail
            time.sleep(0.01)

    def detach():
        global client
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None

    def rename(label, window=None, success=True):
        arguments = ["-w", str(window)] if window is not None else []
        return run("rename-window", *arguments, "--", label, success=success)

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

    try:
        client = Session(extra_env=env, arguments=("new", name))
        client.expect(b"RUSTMUX_READY>")
        first = active()
        client.send(b"stty -echo; KEEP=first; printf 'FIRST_READY\\n'\n")
        client.expect(b"FIRST_READY")
        right = int(run("split-pane", "-p", first).stdout)
        client.expect(b"RUSTMUX_READY>")
        client.send(b"stty -echo; KEEP=right; printf 'RIGHT_READY\\n'\n")
        client.expect(b"RIGHT_READY")
        logs = int(run("new-window", "--name", "logs").stdout)
        client.expect(b"RUSTMUX_READY>")
        client.send(b"stty -echo; KEEP=logs; printf 'LOGS_READY\\n'\n")
        client.expect(b"LOGS_READY")
        before = without_names()
        server_pid = (runtime / f"{name}.pid").read_bytes()
        assert rename("编辑器", 1).stdout == ""
        expect_bar(client, "编辑器".encode())
        assert rename("构建").stdout == ""
        expect_bar(client, "构建".encode())
        assert without_names() == before
        assert [p["window_name"] for p in panes()] == ["编辑器", "编辑器", "构建"]
        client.send(b"printf 'ROUTED:%s\\n' $KEEP\n")
        client.expect(b"ROUTED:logs")
        assert "ROUTED:logs" not in run("capture-pane", "-p", first).stdout

        # Duplicate names are metadata, never target identities.
        rename("构建", 1)
        assert all(p["window_name"] == "构建" for p in panes())
        rename("left", 1)
        rename("right", 2)
        client.send(b"\x02<")
        wait_until(lambda: next(p["window"] for p in panes() if p["id"] == logs) == 1,
                   "window reordering did not complete")
        rename("reordered", 2)
        assert next(p["window_name"] for p in panes() if p["id"] == first) == "reordered"
        assert active() == logs
        exact_limit = "窗" * 42 + "ab"  # 128 UTF-8 bytes, not 128 characters.
        rename(exact_limit)
        assert next(p["window_name"] for p in panes() if p["id"] == logs) == exact_limit
        # The bar clips names to the physical width. Use a short active label
        # before asserting that an inactive window's label is visible too.
        rename("right")

        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        for invalid in ("", "窗" * 43, "x" * 129, "bad\x1b[31m", "bad\nname", "bad\x7fname"):
            assert "window name must contain" in rename(invalid, success=False).stderr
        assert "unknown window number" in rename("bad-target", 42, success=False).stderr
        for body in (b"action='rename-window'\nwindow=0\nname='zero'\n",
                     b"action='rename-window'\nwindow=1\nname=''\n",
                     b"action='rename-window'\nwindow=1\n"):
            rejected = wire(body)
            assert not rejected["ok"], rejected
        assert panes() == before
        client.send(b"/rename-check")
        expect_footer(client, b"Search /rename-check")
        # Successful mutation follows existing script controls: dismiss stale
        # overlays, reset shortcuts and redraw even for an inactive target.
        rename("renamed", 2)
        expect_footer(client, b"LOCKED")
        expect_bar(client, b"renamed")
        assert active() == logs
        client.send(b"\x02")
        expect_footer(client, b"NORMAL")
        rename("live")
        expect_footer(client, b"LOCKED")
        client.send(b"printf 'AFTER_RENAME:%s\\n' $KEEP\n")
        client.expect(b"AFTER_RENAME:logs")
        client.send(b"\x02\t")
        wait_until(lambda: active() == right, "rename changed last-window or remembered pane focus")
        client.expect(b"RIGHT_READY")
        client.send(b"printf 'REMEMBERED:%s\\n' $KEEP\n")
        client.expect(b"REMEMBERED:right")
        client.send(b"\x02\t")
        wait_until(lambda: active() == logs, "last-window return failed")
        assert {p["id"]: p["pid"] for p in panes()} == {p["id"]: p["pid"] for p in before}
        assert (runtime / f"{name}.pid").read_bytes() == server_pid
        detach()

        # Detached mutation preserves remembered focus and layout in a manual
        # snapshot, then restores names into a new server without executing them.
        before = without_names()
        label = f"$(touch {root / 'name-executed'})"
        rename(label, 2)
        rename("detached")
        assert without_names() == before
        saved = subprocess.run([BINARY, "save", name], env=env, capture_output=True, timeout=8)
        assert saved.returncode == 0, saved
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        state = tomllib.loads(snapshot.read_text())
        assert [w["name"] for w in state["windows"]] == ["detached", label], state
        assert state["active_window"] == 0 and state["windows"][1]["layout"]["active"] == 1
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, check=True, timeout=8)
        client = Session(extra_env=env, arguments=("attach", name, "--create"))
        client.expect(b"RUSTMUX_READY>")
        expect_bar(client, b"detached")
        restored = panes()
        assert [p["window_name"] for p in restored] == ["detached", label, label]
        assert next(p["window"] for p in restored if p["active"]) == 1
        assert sum(p["selected"] for p in restored) == 2
        assert not (root / "name-executed").exists()
        detach()

        # A raw application observes no spurious focus report, and retains its
        # cursor-key mode and actual PTY dimensions after the label changes.
        client = Session(extra_env=env, arguments=("new", probe_name))
        client.expect(b"RUSTMUX_READY>")
        probe = """import os,sys,tty
tty.setraw(0)
os.write(1,b'\\x1b[?1004h\\x1b[?1h\\x1b[2J\\x1b[HRENAME_PROBE_READY')
with open(sys.argv[1],'wb',buffering=0) as log:
    while True:
        data=os.read(0,1024)
        if not data: break
        log.write(data)
        size=os.get_terminal_size(0)
        os.write(1,f'\\r\\nRENAME_PROBE_SIZE:{size.lines} {size.columns}'.encode())
"""
        events = root / "rename.events"
        client.send(("exec python3 -u -c " + shlex.quote(probe) + " " + shlex.quote(str(events)) + "\n").encode())
        client.expect(b"RENAME_PROBE_READY")
        wait_until(events.exists, "probe log not created")
        baseline = events.read_bytes()
        process = panes(probe_name)
        result = run("rename-window", "probe-new", target=probe_name)
        assert result.stdout == ""
        expect_bar(client, b"probe-new")
        client.send(b"x")
        wait_until(lambda: events.read_bytes() == baseline + b"x", "rename emitted a focus event")
        client.expect(b"RENAME_PROBE_SIZE:20 78")
        assert client.private_modes.get(1), "rename changed application cursor mode"
        assert [{k: v for k, v in p.items() if k != "window_name"} for p in panes(probe_name)] == \
            [{k: v for k, v in p.items() if k != "window_name"} for p in process]
        detach()
    finally:
        if client:
            client.close()
        for target in (name, probe_name):
            subprocess.run([BINARY, "kill", target], env=env, capture_output=True, timeout=8)

print("script window rename, focus/input isolation, validation and snapshot restore passed")

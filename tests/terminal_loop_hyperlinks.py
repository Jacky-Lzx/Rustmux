"""OSC 8 cell metadata survives a real server, composition, history and reconnect."""
import fcntl
import json
import os
from pathlib import Path
import re
import shlex
import signal
import struct
import subprocess
import tempfile
import termios
import time
import tomllib
from terminal_loop_support import BINARY, Session, expect_footer

with tempfile.TemporaryDirectory(prefix="rustmux-hyperlinks-") as temporary:
    root = Path(temporary)
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"hyperlinks-{os.getpid()}"
    session = None
    probe = root / "probe.py"
    probe.write_text(r'''
import json, os, sys, tty
from pathlib import Path
tty.setraw(0)
label = sys.argv[1].encode()
path = Path(sys.argv[2])
actions = []
def emit_link():
    os.write(1, b"\x1b]8;id=shared;https://example.test/path\x1b\\" + label + b"_LINK")
def save():
    path.with_suffix(".tmp").write_text(json.dumps({"pid":os.getpid(), "actions":actions}))
    os.replace(path.with_suffix(".tmp"), path)
os.write(1, b"\x1b[2J\x1b[H")
emit_link()  # Intentionally leave the application's link open.
save()
while True:
    data = os.read(0, 1024)
    if not data: break
    for byte in data:
        if byte == 1: os.write(1, b"\x1b[?1049h" + label + b"_ALT_PLAIN")
        elif byte == 2: os.write(1, b"\x1b[?1049l\x1b[2;1H" + label + b"_MAIN_PLAIN")
        elif byte == 3:
            os.write(1, b"\x1b[2J\x1b[H")
            for _ in range(50):
                emit_link()
                os.write(1, b"\x1b]8;;\x1b\\\r\n")
        actions.append(byte)
        save()
''')

    def run(action, *args):
        target = [name] if action in ("new", "kill") else ["-s", name]
        result = subprocess.run([BINARY, *action.split(), *target, *map(str, args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, (action, args, result)
        return result.stdout

    def record(label):
        path = root / f"{label}.json"
        return json.loads(path.read_text()) if path.exists() else None

    def wait(predicate):
        deadline = time.monotonic() + 4
        while not predicate():
            if session: session.read(0.01)
            assert time.monotonic() < deadline, bytes(session.output[-3000:]) if session else "detached"
            time.sleep(0.005)

    def visible(marker):
        wait(lambda: any(marker in row for row in session.physical_rows))

    def command(pane, label, byte):
        previous = len(record(label)["actions"])
        run("pane send-keys", "-p", pane, "--literal", chr(byte))
        wait(lambda: len(record(label)["actions"]) > previous)

    def links():
        # Independent byte inspection: every rendered open must have a matching
        # close before positioning/UI output. URLs are metadata, never visible.
        found = []
        for match in re.finditer(rb"\x1b\]8;id=([^;]+);([^\x1b]*)\x1b\\(.*?)\x1b\]8;;\x1b\\",
                                 session.output, re.S):
            identity, uri, payload = match.groups()
            text = re.sub(rb"\x1b\[[0-9;: ?]*m", b"", payload)
            assert text and any(text in label for label in (b"A_LINK", b"B_LINK")), text
            assert uri == b"https://example.test/path", uri
            assert identity.startswith(b"rmx-"), identity
            found.append((identity, text))
        opens = re.findall(rb"\x1b\]8;id=", session.output)
        assert len(opens) == len(found), bytes(session.output)
        return found

    def detach():
        global session
        session.send(b"\x02d")
        session.finish(0)
        assert b"\x1b]8;;\x1b\\\x1b]22;\x1b\\\x1b[0m\x1b[?25h\x1b[?1049l" in session.output
        links()
        session.close()
        session = None

    try:
        run("new", "--detached")
        left = tomllib.loads(run("pane list", "--toml"))["panes"][0]["id"]
        launch = lambda label: f"exec python3 -u {shlex.quote(str(probe))} {label} {shlex.quote(str(root / (label+'.json')))}"
        run("pane send-keys", "-p", left, "--literal", "--enter", launch("A"))
        wait(lambda: record("A") is not None)
        right = int(run("pane split", "-p", left, "--command", launch("B")))
        wait(lambda: record("B") is not None)
        identities = {p["id"]:p["pid"] for p in tomllib.loads(run("pane list", "--toml"))["panes"]}
        session = Session(extra_env=env, arguments=("attach", name))
        visible(b"A_LINK")
        visible(b"B_LINK")
        first = links()
        a_id = next(identity for identity, text in first if text == b"A_LINK")
        b_id = next(identity for identity, text in first if text == b"B_LINK")
        assert a_id != b_id  # Identical child URI + ID remains scoped to each pane.
        assert "https://" not in run("pane capture", "-p", right)
        session.send(b"\x02?")
        visible(b"Shortcut Help")
        links()  # UI captions cannot inherit the child's unclosed link.
        session.send(b"q")
        visible(b"B_LINK")
        command(right, "B", 1)
        visible(b"B_ALT_PLAIN")
        links()
        command(right, "B", 2)
        visible(b"B_MAIN_PLAIN")
        visible(b"B_LINK")
        run("pane select", "-p", left)
        command(left, "A", 3)
        wait(lambda: run("pane capture", "-p", left, "--history").count("A_LINK") >= 50)
        session.send(b"\x02[")
        expect_footer(session, b"HISTORY")
        links()
        session.send(b"q")
        visible(b"A_LINK")
        detach()
        session = Session(extra_env=env, arguments=("attach", name))
        visible(b"A_LINK")
        visible(b"B_LINK")
        assert all(identity == (a_id if text == b"A_LINK" else b_id) for identity, text in links())
        fcntl.ioctl(session.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 26, 100, 0, 0))
        os.kill(session.app_pid, signal.SIGWINCH)
        wait(lambda: len(session.physical_rows) == 26)
        visible(b"A_LINK")
        visible(b"B_LINK")
        assert {p["id"]:p["pid"] for p in tomllib.loads(run("pane list", "--toml"))["panes"]} == identities
        detach()
    finally:
        if session: session.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

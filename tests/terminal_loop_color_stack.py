"""Pane-local color stacks survive a real server, buffer changes and reconnect."""
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

with tempfile.TemporaryDirectory(prefix="rustmux-color-stack-") as temporary:
    root = Path(temporary)
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"color-stack-{os.getpid()}"
    session = None
    counter = 0
    probe = root / "probe.py"
    probe.write_text(r'''
import json, os, sys, tty
from pathlib import Path
tty.setraw(0)
label = sys.argv[1].encode()
path = Path(sys.argv[2])
def osc(value): os.write(1, b"\x1b]" + value.encode() + b"\x1b\\")
def profile(seed):
    osc(f"10;#{seed:02x}0001;#00{seed:02x}02;#0003{seed:02x}")
    for index in range(256): osc(f"4;{index};#{seed:02x}0405")
def record(token, reply=None):
    with path.open("a") as output:
        output.write(json.dumps({"token":token, "pid":os.getpid(), "reply":reply}) + "\n")
def paint():
    os.write(1, b"\x1b[H\x1b[0m" + label + b"_DEFAULT\x1b[2;1H\x1b[31m" + label + b"_INDEXED\x1b[0m")
profile(int(sys.argv[3])); paint(); record(0)
while True:
    line = bytearray()
    while not line.endswith(b"\n"):
        byte = os.read(0,1)
        if not byte: sys.exit(0)
        line.extend(byte)
    token, operation, value = line.decode().rstrip("\n").split(":",2)
    reply = None
    if operation == "push": os.write(1, b"\x1b]30001\x07")
    elif operation == "pop": osc("30101")
    elif operation == "set": profile(int(value))
    elif operation == "clear":
        for code in (104,110,111,112): osc(str(code))
    elif operation == "alt": os.write(1,b"\x1b[?1049h")
    elif operation == "main": os.write(1,b"\x1b[?1049l")
    elif operation == "soft": os.write(1,b"\x1b[!p")
    elif operation == "reset": os.write(1,b"\x1bc")
    elif operation == "query":
        osc("10;?;?;?"); osc("4;1;?;255;?")
        data = bytearray()
        while data.count(b"\x1b\\") < 5: data.extend(os.read(0,1))
        reply = data.decode()
    paint()
    os.write(1,f"\x1b[3;1HACK_{token}".encode())
    record(int(token),reply)
''')

    def run(action, *args):
        target = [name] if action in ("new", "kill") else ["-s", name]
        result = subprocess.run([BINARY, action, *target, *map(str, args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, (action,args,result)
        return result.stdout

    def records(label):
        path = root / f"{label}.jsonl"
        if not path.exists(): return []
        return [json.loads(line) for line in path.read_text().splitlines(keepends=True) if line.endswith("\n")]

    def wait(predicate):
        deadline = time.monotonic() + 4
        while not predicate():
            if session: session.read(0.01)
            assert time.monotonic() < deadline, bytes(session.output[-2500:]) if session else "detached"
            time.sleep(0.005)

    def command(pane, label, action, value=""):
        global counter
        counter += 1
        run("send-keys", "-p", pane, "--literal", f"{counter}:{action}:{value}\n")
        wait(lambda: any(row["token"] == counter for row in records(label)))
        return next(row for row in records(label) if row["token"] == counter)

    def query(pane, label, expected):
        reply = command(pane,label,"query")["reply"]
        colors = {}
        for match in re.finditer(r"\x1b\](10|11|12|4);(?:(\d+);)?rgb:([0-9a-f]+)/([0-9a-f]+)/([0-9a-f]+)\x1b\\",reply):
            code,index,*components = match.groups()
            colors[(int(code),int(index) if index else None)] = tuple(int(c,16)//257 for c in components)
        assert colors == expected, (label,colors,expected,reply)

    def profile(seed):
        return {(10,None):(seed,0,1), (11,None):(0,seed,2), (12,None):(0,3,seed),
                (4,1):(seed,4,5), (4,255):(seed,4,5)}

    def attach(theme):
        global session
        session = Session(extra_env=env, arguments=("attach",name))
        wait(lambda: b"\x1b]12;?\x1b\\" in session.output)
        replies = bytearray()
        for (code,index),color in theme.items():
            key = f"{code};" + (f"{index};" if index is not None else "")
            replies.extend(f"\x1b]{key}rgb:{'/'.join(f'{c:02x}{c:02x}' for c in color)}\x1b\\".encode())
        session.send(replies + b"\x1b[?1;2c")
        wait(lambda: any(b"B_DEFAULT" in row for row in session.physical_rows))

    def cursor(seed):
        expected = f"#0003{seed:02x}".encode()
        wait(lambda: (found := re.findall(rb"\x1b\]12;([^\x1b]*)\x1b\\",session.output)) and found[-1] == expected)
        assert b"\x1b]30001" not in session.output and b"\x1b]30101" not in session.output
        session.output.clear()

    def detach():
        global session
        session.send(b"\x02d"); session.finish(0)
        assert b"\x1b]30001" not in session.output and b"\x1b]30101" not in session.output
        session.close(); session = None

    try:
        run("new","--detached")
        left = tomllib.loads(run("list-panes","--toml"))["panes"][0]["id"]
        launch = lambda label,seed: f"exec python3 -u {shlex.quote(str(probe))} {label} {shlex.quote(str(root/(label+'.jsonl')))} {seed}"
        run("send-keys","-p",left,"--literal","--enter",launch("A",11))
        wait(lambda: bool(records("A")))
        right = int(run("split-pane","-p",left,"--command",launch("B",22)))
        wait(lambda: bool(records("B")))
        identities = {p["id"]:p["pid"] for p in tomllib.loads(run("list-panes","--toml"))["panes"]}
        command(left,"A","push"); command(left,"A","set",33)
        command(left,"A","push"); command(left,"A","set",44)
        command(right,"B","push"); command(right,"B","set",55)
        query(left,"A",profile(44)); query(right,"B",profile(55))  # Replies work detached.
        attach(profile(101)); cursor(55)
        command(left,"A","pop"); query(left,"A",profile(33)); query(right,"B",profile(55))
        run("select-pane","-p",left); cursor(33)
        command(left,"A","alt"); command(left,"A","set",66)
        command(left,"A","pop"); query(left,"A",profile(11)); cursor(11)
        command(left,"A","main")
        command(right,"B","soft"); command(right,"B","pop"); query(right,"B",profile(22))
        command(left,"A","push"); command(left,"A","set",77)
        detach(); attach(profile(102))
        query(left,"A",profile(77))
        fcntl.ioctl(session.slave,termios.TIOCSWINSZ,struct.pack("HHHH",26,100,0,0))
        os.kill(session.app_pid,signal.SIGWINCH)
        wait(lambda: len(session.physical_rows) == 26)
        command(left,"A","pop"); query(left,"A",profile(11))
        command(left,"A","clear"); command(left,"A","push"); command(left,"A","set",88)
        detach(); attach(profile(103))
        command(left,"A","pop"); query(left,"A",profile(103))  # Restored unset overrides follow the new outer theme.
        run("select-pane","-p",right)
        command(right,"B","push"); command(right,"B","set",99)
        session.send(b"\x02?")
        wait(lambda: any(b"Shortcut Help" in row for row in session.physical_rows))
        session.send(b"q")
        wait(lambda: any(b"B_DEFAULT" in row for row in session.physical_rows))
        session.send(b"\x02["); expect_footer(session,b"HISTORY"); session.send(b"q")
        wait(lambda: any(b"B_DEFAULT" in row for row in session.physical_rows))
        command(right,"B","pop"); query(right,"B",profile(22)); cursor(22)
        command(right,"B","push"); command(right,"B","set",100)
        command(right,"B","reset"); command(right,"B","pop"); query(right,"B",profile(100))
        assert {p["id"]:p["pid"] for p in tomllib.loads(run("list-panes","--toml"))["panes"]} == identities
        assert all(row["pid"] == records(label)[0]["pid"] for label in ("A","B") for row in records(label))
        detach()
    finally:
        if session: session.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)

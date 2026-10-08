"""OSC 21 child replies, pane isolation and inherited resets through reconnect."""
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
from terminal_loop_support import BINARY, Session

with tempfile.TemporaryDirectory(prefix="rustmux-osc21-") as temporary:
    root = Path(temporary)
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root/"state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"osc21-{os.getpid()}"
    session = None
    counter = 0
    probe = root/"probe.py"
    probe.write_text(r'''
import json, os, sys, threading, tty
from pathlib import Path
tty.setraw(0)
path = Path(sys.argv[2])
def record(token, reply):
    with path.open("a") as output:
        output.write(json.dumps({"token": token, "pid": os.getpid(), "reply": reply}) + "\n")
os.write(1,sys.argv[1].encode()); record(0,"")
while True:
    line = bytearray()
    while not line.endswith(b"\n"):
        byte = os.read(0,1)
        if not byte: sys.exit(0)
        line.extend(byte)
    token, encoded = line.decode().strip().split(":",1)
    # All commands end in a DSR barrier. It orders replies after preceding output.
    encoded, _, repeat = encoded.partition("*")
    payload = bytes.fromhex(encoded) * int(repeat or 1) + b"\x1b[5n"
    def write_all():
        pending = memoryview(payload)
        while pending: pending = pending[os.write(1,pending):]
    writer = threading.Thread(target=write_all); writer.start()
    data = bytearray()
    while not data.endswith(b"\x1b[0n"): data.extend(os.read(0,4096))
    writer.join(); record(int(token),data[:-4].decode())
''')
    def run(action,*args):
        target = [name] if action in ("new","kill") else ["-s",name]
        result = subprocess.run([BINARY, *action.split(),*target,*map(str,args)],env=env,
                                capture_output=True,text=True,timeout=8)
        assert result.returncode == 0, (action,result.returncode,result.stderr)
        return result.stdout
    def records(label):
        path = root/f"{label}.jsonl"
        if not path.exists(): return []
        return [json.loads(line) for line in path.read_text().splitlines(keepends=True) if line.endswith("\n")]
    def wait(predicate):
        deadline = time.monotonic()+5
        while not predicate():
            if session: session.read(0.01)
            assert time.monotonic() < deadline, bytes(session.output[-2000:]) if session else "detached"
            time.sleep(0.005)
    def command(pane,label,data,repeat=1):
        global counter
        counter += 1
        run("pane send-keys","-p",pane,"--literal",f"{counter}:{data.hex()}*{repeat}\n")
        wait(lambda: any(row["token"] == counter for row in records(label)))
        return next(row["reply"] for row in records(label) if row["token"] == counter)
    def osc(values): return f"\x1b]21;{values}\x1b\\".encode()
    def attach(seed):
        global session
        session = Session(extra_env=env,arguments=("attach",name))
        wait(lambda: b"\x1b]12;?\x1b\\" in session.output)
        data = bytearray()
        for code in (10,11,12): data.extend(f"\x1b]{code};rgb:{seed:02x}/{seed:02x}/{seed:02x}\x1b\\".encode())
        data.extend(f"\x1b]4;1;rgb:{seed:02x}/{seed:02x}/{seed:02x}\x1b\\\x1b[?1;2c".encode())
        session.send(data)
        wait(lambda: any(b"PROBE_B" in row for row in session.physical_rows))
    def detach():
        global session
        assert b"\x1b]21;" not in session.output  # Commands/replies never escape the child.
        session.send(b"\x02d"); session.finish(0); session.close(); session = None
    try:
        run("new","--detached")
        left = tomllib.loads(run("pane list","--toml"))["panes"][0]["id"]
        launch = lambda label: f"exec python3 -u {shlex.quote(str(probe))} PROBE_{label} {shlex.quote(str(root/(label+'.jsonl')))}"
        run("pane send-keys","-p",left,"--literal","--enter",launch("A")); wait(lambda: records("A"))
        right = int(run("pane split","-p",left,"--command",launch("B"))); wait(lambda: records("B"))
        for pane,label,color in [(left,"A","123456"),(right,"B","abcdef")]:
            assert command(pane,label,osc(f"foreground=#{color};foreground=?")) == f"\x1b]21;foreground=rgb:{color[:2]}/{color[2:4]}/{color[4:]}\x1b\\"
        attach(33)
        assert command(left,"A",osc("foreground=?")) == "\x1b]21;foreground=rgb:12/34/56\x1b\\"
        assert command(right,"B",osc("foreground=?")) == "\x1b]21;foreground=rgb:ab/cd/ef\x1b\\"
        assert command(left,"A",osc("nonsense=?;cursor_text=?")) == "\x1b]21;unknown=bm9uc2Vuc2U;unknown=Y3Vyc29yX3RleHQ\x1b\\"
        command(right,"B",osc("cursor=#0a0b0c"))
        wait(lambda: b"\x1b]12;#0a0b0c\x1b\\" in session.output)
        command(left,"A",b"\x1b]30001\x07"+osc("cursor=#010203;1=#123456"))
        command(left,"A",b"\x1b[?1049h"+osc("foreground=#112233"))
        assert command(left,"A",b"\x1b]30101\x07"+osc("foreground=?")) == "\x1b]21;foreground=rgb:12/34/56\x1b\\"
        command(left,"A",b"\x1b[?1049l")
        command(left,"A",osc("foreground;background;cursor;1"))
        # Burst exceeds the 64 KiB child-input queue; the child drains concurrently.
        burst = osc("1=?")
        expected = "\x1b]21;1=rgb:21/21/21\x1b\\"*4000
        assert len(expected.encode()) > 65536
        assert command(left,"A",burst,repeat=4000) == expected
        detach(); attach(77)
        assert command(left,"A",osc("foreground=?;background=?;cursor=?;1=?")) == "\x1b]21;foreground=rgb:4d/4d/4d;background=rgb:4d/4d/4d;cursor=rgb:4d/4d/4d;1=rgb:4d/4d/4d\x1b\\"
        fcntl.ioctl(session.slave,termios.TIOCSWINSZ,struct.pack("HHHH",26,100,0,0)); os.kill(session.app_pid,signal.SIGWINCH)
        wait(lambda: len(session.physical_rows) == 26)
        assert command(right,"B",osc("foreground=?")) == "\x1b]21;foreground=rgb:ab/cd/ef\x1b\\"
        assert all(row["pid"] == records(label)[0]["pid"] for label in ("A","B") for row in records(label))
        detach()
    finally:
        if session: session.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)

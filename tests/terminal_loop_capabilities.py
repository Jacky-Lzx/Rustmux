"""Pane-local XTGETTCAP replies, including detached and backpressured queries."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import tomllib
from terminal_loop_support import BINARY, Session

with tempfile.TemporaryDirectory(prefix="rustmux-capabilities-") as temporary:
    root = Path(temporary)
    env = dict(os.environ, XDG_CONFIG_HOME=str(root/"config"), XDG_STATE_HOME=str(root/"state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="",
               CAPABILITY_ROOT=str(root))
    probe = root/"probe.py"
    probe.write_text("#!"+sys.executable+"\n"+r'''
import json, os, select, sys, threading, time, tty
from pathlib import Path
tty.setraw(0)
root=Path(os.environ["CAPABILITY_ROOT"])
label=sys.argv[1] if len(sys.argv)>1 and sys.argv[1]!="-i" else "foreground"
def record(done):
    target=root/(label+".json")
    temporary=target.with_suffix(".tmp")
    temporary.write_text(json.dumps({"pid":os.getpid(),"done":done}))
    temporary.replace(target)
def write(data):
    pending=memoryview(data)
    while pending: pending=pending[os.write(1,pending):]
def receive(expected):
    received=bytearray(); deadline=time.monotonic()+8
    while len(received)<len(expected):
        assert time.monotonic()<deadline,(label,len(received),len(expected),received[-200:])
        if select.select([0],[],[],0.05)[0]: received.extend(os.read(0,4096))
    assert received==expected,(label,received[-200:],expected[-200:])
record(False)
while not (root/("gate_"+label)).exists(): time.sleep(0.005)
write(b"\x1b[?2026h\x1b[31m")
# A query split immediately before ST must reply only when ST completes.
write(b"\x1bP+q436f;524742\x1b")
assert not select.select([0],[],[],0.05)[0], "reply before the terminator"
write(b"\\")
response=b"\x1bP1+r436f=323536;524742=38\x1b\\"
receive(response)
write(b"\x1bP+q544e\x1b\\\x1bP+q436f;544e;524742\x1b\\\x1b[5n")
receive(b"\x1bP0+r\x1b\\\x1bP1+r436f=323536\x1b\\\x1b[0n")
# Read concurrently with child output; the replies exceed the 64 KiB input
# queue and must make progress without an outer-terminal response.
request=b"\x1bP+q436f;524742\x1b\\"
writer=threading.Thread(target=write,args=(request*4000,))
writer.start(); receive(response*4000); writer.join(timeout=8)
assert not writer.is_alive()
write(b"\x1b[?2026l\x1b[0mCAP_OK_"+label.encode()+b"\r\n")
record(True)
if label=="foreground": sys.exit(0)
# Retain named pane processes until the scenario kills its own session.
while os.read(0,4096): pass
''')
    probe.chmod(0o755)
    name=f"capabilities-{os.getpid()}"
    client=None
    def run(action,*args):
        target=[name] if action in ("new","kill") else ["-s",name]
        result=subprocess.run([BINARY,action,*target,*map(str,args)],env=env,capture_output=True,text=True,timeout=8)
        assert result.returncode==0,(action,result.stderr)
        return result.stdout
    def record(label):
        path=root/(label+".json")
        return json.loads(path.read_text()) if path.exists() else None
    def wait(predicate):
        deadline=time.monotonic()+8
        while not predicate():
            if client:
                client.read(0.01)
                assert client.child.poll() is None,client.child.returncode
            assert time.monotonic()<deadline,"probe did not complete"
            time.sleep(0.005)
    def launch(label):
        return "exec "+shlex.quote(sys.executable)+" "+shlex.quote(str(probe))+" "+label
    try:
        (root/"gate_foreground").touch()
        foreground=Session(extra_env=dict(env,RUSTMUX_SHELL=str(probe)),lifetime=20)
        try:
            raw=foreground.expect(b"CAP_OK_foreground")
            assert b"\x1bP+q" not in raw and b"\x1bP1+r" not in raw
            foreground.finish(0)
        finally: foreground.close()
        run("new","--detached")
        left=tomllib.loads(run("list-panes","--toml"))["panes"][0]["id"]
        run("send-keys","-p",left,"--literal","--enter",launch("A"))
        right=int(run("split-pane","-p",left,"--command",launch("B")))
        run("new-window","--name","other","--command",launch("C"))
        wait(lambda: all(record(label) for label in ("A","B","C")))
        original={label:record(label)["pid"] for label in ("A","B","C")}
        # Detached background queries are answered locally with no client.
        (root/"gate_A").touch(); wait(lambda: record("A")["done"])
        run("select-window","-w",1)
        client=Session(extra_env=env,arguments=("attach",name),lifetime=20)
        raw=client.expect(b"CAP_OK_A")
        assert b"\x1bP+q" not in raw and b"\x1bP1+r" not in raw
        # Keep A active while B queries: replies must go to their source pane.
        run("select-pane","-p",left)
        (root/"gate_B").touch(); wait(lambda: record("B")["done"])
        raw=client.expect(b"CAP_OK_B")
        assert b"\x1bP+q" not in raw and b"\x1bP1+r" not in raw
        # Another window can query without taking focus.
        (root/"gate_C").touch(); wait(lambda: record("C")["done"])
        assert next(p["window"] for p in tomllib.loads(run("list-panes","--toml"))["panes"] if p["active"])==1
        run("select-window","-w",2)
        raw=client.expect(b"CAP_OK_C")
        assert b"\x1bP+q" not in raw and b"\x1bP1+r" not in raw
        assert all(record(label)["pid"]==original[label] for label in ("A","B","C"))
        client.send(b"\x02d"); client.finish(0)
    finally:
        if client: client.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)

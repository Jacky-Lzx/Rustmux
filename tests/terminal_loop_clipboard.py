"""Opt-in child OSC 52 writes, config reload and attachment-local delivery."""
import base64
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

with tempfile.TemporaryDirectory(prefix="rustmux-clipboard-") as temporary:
    root = Path(temporary)
    config = root/"rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("clipboard_write=false\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root/"state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"clipboard-{os.getpid()}"
    client = None
    counter = 0
    probe = root/"probe.py"
    probe.write_text(r'''
import base64, json, os, signal, sys, threading, time, tty
from pathlib import Path
tty.setraw(0)
signal.signal(signal.SIGINT,signal.SIG_IGN) # Behave like the retained interactive shell on close.
path=Path(sys.argv[2])
def record(token):
    with path.open("a") as output: output.write(json.dumps({"token":token,"pid":os.getpid()})+"\n")
os.write(1,("PROBE_"+sys.argv[1]).encode()); record(0)
while True:
    line=bytearray()
    while not line.endswith(b"\n"):
        byte=os.read(0,1)
        if not byte: sys.exit(0)
        line.extend(byte)
    token,operation,value=line.decode().strip().split(":",2)
    if operation == "later":
        data = bytes.fromhex(value)
        def emit_later(data=data, token=token):
            gate = path.parent/("gate_"+token)
            while not gate.exists(): time.sleep(0.005)
            os.write(1,data+b"HIDDEN_COPY_DONE")
            (path.parent/("emitted_"+token)).touch()
        threading.Thread(target=emit_later,daemon=True).start()
        data = b""
    elif operation == "max": data=b"\x1b]52;c;"+base64.b64encode(b"M"*int(value))+b"\x07"
    elif operation == "flood": data=(b"\x1b]52;c;"+base64.b64encode(b"F"*2048)+b"\x07")*int(value)
    else: data=bytes.fromhex(value)
    # Deliberately incomplete strings skip the DSR barrier until a later command.
    pending=memoryview(data)
    while pending: pending=pending[os.write(1,pending):]
    if operation == "partial": record(int(token)); continue
    os.write(1,b"\x1b[5n")
    reply=bytearray()
    while not reply.endswith(b"\x1b[0n"): reply.extend(os.read(0,4096))
    assert reply == b"\x1b[0n", reply
    record(int(token))
''')
    def run(action,*args):
        target=[name] if action in ("new","kill") else ["-s",name]
        result=subprocess.run([BINARY,action,*target,*map(str,args)],env=env,capture_output=True,text=True,timeout=8)
        assert result.returncode == 0,(action,result.returncode,result.stderr)
        return result.stdout
    def records(label):
        path=root/f"{label}.jsonl"
        if not path.exists(): return []
        return [json.loads(line) for line in path.read_text().splitlines(keepends=True) if line.endswith("\n")]
    def wait(predicate,detail="timeout"):
        deadline=time.monotonic()+6
        while not predicate():
            if client: client.read(0.01)
            assert time.monotonic() < deadline,(detail, client.physical_rows if client else None, bytes(client.output[-1500:]) if client else None)
            time.sleep(0.005)
    def status(): return tomllib.loads(run("show-config"))
    def write(enabled):
        config.write_text(f"clipboard_write={str(enabled).lower()}\n")
        wait(lambda: status()["settings"]["clipboard_write"] == enabled and not status().get("error"),"config reload")
    def clipboard(selection,data,terminator=b"\x1b\\"):
        return b"\x1b]52;"+selection+b";"+base64.b64encode(data)+terminator
    def packets():
        return [(selection,base64.b64decode(data,validate=True)) for selection,data in
                re.findall(rb"\x1b\]52;([cpqs0-7]*);([A-Za-z0-9+/=]*)\x07",client.output)]
    def command(pane,label,data=b"",operation="send",value=None):
        global counter
        counter+=1
        run("send-keys","-p",pane,"--literal",f"{counter}:{operation}:{value if value is not None else data.hex()}\n")
        wait(lambda: any(row["token"]==counter for row in records(label)),"probe completion")
        if client:
            # The DSR barrier confirms parsing. Drain the asynchronously queued outer write.
            deadline=time.monotonic()+0.15
            while time.monotonic()<deadline: client.read(0.01)
    def clear():
        client.read(0.01); client.output.clear()
    def attach():
        global client
        client=Session(extra_env=env,arguments=("attach",name))
        raw=client.expect(b"PROBE_B"); assert b"\x1b]52;" not in raw
    def detach():
        global client
        client.send(b"\x02d"); client.finish(0); client.close(); client=None
    try:
        run("new","--detached")
        left=tomllib.loads(run("list-panes","--toml"))["panes"][0]["id"]
        launch=lambda label: f"exec python3 -u {shlex.quote(str(probe))} {label} {shlex.quote(str(root/(label+'.jsonl')))}"
        run("send-keys","-p",left,"--literal","--enter",launch("A")); wait(lambda: records("A"))
        right=int(run("split-pane","-p",left,"--command",launch("B"))); wait(lambda: records("B"))
        original={label:records(label)[0]["pid"] for label in ("A","B")}
        attach(); clear()
        command(left,"A",clipboard(b"c",b"default blocked")); assert not packets()
        write(True); clear()
        command(left,"A",clipboard(b"c","中\ncopy".encode())); assert packets()==[(b"c","中\ncopy".encode())]
        clear(); command(right,"B",clipboard(b"p",b"foreground",b"\x07")); assert packets()==[(b"p",b"foreground")]
        clear(); command(left,"A",clipboard(b"",b"")); assert packets()==[(b"",b"")]
        for invalid in [b"\x1b]52;c;?\x07",b"\x1b]52;c;!\x07",b"\x1bPignored\x1b]52;c;YWJj\x07\x1b\\",b"\x1b]52;c;YW\x18"]:
            clear(); command(left,"A",invalid); assert not packets()
        clear(); command(left,"A",operation="max",value=32768); assert packets()==[(b"c",b"M"*32768)]
        clear(); command(left,"A",operation="max",value=32769); assert not packets()
        clear(); command(left,"A",b"\x1b]52;c;YW",operation="partial")
        # Confirm the partial output was consumed before changing policy.
        wait(lambda: base64.b64decode(tomllib.loads(run("read-pane-output","-p",left,"--after","0"))["bytes_base64"]).endswith(b"\x1b]52;c;YW"))
        write(False); write(True)
        command(left,"A",b"Jj\x07"); assert not packets()
        config.write_text("clipboard_write='invalid'\n")
        wait(lambda: bool(status().get("error")),"invalid config rejected")
        clear(); command(left,"A",clipboard(b"c",b"last valid policy")); assert packets()==[(b"c",b"last valid policy")]
        write(True); clear()
        command(left,"A",b"\x1b]52;c;YW",operation="partial")
        wait(lambda: base64.b64decode(tomllib.loads(run("read-pane-output","-p",left,"--after","0"))["bytes_base64"]).endswith(b"\x1b]52;c;YW"))
        detach(); command(left,"A",b"Jj\x07")
        command(right,"B",clipboard(b"c",b"detached")); attach(); assert not packets()
        clear(); command(left,"A",clipboard(b"c",b"reconnected")); assert packets()==[(b"c",b"reconnected")]
        # Incomplete requests cannot span attachment boundaries, even if no
        # remaining bytes are observed while detached.
        clear(); command(right,"B",b"\x1b]52;c;YW",operation="partial")
        wait(lambda: base64.b64decode(tomllib.loads(run("read-pane-output","-p",right,"--after","0"))["bytes_base64"]).endswith(b"\x1b]52;c;YW"))
        detach(); attach(); clear(); command(right,"B",b"Jj\x07"); assert not packets()
        # A hidden live pane drains requests without a backlog for undo.
        run("select-pane","-p",left)
        clear(); command(left,"A",clipboard(b"c",b"hidden"),operation="later")
        hidden_token=counter
        client.send(b"\x02x"); client.expect(b"Close pane? Type yes:")
        client.send(b"yes\r")
        wait(lambda: all(p["id"]!=left for p in tomllib.loads(run("list-panes","--toml"))["panes"]))
        (root/f"gate_{hidden_token}").touch(); wait(lambda: (root/f"emitted_{hidden_token}").exists())
        deadline=time.monotonic()+0.3
        while time.monotonic()<deadline: client.read(0.01)
        assert not packets()
        client.send(b"\x02z")
        wait(lambda: any(p["id"]==left for p in tomllib.loads(run("list-panes","--toml"))["panes"]))
        restored=client.expect(b"HIDDEN_COPY_DONE"); assert b"\x1b]52;" not in restored and not packets()
        clear(); command(left,"A",clipboard(b"c",b"after undo")); assert packets()==[(b"c",b"after undo")]
        # Resize and buffer switches retain ordinary pane behavior.
        clear(); command(left,"A",b"\x1b[?1049h"+clipboard(b"c",b"alternate")+b"\x1b[?1049l")
        assert packets()==[(b"c",b"alternate")]
        fcntl.ioctl(client.slave,termios.TIOCSWINSZ,struct.pack("HHHH",26,100,0,0)); os.kill(client.app_pid,signal.SIGWINCH)
        wait(lambda: len(client.physical_rows)==26)
        clear(); command(left,"A",operation="flood",value=200)
        assert packets() and all(data==b"F"*2048 for _,data in packets())
        clear(); command(left,"A",clipboard(b"c",b"after flood")); assert packets()==[(b"c",b"after flood")]
        # The foreground unnamed path uses the same config and event routing.
        local=Session(extra_env=env)
        try:
            local.expect(b"RUSTMUX_READY>")
            local.send(b"stty -echo; printf 'LOCAL_%s\\n' QUIET\n"); local.expect(b"LOCAL_QUIET")
            local.send(b"printf '\\033]52;c;bG9jYWw=\\007LOCAL_DONE\\n'\n")
            raw=local.expect(b"LOCAL_DONE")
            assert b"\x1b]52;c;bG9jYWw=\x07" in raw
            local.send(b"exit\n"); local.finish(0)
        finally: local.close()
        write(False); clear(); command(right,"B",clipboard(b"c",b"disabled")); assert not packets()
        assert all(row["pid"]==original[label] for label in ("A","B") for row in records(label))
        detach()
    finally:
        if client: client.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)

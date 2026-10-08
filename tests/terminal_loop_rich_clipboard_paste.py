"""Kitty MIME paste notifications through real child PTYs and a simulated host."""
import base64
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import time
import tomllib
from terminal_loop_support import BINARY, Session

PREFIX = b"\x1b]5522;"
ST = b"\x1b\\"
QUERY = b"\x1b[?5522$p"
ON = b"\x1b[?5522h"
OFF = b"\x1b[?5522l"
def packet(meta, payload=b""):
    return PREFIX + meta.encode() + (b";" + payload if payload else b"") + ST
OK = packet("type=read:status=OK:loc=primary:pw=c2VjcmV0")
DATA = packet("type=read:status=DATA:mime=Lg==:pw=c2VjcmV0", base64.b64encode(b"text/plain image/png\n"))
DONE = packet("type=read:status=DONE:pw=c2VjcmV0")
EVENT = OK + DATA + DONE

with tempfile.TemporaryDirectory(prefix="rustmux-paste-events-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("clipboard_read=false\nremain_on_exit=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root/"state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"paste-events-{os.getpid()}"
    probe = root / "probe.py"
    probe.write_text(r'''
import json, os, select, signal, sys, time, tty
from pathlib import Path
tty.setraw(0)
signal.signal(signal.SIGINT, signal.SIG_IGN)
root=Path(sys.argv[1]); label=sys.argv[2]
def record(index, state):
    path=root/(label+".json"); temporary=path.with_suffix(".tmp")
    temporary.write_text(json.dumps(dict(index=index,state=state,pid=os.getpid()))); temporary.replace(path)
record(0,"ready"); os.write(1,("PROBE_"+label).encode()); index=1
while True:
    path=root/(label+"-"+str(index)+".json")
    if not path.exists(): time.sleep(0.005); continue
    command=json.loads(path.read_text()); pending=memoryview(bytes.fromhex(command["wire"]))
    while pending: pending=pending[os.write(1,pending):]
    record(index,"emitted"); received=bytearray(); expected=bytes.fromhex(command["expected"])
    deadline=time.monotonic()+12
    while len(received)<len(expected):
        assert time.monotonic()<deadline,(label,index,received,expected)
        if select.select([0],[],[],0.03)[0]: received.extend(os.read(0,65536))
    assert received==expected,(label,index,received,expected)
    # Check that suppressed or misrouted events did not arrive after the sentinel.
    until=time.monotonic()+0.12
    while time.monotonic()<until:
        if select.select([0],[],[],0.01)[0]: raise AssertionError((label,index,"extra input",os.read(0,65536)))
    record(index,"done"); index+=1
    if command.get("exit"): sys.exit(0)
''')
    client = None
    counters = dict(A=0, B=0)
    def run(action, *args):
        target = [name] if action in ("new", "kill") else ["-s", name]
        result = subprocess.run([BINARY, *action.split(), *target, *map(str,args)], env=env, capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, (action, result.stderr)
        return result.stdout
    def wait(predicate, detail="paste timeout", timeout=10):
        deadline = time.monotonic()+timeout
        while not predicate():
            if client:
                client.read(0.01)
                assert client.child.poll() is None, (client.child.returncode, detail)
            assert time.monotonic()<deadline, detail
            time.sleep(0.005)
    def record(label):
        path=root/(label+".json")
        return json.loads(path.read_text()) if path.exists() else None
    def command(label, wire, expected, exit=False):
        counters[label]+=1; index=counters[label]
        path=root/(label+"-"+str(index)+".json"); temp=path.with_suffix(".tmp")
        temp.write_text(json.dumps(dict(wire=wire.hex(), expected=expected.hex(), exit=exit)));temp.replace(path)
        wait(lambda: record(label)["index"]==index, (label,"not emitted"))
    def done(label):
        wait(lambda: record(label)["index"]==counters[label] and record(label)["state"]=="done", (label,"not done"))
    def mode(enabled):
        wait(lambda: client.private_modes.get(5522)==enabled, ("outer 5522",enabled))
    def policy(enabled):
        config.write_text("clipboard_read="+str(enabled).lower()+"\nremain_on_exit=true\n")
        wait(lambda: tomllib.loads(run("config show"))["settings"]["clipboard_read"]==enabled, "policy reload")
    launch=lambda label: "exec "+shlex.quote(sys.executable)+" -u "+shlex.quote(str(probe))+" "+shlex.quote(str(root))+" "+label
    def attach(label):
        global client
        client=Session(extra_env=env,arguments=("attach",name),lifetime=60)
        client.expect(("PROBE_"+label).encode())
    def detach():
        global client
        client.send(b"\x02d");client.finish(0)
        assert client.output.rfind(OFF)>=0, "5522 was not reset on detach"
        client.close();client=None
    try:
        run("new","--detached")
        left=tomllib.loads(run("pane list","--toml"))["panes"][0]["id"]
        run("pane send-keys","-p",left,"--literal","--enter",launch("A"))
        wait(lambda: record("A"))
        command("A",ON+QUERY,b"\x1b[?5522;4$y");done("A")
        policy(True)
        command("A",QUERY+ON+QUERY,b"\x1b[?5522;2$y\x1b[?5522;1$y");done("A")
        right=int(run("pane split","-p",left,"--command",launch("B")))
        wait(lambda: record("B"));attach("B");mode(False)
        command("B",ON+QUERY,b"\x1b[?5522;1$y");done("B");mode(True)
        # Prefix ownership survives a focus switch before OK is fully framed.
        command("B",b"",EVENT);client.send(OK[:20]);client.read(0.03)
        run("pane select","-p",left);mode(True)
        client.send(OK[20:]+DATA)
        client.read(0.03);assert record("B")["state"]=="emitted"
        client.send(DONE);done("B")
        # The application reads selected MIME data using the terminal's paste grant.
        request=packet("type=read:id=content:loc=primary:pw=c2VjcmV0:name=UGFzdGUgZXZlbnQ=",base64.b64encode(b"text/plain"))
        expected=packet("type=read:status=OK:id=content")+packet("type=read:status=DATA:mime=dGV4dC9wbGFpbg==:id=content",b"AP9h")+packet("type=read:status=DONE:id=content")
        client.output.clear();command("B",request,expected)
        def outer():
            return re.findall(rb"\x1b\]5522;([ -~]*?)\x1b\\",client.output)
        wait(lambda: outer(),"no paste content read")
        fields=dict(item.split(b"=",1) for item in outer()[0].split(b";",1)[0].split(b":"))
        assert fields[b"pw"]==b"c2VjcmV0" and fields[b"name"]==b"UGFzdGUgZXZlbnQ=" and fields[b"loc"]==b"primary"
        identifier=fields[b"id"].decode();client.send(expected.replace(b"id=content",("id="+identifier).encode()));done("B")
        # Local History, help and rename editors suppress host events, then restore mode.
        for keys in (b"\x02[",b"\x02?",b"\x02,"):
            command("A",b"",b"x");client.send(keys);mode(False)
            client.send(EVENT);client.read(0.08);client.send(b"\x1b");mode(True)
            client.send(b"x");done("A")
        # A mode-off pane does not receive unsolicited events or activate host mode.
        command("A",OFF+QUERY,b"\x1b[?5522;2$y");done("A");mode(False)
        command("A",b"",b"x");client.send(EVENT+b"x");done("A")
        command("A",ON+QUERY,b"\x1b[?5522;1$y");done("A");mode(True)
        # Invalid MIME/password sequences are withheld in full.
        command("A",b"",b"x")
        client.send(OK+DATA.replace(b"pw=c2VjcmV0",b"pw=b3RoZXI=")+DONE+b"x");done("A")
        # DECRST+DECSET within the same child read revokes the old partial event.
        client.send(OK);client.read(0.03)
        command("A",OFF+ON+QUERY,b"\x1b[?5522;1$y");done("A")
        command("A",b"",b"x");client.send(DATA+DONE+b"x");done("A")
        # Live disable discards incomplete header and resets the child's mode.
        command("A",b"",b"x");client.send(OK[:20]);client.read(0.03)
        policy(False);mode(False);policy(True)
        client.send(OK[20:]+DATA+DONE+b"x");done("A");mode(False)
        command("A",QUERY+ON+QUERY,b"\x1b[?5522;2$y\x1b[?5522;1$y");done("A");mode(True)
        # A partial event cannot cross attachments, while the live pane mode can.
        client.send(OK);client.read(0.03);detach();attach("A");mode(True)
        command("A",b"",b"x");client.send(DATA+DONE+b"x");done("A")
        # Respawn has a fresh incarnation and an initially disabled mode.
        pid=record("A")["pid"];os.kill(pid,15)
        wait(lambda: next(p for p in tomllib.loads(run("pane list","--toml"))["panes"] if p["id"]==left)["output_complete"])
        counters["R"]=0;run("pane respawn","-p",left,"--command",launch("R"));wait(lambda:record("R"));mode(False)
        command("R",QUERY,b"\x1b[?5522;2$y");done("R")
        command("R",b"",b"x");client.send(EVENT+b"x");done("R");detach()
        # Foreground unnamed sessions share mode synchronization and event routing.
        config.write_text("clipboard_read=true\n")
        counters["F"]=0
        wrapper=root/"shell";wrapper.write_text("#!/bin/sh\n"+launch("F")+"\n");wrapper.chmod(0o700)
        client=Session(shell=str(wrapper),extra_env=dict(env,RUSTMUX_SHELL=str(wrapper)),lifetime=20);client.expect(b"PROBE_F")
        command("F",ON+QUERY,b"\x1b[?5522;1$y");done("F");mode(True)
        command("F",b"",EVENT);client.send(EVENT);done("F")
        command("F",b"",b"x",exit=True);client.send(b"x");done("F")
        client.finish(0)
        assert client.output.rfind(OFF)>=0
        client.close();client=None
    finally:
        if client: client.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)

"""OSC 72 source relay with real pane processes and simulated host gestures."""
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

PREFIX=b"\x1b]72;"; ST=b"\x1b\\"
def packet(metadata,payload=None):
    return PREFIX+metadata.encode()+(b";"+payload if payload is not None else b"")+ST
with tempfile.TemporaryDirectory(prefix="rustmux-drag-source-") as temporary:
    root=Path(temporary);config=root/"rustmux/config.toml";config.parent.mkdir()
    config.write_text("drag_source=false\nremain_on_exit=true\n")
    env=dict(os.environ,XDG_CONFIG_HOME=str(root),XDG_STATE_HOME=str(root/"state"),
             RUSTMUX_SHELL="/bin/sh",PS1="RUSTMUX_READY> ",ENV="",BASH_ENV="")
    name=f"drag-source-{os.getpid()}";probe=root/"probe.py"
    probe.write_text(r'''
import base64, json, os, re, select, signal, sys, time, tty
from pathlib import Path
tty.setraw(0);signal.signal(signal.SIGINT,signal.SIG_IGN)
root=Path(sys.argv[1]);label=sys.argv[2]
def record(index,state):
    path=root/(label+".json");temp=path.with_suffix(".tmp")
    temp.write_text(json.dumps(dict(index=index,state=state,pid=os.getpid())));temp.replace(path)
record(0,"ready");os.write(1,("PROBE_"+label).encode());index=1
while True:
    path=root/(label+"-"+str(index)+".json")
    if not path.exists():time.sleep(0.005);continue
    command=json.loads(path.read_text());pending=memoryview(bytes.fromhex(command["wire"]))
    while pending:pending=pending[os.write(1,pending):]
    record(index,"emitted");expected=bytes.fromhex(command["expected"]);received=bytearray();deadline=time.monotonic()+15
    while len(received)<len(expected):
        assert time.monotonic()<deadline,(label,index,len(received),len(expected),received[-100:])
        if select.select([0],[],[],0.05)[0]:received.extend(os.read(0,4096))
    assert received==expected,(label,index,received[-100:],expected[-100:])
    record(index,"done")
    if command.get("exit"):sys.exit(0)
    index+=1
''')
    client = None
    counters = dict(A=0, B=0)
    def run(action, *args):
        target = [name] if action in ("new","kill") else ["-s", name]
        result = subprocess.run([BINARY,action,*target,*map(str,args)],env=env,capture_output=True,text=True,timeout=8)
        assert result.returncode == 0, (action,result.stderr)
        return result.stdout
    def wait(predicate, detail="timeout", timeout=12):
        deadline=time.monotonic()+timeout
        while not predicate():
            if client:
                client.read(0.01)
                assert client.child.poll() is None,(detail,client.child.returncode,bytes(client.output[-500:]))
            else: time.sleep(0.005)
            assert time.monotonic()<deadline,(detail,bytes(client.output[-500:]) if client else None)
    def record(label):
        path=root/(label+".json")
        return json.loads(path.read_text()) if path.exists() else None
    def command(label, wire=b"", expected=b"", save=False, exit=False):
        counters[label]=counters.get(label,0)+1
        index=counters[label];path=root/(label+"-"+str(index)+".json");temp=path.with_suffix(".tmp")
        temp.write_text(json.dumps(dict(wire=wire.hex(),expected=expected.hex(),save=save,exit=exit)));temp.replace(path)
        wait(lambda: record(label) and record(label)["index"]==index,(label,"not emitted"))
    def done(label):
        wait(lambda: record(label)["index"]==counters[label] and record(label)["state"]=="done",(label,"not done"))

    def packets():
        result=[]
        for body in re.findall(rb"\x1b\]72;([ -~]*?)\x1b\\",client.output):
            meta,_,payload=body.partition(b";")
            result.append((dict(f.split(b"=",1) for f in meta.split(b":")),payload))
        return result
    def outer(kind, **fields):
        def matches():
            return [(p,data) for p,data in packets() if p.get(b"t")==kind.encode()
                    and all(p.get(k.encode())==str(v).encode() for k,v in fields.items())]
        wait(matches,("missing command",kind,fields));return matches()[-1][0][b"i"].decode()
    def clear():client.read(0);client.output.clear()
    def support():
        identifier=outer("q");client.send(packet("t=q:i="+identifier));return identifier
    def policy(enabled):
        config.write_text(f"drag_source={str(enabled).lower()}\nremain_on_exit=true\n")
        wait(lambda:tomllib.loads(run("show-config"))["settings"]["drag_source"]==enabled,"reload")
    def attach():
        global client
        client=Session(extra_env=env,arguments=("attach",name),pixels=(800,480),lifetime=80)
        if tomllib.loads(run("show-config"))["settings"]["drag_source"]:support()
        client.expect(b"PROBE_A");clear()
    def detach():
        global client
        client.send(b"\x02d");client.finish(0);raw=bytes(client.output);client.close();client=None;return raw
    launch=lambda label:"exec "+shlex.quote(sys.executable)+" -u "+shlex.quote(str(probe))+" "+shlex.quote(str(root))+" "+label
    try:
        run("new","--detached");left=tomllib.loads(run("list-panes","--toml"))["panes"][0]["id"]
        run("send-keys","-p",left,"--literal","--enter",launch("A"))
        right=int(run("split-pane","-p",left,"--command",launch("B")))
        wait(lambda:all(record(label) for label in counters));original={label:record(label)["pid"] for label in counters}
        command("A",packet("t=o:o=1:i=7",b"text/plain"),packet("t=E:i=7",b"ENOSYS"));done("A")
        attach();command("A",packet("t=o:o=1:i=7",b"text/plain"),packet("t=E:i=7",b"EPERM"));done("A");assert not packets()
        policy(True);support();clear()
        command("A",packet("t=o:x=1:i=7",b"1:machine-A"));done("A")
        command("B",packet("t=q:i=9"),packet("t=q:i=9"));done("B")
        command("B",b"\x1b[?1000h\x1b[?1006h"+packet("t=o:x=1:i=7",b"1:machine-B"));done("B");b=outer("o",x=1)
        # A gesture outside the active pane never reaches either child. The next
        # command checks there was no stray input before the valid gesture.
        client.send(packet(f"t=o:i={b}:x=0:y=3:X=0:Y=60"));client.read(0.02)
        command("B",expected=b"\x1b[<0;4;3M"+packet("t=o:x=3:y=2:X=35:Y=47:i=7"))
        # Kitty's initial physical gesture has no ID: registration does not
        # set the host's client ID until the child returns its MIME offer.
        gesture=packet("t=o:x=44:y=4:X=445:Y=87")
        # One frontend batch must deliver the translated mouse press before
        # its extracted notification, as required by real Yazi's component.
        client.send(b"\x1b[<0;45;5M"+gesture);done("B")
        # Pre-send binary data in chunks with metadata only in the first packet.
        payload=bytes(range(256))*1024;source=root/"source.bin";source.write_bytes(payload)
        encoded=base64.b64encode(source.read_bytes());wire=packet("t=o:o=1:i=7",b"application/octet-stream")
        for offset in range(0,len(encoded),4096):
            meta="t=p:x=0:i=7:m=1" if offset==0 else "m=1"
            wire+=packet(meta,encoded[offset:offset+4096])
        wire+=packet("m=0",b"")+packet("t=P:x=-1:i=7")
        clear();command("B",wire,packet("t=E:i=7",b"OK"));outer("P",x=-1)
        captured=b"".join(data for fields,data in packets() if fields.get(b"t")==b"p" or b"t" not in fields)
        received=root/"host.bin";received.write_bytes(base64.b64decode(captured,validate=True));assert received.read_bytes()==source.read_bytes()
        assert all(fields[b"i"]==b.encode() for fields,data in packets())
        reply=packet("t=E:i="+b,b"OK")
        client.send(reply[:-1]);client.read(0.02);assert record("B")["state"]!="done"
        client.send(b"\\");done("B")
        # Data requests remain pinned while focus/window/geometry change.
        run("select-pane","-p",left);run("new-window","--name","target")
        target=next(p["id"] for p in tomllib.loads(run("list-panes","--toml"))["panes"] if p["id"] not in (left,right))
        run("join-pane","-p",right,"--to-pane",target)
        command("B",expected=packet("t=e:x=5:y=0:i=7"));client.send(packet("t=e:x=5:y=0:i="+b));done("B")
        clear();command("B",packet("t=e:y=0:i=7:m=1",b"AA")+packet("m=0",b"=="));done("B")
        wait(lambda:len(packets())>=2,"data reply");assert b"".join(data for _,data in packets())==b"AA=="
        command("B",expected=packet("t=e:x=4:y=0:i=7"));client.send(packet("t=e:x=4:y=0:i="+b));done("B")
        # Returning to the first window restores A's registration with a fresh ID.
        clear();run("select-window","-w",1);run("select-pane","-p",left);a=outer("o",x=1);assert a!=b
        command("A",expected=packet("t=o:x=3:y=2:X=35:Y=47:i=7"))
        client.send(packet("t=e:x=5:y=0:i="+b)+packet(f"t=o:i={a}:x=4:y=4:X=45:Y=87"));done("A")
        command("A",packet("t=o:o=1:i=7",b"text/plain"));done("A");clear()
        command("A",expected=packet("t=E:i=7",b"ECANCELED"));policy(False);done("A")
        wait(lambda:packet("t=E:y=-1:i="+a) in client.output and packet("t=o:x=2:i="+a) in client.output,"policy cancellation not flushed")
        policy(True);support();clear()
        # Policy changes clear registrations; the child must register again.
        assert not any(fields.get(b"t")==b"o" for fields,_ in packets())
        command("A",packet("t=o:x=1:i=7"));done("A");fresh=outer("o",x=1);assert fresh!=a
        command("A",expected=packet("t=o:x=3:y=2:X=35:Y=47:i=7"))
        # A later physical gesture can carry the previous MIME offer's ID.
        client.send(packet(f"t=o:i={a}:x=4:y=4:X=45:Y=87"));done("A")
        command("A",packet("t=o:o=1:i=7",b"text/plain"));done("A")
        command("A",expected=packet("t=E:i=7",b"ECANCELED"));raw=detach();done("A")
        assert packet("t=E:y=-1:i="+fresh) in raw and packet("t=o:x=2:i="+fresh) in raw
        attach();command("A",packet("t=o:x=1:i=7"));done("A");latest=outer("o",x=1);assert latest!=fresh
        os.kill(original["A"],15)
        wait(lambda:next(p for p in tomllib.loads(run("list-panes","--toml"))["panes"] if p["id"]==left)["output_complete"],"exit")
        run("respawn-pane","-p",left,"--command",launch("R"));wait(lambda:record("R"));clear()
        command("R",packet("t=o:x=1:i=7"));done("R");replacement=outer("o",x=1);assert replacement!=latest
        command("R",expected=packet("t=o:x=3:y=2:X=35:Y=47:i=7"))
        client.send(packet(f"t=o:i={latest}:x=4:y=4:X=45:Y=87"));done("R")
        command("R",packet("t=E:y=-1:i=7"));done("R")
        wait(lambda:packet("t=E:y=-1:i="+replacement) in client.output,"explicit cancellation lost")
        assert record("R")["pid"]!=original["A"] and record("B")["pid"]==original["B"]
        detach()
        # Foreground exit unregisters even if the child exits immediately.
        config.write_text("drag_source=true\n")
        wrapper=root/"foreground.sh";wrapper.write_text("#!/bin/sh\n"+launch("F")+"\n");wrapper.chmod(0o755)
        client=Session(extra_env=dict(env,RUSTMUX_SHELL=str(wrapper)),pixels=(800,480),lifetime=20)
        support();client.expect(b"PROBE_F");clear()
        command("F",packet("t=o:x=1:i=7"));done("F");identifier=outer("o",x=1)
        command("F",exit=True);done("F");client.finish(0);assert packet("t=o:x=2:i="+identifier) in client.output
        client.close();client=None
    finally:
        if client:client.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)

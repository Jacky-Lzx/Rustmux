"""OSC 72 receiving relay with real pane processes and simulated host drops."""
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
PREFIX=b"\x1b]72;";ST=b"\x1b\\"
def packet(meta,payload=None):return PREFIX+meta.encode()+(b";"+payload if payload is not None else b"")+ST
with tempfile.TemporaryDirectory(prefix="rustmux-drop-target-") as temporary:
    root=Path(temporary);config=root/"rustmux/config.toml";config.parent.mkdir();config.write_text("drop_target=false\nremain_on_exit=true\n")
    env=dict(os.environ,XDG_CONFIG_HOME=str(root),XDG_STATE_HOME=str(root/"state"),RUSTMUX_SHELL="/bin/sh",PS1="RUSTMUX_READY> ",ENV="",BASH_ENV="")
    name=f"drop-target-{os.getpid()}";probe=root/"probe.py"
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
    if command.get("save"): (root/(label+".received")).write_bytes(received)
    record(index,"done")
    if command.get("exit"):sys.exit(0)
    index+=1
''')
    client=None;counters=dict(A=0,B=0)
    def run(action,*args):
        target=[name] if action in ("new","kill") else ["-s",name]
        result=subprocess.run([BINARY,action,*target,*map(str,args)],env=env,capture_output=True,text=True,timeout=8)
        assert result.returncode==0,(action,result.stderr)
        return result.stdout
    def wait(predicate,detail="timeout",timeout=15):
        end=time.monotonic()+timeout
        while not predicate():
            if client:
                client.read(.01);assert client.child.poll() is None,(detail,client.child.returncode,bytes(client.output[-500:]))
            else:time.sleep(.005)
            assert time.monotonic()<end,(detail,bytes(client.output[-500:]) if client else None)
    def record(label):
        p=root/(label+".json");return json.loads(p.read_text()) if p.exists() else None
    def command(label,wire=b"",expected=b"",exit=False,save=False):
        counters[label]=counters.get(label,0)+1;index=counters[label];p=root/(label+"-"+str(index)+".json");tmp=p.with_suffix(".tmp")
        tmp.write_text(json.dumps(dict(wire=wire.hex(),expected=expected.hex(),exit=exit,save=save)));tmp.replace(p)
        wait(lambda:record(label) and record(label)["index"]==index,(label,"not emitted"))
    def done(label):wait(lambda:record(label)["index"]==counters[label] and record(label)["state"]=="done",(label,"not done"))
    def packets():
        result=[]
        for body in re.findall(rb"\x1b\]72;([ -~]*?)\x1b\\",client.output):
            meta,_,data=body.partition(b";");result.append((dict(p.split(b"=",1) for p in meta.split(b":")),data))
        return result
    def outer(kind,**fields):
        def found():return [(f,d) for f,d in packets() if f.get(b"t")==kind.encode() and all(f.get(k.encode())==str(v).encode() for k,v in fields.items())]
        wait(found,("missing outer",kind,fields));return found()[-1][0][b"i"].decode()
    def clear():client.read(0);client.output.clear()
    def support():identifier=outer("q");client.send(packet("t=q:i="+identifier))
    def policy(enabled):
        config.write_text(f"drop_target={str(enabled).lower()}\nremain_on_exit=true\n")
        wait(lambda:tomllib.loads(run("show-config"))["settings"]["drop_target"]==enabled,"reload")
    def attach():
        global client
        client=Session(extra_env=env,arguments=("attach",name),pixels=(800,480),lifetime=80)
        if tomllib.loads(run("show-config"))["settings"]["drop_target"]:support()
        client.expect(b"PROBE_A");clear()
    def detach():
        global client
        client.send(b"\x02d");client.finish(0);raw=bytes(client.output);client.close();client=None;return raw
    def position(kind,identifier,pane="A",mimes=b"text/plain image/png"):
        x=4 if pane=="A" else 44;return packet(f"t={kind}:i={identifier}:x={x}:y=4:X={x*10+5}:Y=87:o=3",mimes)
    def event(kind):return packet(f"t={kind}:x=3:y=2:X=35:Y=47:o=3:m=0:i=7",b"text/plain image/png")
    launch=lambda label:"exec "+shlex.quote(sys.executable)+" -u "+shlex.quote(str(probe))+" "+shlex.quote(str(root))+" "+label
    try:
        run("new","--detached");left=tomllib.loads(run("list-panes","--toml"))["panes"][0]["id"]
        run("send-keys","-p",left,"--literal","--enter",launch("A"));right=int(run("split-pane","-p",left,"--command",launch("B")))
        wait(lambda:all(record(label) for label in counters));original={label:record(label)["pid"] for label in counters}
        command("A",packet("t=r:x=1:i=7"),packet("t=R:i=7:x=1",b"ENOSYS"));done("A")
        attach();command("A",packet("t=r:x=1:i=7"),packet("t=R:i=7:x=1",b"EPERM"));done("A");assert not packets()
        policy(True);support();clear()
        command("A",packet("t=q:i=9"),packet("t=q:i=9"));done("A")
        command("A",packet("t=a:i=7",b"text/plain"));done("A");first=outer("a",m=0);clear()
        command("B",packet("t=a:i=7",b"image/png"));done("B");identifier=outer("a",m=0)
        assert any(data==b"text/plain image/png" for _,data in packets())
        command("A",expected=event("m"));client.send(position("m",identifier));done("A")
        command("A",packet("t=r:x=1:i=7"),packet("t=R:i=7:x=1",b"EPERM"));done("A")
        command("A",expected=packet("t=m:x=-1:y=-1:X=0:Y=0:o=0:i=7",b""));command("B",expected=event("m"))
        client.send(position("m",identifier,"B",None));done("A");done("B")
        clear();command("A",packet("t=m:o=1:i=7",b"text/plain"));done("A");client.read(.05);assert not any(f.get(b"t")==b"m" and f.get(b"o")==b"1" for f,_ in packets())
        command("B",packet("t=m:o=1:i=7",b"image/png"));done("B");assert outer("m",o=1)==identifier
        command("B",expected=event("M"));client.send(position("M",first,"B"));client.read(.02);assert record("B")["state"]!="done"
        client.send(position("M",identifier,"B"));done("B")
        clear();command("B",packet("t=r:x=1:i=7"));done("B");assert outer("r",x=1)==identifier
        run("select-pane","-p",left)
        command("A",packet("t=r:x=1:i=7"),packet("t=R:i=7:x=1",b"EPERM"));done("A")
        payload=bytes(range(256))*1024;source=root/"source.bin";source.write_bytes(payload);encoded=base64.b64encode(source.read_bytes());host=b"";nested=b""
        for offset in range(0,len(encoded),4096):
            more=int(offset+4096<len(encoded))
            meta=f"t=r:x=1:i={identifier}:m={more}" if offset==0 else f"m={more}"
            host+=packet(meta,encoded[offset:offset+4096]);nested+=packet(f"t=r:x=1:m={more}:i=7" if offset==0 else f"m={more}:i=7",encoded[offset:offset+4096])
        host+=packet(f"t=r:x=1:i={identifier}",b"")
        nested+=packet("t=r:x=1:i=7",b"")
        command("B",expected=nested,save=True);client.send(host);done("B")
        received=root/"received.bin";received.write_bytes(base64.b64decode(b"".join(body.partition(b";")[2] for body in re.findall(rb"\x1b\]72;(.*?)\x1b\\",(root/"B.received").read_bytes())),validate=True));assert received.read_bytes()==source.read_bytes()
        clear();command("B",packet("t=r:o=1:i=7"));done("B");assert outer("r",o=1)==identifier;fresh=outer("a",m=0);assert fresh!=identifier
        client.send(packet(f"t=r:x=1:i={identifier}",b"STALE"));client.read(.02)
        command("A",expected=event("m"));client.send(position("m",fresh));done("A")
        clear();command("A",expected=packet("t=m:x=-1:y=-1:X=0:Y=0:o=0:i=7",b""));client.send(packet(f"t=m:x=-1:y=-1:X=0:Y=0:i={fresh}",b""));done("A")
        new_id=outer("a",m=0)
        command("A",expected=event("M"));client.send(position("M",new_id));done("A")
        command("A",packet("t=r:x=1:i=7"));done("A");clear()
        command("A",expected=packet("t=R:i=7:x=1",b"ECANCELED"));policy(False);done("A")
        wait(lambda:packet(f"t=r:o=0:i={new_id}") in client.output and packet(f"t=A:i={new_id}") in client.output,"reload cancellation")
        policy(True);support();clear();assert not any(f.get(b"t")==b"a" for f,_ in packets())
        command("A",packet("t=a:i=7",b"text/plain"));done("A");latest=outer("a",m=0)
        command("A",expected=event("M"));client.send(position("M",latest));done("A")
        command("A",expected=packet("t=R:i=7:x=0",b"ECANCELED"));raw=detach();done("A");assert packet(f"t=r:o=0:i={latest}") in raw and packet(f"t=A:i={latest}") in raw
        attach();command("A",packet("t=a:i=7",b"text/plain"));done("A");latest=outer("a",m=0)
        os.kill(original["A"],15);wait(lambda:next(p for p in tomllib.loads(run("list-panes","--toml"))["panes"] if p["id"]==left)["output_complete"],"old process exit")
        run("respawn-pane","-p",left,"--command",launch("R"));wait(lambda:record("R"));clear()
        command("R",packet("t=a:i=7",b"text/plain"));done("R");replacement=outer("a",m=0);assert replacement!=latest
        command("R",expected=event("M"));client.send(position("M",latest));client.read(.02);assert record("R")["state"]!="done"
        client.send(position("M",replacement));done("R")
        command("R",packet("t=A:i=7"),packet("t=R:i=7:x=0",b"ECANCELED"));done("R")
        wait(lambda:packet(f"t=A:i={replacement}") in client.output,"unregister not flushed")
        assert record("R")["pid"]!=original["A"] and record("B")["pid"]==original["B"]
        detach()
    finally:
        if client:client.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)

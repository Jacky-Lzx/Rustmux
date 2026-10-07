"""OSC 5113 relay: real pane processes, binary fixtures and a simulated host."""
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

PREFIX = b"\x1b]5113;"
ST = b"\x1b\\"
def packet(action, identifier, **fields):
    return PREFIX + ";".join(["ac="+action, "id="+identifier, *[k+"="+str(v) for k,v in fields.items()]]).encode() + ST

def status(identifier, code, **fields):
    return packet("status", identifier, st=base64.b64encode(code.encode()).decode(), **fields)

with tempfile.TemporaryDirectory(prefix="rustmux-file-transfer-") as temporary:
    root = Path(temporary)
    config = root/"rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("file_transfer=false\nclipboard_read=true\nremain_on_exit=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root/"state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"file-transfer-{os.getpid()}"
    probe = root/"probe.py"
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
    if command.get("save"):
        chunks=[]
        for body in re.findall(rb"\x1b\]5113;([ -~]*?)\x1b\\",received):
            fields=dict(item.split(b"=",1) for item in body.split(b";"))
            if fields[b"ac"] in (b"data",b"end_data"):chunks.append(base64.b64decode(fields.get(b"d",b""),validate=True))
        (root/(label+".bin")).write_bytes(b"".join(chunks))
    record(index,"done");index+=1
    if command.get("exit"):sys.exit(0)
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
        return [dict(field.split(b"=",1) for field in body.split(b";"))
                for body in re.findall(rb"\x1b\]5113;([ -~]*?)\x1b\\", client.output)]
    def outer(action, **fields):
        def matches():
            return [p for p in packets() if p[b"ac"]==action.encode() and all(p.get(k.encode())==str(v).encode() for k,v in fields.items())]
        wait(matches,("no outer command",action,fields))
        found=matches();assert len(found)==1,found
        return found[0][b"id"].decode()
    def clear(): client.read(0);client.output.clear()
    def policy(enabled):
        config.write_text(f"file_transfer={str(enabled).lower()}\nclipboard_read=true\nremain_on_exit=true\n")
        wait(lambda: tomllib.loads(run("show-config"))["settings"]["file_transfer"]==enabled,"config reload")
    def attach():
        global client
        client=Session(extra_env=env,arguments=("attach",name),lifetime=80)
        client.expect(b"PROBE_B");clear()
    def detach():
        global client
        client.send(b"\x02d");client.finish(0);raw=bytes(client.output);client.close();client=None
        return raw
    launch=lambda label: "exec "+shlex.quote(sys.executable)+" -u "+shlex.quote(str(probe))+" "+shlex.quote(str(root))+" "+label
    try:
        run("new","--detached")
        left=tomllib.loads(run("list-panes","--toml"))["panes"][0]["id"]
        run("send-keys","-p",left,"--literal","--enter",launch("A"))
        right=int(run("split-pane","-p",left,"--command",launch("B")))
        wait(lambda: all(record(label) for label in counters))
        original={label:record(label)["pid"] for label in counters}
        command("A",packet("send","detached"),status("detached","ENOSYS"));done("A")
        attach();command("A",packet("send","disabled"),status("disabled","EPERM"));done("A");assert not packets()
        policy(True);clear()
        # Identical IDs in two panes receive different outer IDs and replies
        # return to their original processes even after focus and layout changes.
        command("A",packet("send","shared",hint="A"),status("shared","OK"));a=outer("send",hint="A")
        command("B",packet("send","shared",hint="B"),status("shared","OK"));b=outer("send",hint="B");assert a!=b
        run("select-pane","-p",left)
        client.send(status("unknown","OK")+status(b,"OK")+status(a,"OK")[:-1]);client.read(0.02)
        assert record("A")["state"]!="done"
        client.send(b"\\");done("A");done("B")
        run("new-window","--name","target")
        target=next(p["id"] for p in tomllib.loads(run("list-panes","--toml"))["panes"] if p["id"] not in (left,right))
        run("join-pane","-p",left,"--to-pane",target)
        clear();payload=bytes(range(256))*16;data=base64.b64encode(payload).decode()
        path=base64.b64encode("/tmp/中;upload.bin".encode()).decode()
        command("A",packet("file","shared",fid="f1",n=path,sz=4096,prm=420)+packet("end_data","shared",fid="f1",d=data),
                status("shared","STARTED",fid="f1")+status("shared","OK",fid="f1",sz=4096))
        assert outer("file",fid="f1")==a
        wait(lambda: any(p[b"ac"]==b"end_data" for p in packets()),"no upload data")
        upload=next(p for p in packets() if p[b"ac"]==b"end_data")
        assert upload[b"id"]==a.encode() and base64.b64decode(upload[b"d"],validate=True)==payload
        assert next(p for p in packets() if p[b"ac"]==b"file")[b"n"]==path.encode()
        client.send(status(a,"STARTED",fid="f1")+status(a,"OK",fid="f1",sz=4096));done("A")
        # finish has no success acknowledgement in Kitty; errors can still follow.
        clear();command("B",packet("finish","shared"));done("B");assert outer("finish")==b
        clear();metadata=packet("file","download",fid="query",st=base64.b64encode(b"f2").decode(),n=path,ft="regular",sz=4096)
        listing=status("download","OK")+metadata+status("download","OK",n=base64.b64encode(b"/home/host").decode())
        command("B",packet("receive","download",sz=1,hint="B")+packet("file","download",fid="query",n=path),listing);r=outer("receive")
        assert outer("file",fid="query")==r
        client.send(status(r,"OK")+packet("file",r,fid="query",st=base64.b64encode(b"f2").decode(),n=path,ft="regular",sz=4096)+status(r,"OK",n=base64.b64encode(b"/home/host").decode()));done("B")
        # A 256 KiB download exceeds both input queues. Persist the decoded
        # bytes in the real receiving child and compare the resulting artifact.
        clear();expected=packet("data","download",fid="f2",d=data)*64+packet("end_data","download",fid="f2")
        command("B",packet("file","download",fid="f2",n=path,tt="rsync",zip="zlib"),expected,save=True)
        assert outer("file",fid="f2")==r
        client.send(packet("data",r,fid="f2",d=data)*64+packet("end_data",r,fid="f2"));done("B")
        assert (root/"B.bin").read_bytes()==payload*64
        # Clipboard and file transfer leases coexist on the shared host framer.
        clear();query=b"\x1b]5522;type=read:id=clip;Lg==\x1b\\"
        expected_clip=b"\x1b]5522;type=read:status=ENOSYS:id=clip\x1b\\"
        command("A",query,expected_clip)
        wait(lambda: re.search(rb"\x1b\]5522;type=read:id=([^;:]+);Lg==\x1b\\",client.output),"no clipboard query")
        clip=re.search(rb"\x1b\]5522;type=read:id=([^;:]+);Lg==\x1b\\",client.output)[1]
        client.send(b"\x1b]5522;type=read:status=ENOSYS:id="+clip+ST);done("A")
        # Reload revokes staged delivery, cancels host sessions, and does not
        # feed delayed responses to a process using the same application ID.
        clear();command("A",expected=status("shared","CANCELED"));command("B",expected=status("download","CANCELED"))
        policy(False);done("A");done("B");assert outer("cancel",id=a)==a and outer("cancel",id=r)==r
        policy(True);clear();command("A",packet("send","shared",hint="fresh"),status("shared","OK"));fresh=outer("send");assert fresh!=a
        client.send(status(a,"OK")+status(r,"OK")+status(fresh,"OK"));done("A")
        clear();command("A",expected=status("shared","CANCELED"));raw=detach();done("A")
        assert packet("cancel",fresh) in raw
        run("select-window","-w",1);attach();clear()
        # A retained CLI pane gets a fresh process incarnation on respawn.
        command("B",packet("send","retired"),status("retired","OK"));retired=outer("send")
        os.kill(original["B"],15)
        def exited():
            p=next(p for p in tomllib.loads(run("list-panes","--toml"))["panes"] if p["id"]==right)
            return p["exited"] and p["output_complete"]
        wait(exited);run("respawn-pane","-p",right,"--command",launch("R"));wait(lambda: record("R"));clear()
        command("R",packet("send","retired"),status("retired","EPERM"));replacement=outer("send");assert replacement!=retired
        client.send(status(retired,"OK")+status(fresh,"OK")+status(replacement,"EPERM"));done("R")
        assert record("R")["pid"]!=original["B"] and record("A")["pid"]==original["A"]
        detach()
        # The unnamed foreground path uses the same ownership and cleanup.
        config.write_text("file_transfer=true\nclipboard_read=true\n")
        wrapper=root/"foreground.sh";wrapper.write_text("#!/bin/sh\n"+launch("F")+"\n");wrapper.chmod(0o755)
        client=Session(extra_env=dict(env,RUSTMUX_SHELL=str(wrapper)),lifetime=25)
        client.expect(b"PROBE_F");clear()
        command("F",packet("send","local"),status("local","OK"));local=outer("send");client.send(status(local,"OK"));done("F")
        clear();command("F",packet("file","local",fid="f",n=path)+packet("end_data","local",fid="f",d=data),status("local","OK",fid="f"))
        wait(lambda: any(p[b"ac"]==b"end_data" for p in packets()),"no local upload")
        assert all(p[b"id"]==local.encode() for p in packets())
        client.send(status(local,"OK",fid="f"));done("F")
        command("F",packet("finish","local"),exit=True);done("F");client.finish(0)
        assert packet("finish",local) in client.output
        client.close();client=None
    finally:
        if client:client.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)

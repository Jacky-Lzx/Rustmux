"""OSC 5522 read routing through a simulated outer terminal and real child PTYs."""
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
def packet(metadata, payload=b""):
    return PREFIX + metadata.encode() + (b";" + payload if payload else b"") + ST

def query(identifier=None):
    return packet("type=read:loc=primary:name=QXBw" + (":id="+identifier if identifier else ""), b"Lg==")

def reply(identifier, status, payload=b"", mime=False):
    return packet("type=read:status="+status + (":mime=dGV4dC9wbGFpbg==" if mime else "") + (":id="+identifier if identifier else ""), payload)

with tempfile.TemporaryDirectory(prefix="rustmux-rich-clipboard-") as temporary:
    root = Path(temporary)
    config = root/"rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("clipboard_read=false\nremain_on_exit=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root/"state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"rich-clipboard-{os.getpid()}"
    probe = root/"probe.py"
    probe.write_text(r'''
import json, os, select, signal, sys, time, tty
from pathlib import Path
tty.setraw(0)
signal.signal(signal.SIGINT, signal.SIG_IGN)
root=Path(sys.argv[1]); label=sys.argv[2]
def record(index, state):
    path=root/(label+".json")
    temporary=path.with_suffix(".tmp")
    temporary.write_text(json.dumps({"index":index,"state":state,"pid":os.getpid()})); temporary.replace(path)
record(0,"ready")
os.write(1,("PROBE_"+label).encode())
index=1
while True:
    path=root/(label+"-"+str(index)+".json")
    if not path.exists(): time.sleep(0.005); continue
    command=json.loads(path.read_text())
    data=bytes.fromhex(command["wire"]); expected=bytes.fromhex(command["expected"])
    pending=memoryview(data)
    while pending: pending=pending[os.write(1,pending):]
    record(index,"emitted")
    received=bytearray(); deadline=time.monotonic()+12
    while len(received)<len(expected):
        assert time.monotonic()<deadline,(label,index,len(received),len(expected),received[-200:])
        if select.select([0],[],[],0.05)[0]: received.extend(os.read(0,4096))
    assert received==expected,(label,index,received[-200:],expected[-200:])
    record(index,"done");index+=1
    if command.get("exit"): sys.exit(0)
''')
    client = None
    counters = dict(A=0, B=0, C=0)
    def run(action, *args):
        target = [name] if action in ("new", "kill") else ["-s", name]
        result = subprocess.run([BINARY, *action.split(), *target, *map(str, args)], env=env, capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, (action, result.stderr)
        return result.stdout
    def wait(predicate, detail="probe timeout", timeout=8):
        deadline = time.monotonic()+timeout
        while not predicate():
            if client:
                client.read(0.01)
                assert client.child.poll() is None, (client.child.returncode, detail)
            assert time.monotonic()<deadline, detail
            time.sleep(0.005)
    def record(label):
        path = root/(label+".json")
        return json.loads(path.read_text()) if path.exists() else None
    def command(label, wire, expected, exit=False):
        counters[label]+=1
        index=counters[label]
        path=root/(label+"-"+str(index)+".json")
        temp=path.with_suffix(".tmp")
        temp.write_text(json.dumps(dict(wire=wire.hex(), expected=expected.hex(), exit=exit))); temp.replace(path)
        wait(lambda: record(label)["index"]==index, (label, "not emitted"))
    def done(label):
        wait(lambda: record(label)["index"]==counters[label] and record(label)["state"]=="done", (label,"not done"), timeout=12)
    def packets():
        client.read(0.01)
        return re.findall(rb"\x1b\]5522;([ -~]*?)\x1b\\", client.output)
    def outer_id():
        wait(lambda: packets(), "no outer clipboard query")
        found=packets()
        assert len(found)==1, found
        metadata, payload=found[0].split(b";",1)
        fields=dict(field.split(b"=",1) for field in metadata.split(b":"))
        assert fields[b"type"]==b"read" and fields[b"loc"]==b"primary" and payload==b"Lg=="
        assert fields[b"name"]==b"QXBw"
        client.output.clear()
        return fields[b"id"].decode()
    def clear():
        client.read(0.01);client.output.clear()
    def attach():
        global client
        client=Session(extra_env=env, arguments=("attach",name), lifetime=40)
        client.expect(b"PROBE_B");clear()
    def detach():
        global client
        client.send(b"\x02d"); client.finish(0); client.close(); client=None
    def policy(enabled):
        config.write_text("clipboard_read="+str(enabled).lower()+"\nremain_on_exit=true\n")
        wait(lambda: tomllib.loads(run("config show"))["settings"]["clipboard_read"]==enabled, "config reload")
        # config show reads the session's committed configuration.
    launch=lambda label: "exec "+shlex.quote(sys.executable)+" -u "+shlex.quote(str(probe))+" "+shlex.quote(str(root))+" "+label
    try:
        run("new","--detached")
        left=tomllib.loads(run("pane list","--toml"))["panes"][0]["id"]
        run("pane send-keys","-p",left,"--literal","--enter",launch("A"))
        right=int(run("pane split","-p",left,"--command",launch("B")))
        run("window new","--name","other","--command",launch("C"))
        other=next(p["id"] for p in tomllib.loads(run("pane list","--toml"))["panes"] if p["id"] not in (left,right))
        wait(lambda: all(record(label) for label in counters))
        original={label:record(label)["pid"] for label in counters}
        # Detached reads fail locally, and are never saved for a later attachment.
        command("C",query("detached"),reply("detached","ENOSYS"));done("C")
        run("window select","-w",1);attach()
        command("A",query("disabled"),reply("disabled","EPERM"));done("A");assert not packets()
        policy(True);clear()
        # B is foreground while A requests; changing windows before replies must
        # not move clipboard data away from A. Exercise missing application IDs.
        payload=base64.b64encode(b"\x00\xffbinary\n")
        command("A",query(),reply(None,"OK")+reply(None,"DATA",payload,True)+reply(None,"DONE"))
        first=outer_id()
        command("B",query("same"),reply("same","EBUSY"));done("B");assert not packets()
        run("window select","-w",2)
        run("pane join","-p",left,"--to-pane",other)
        # Moving the source to a different window preserves its request ownership.
        # Wrong IDs, unsolicited packets and status=DATA before OK are swallowed.
        client.send(reply("unknown","EPERM")+reply(None,"DATA",payload,True)+reply(first,"DATA",payload,True))
        client.send(reply(first,"OK")+reply(first,"DATA",payload,True)[:-1])
        client.read(0.05); assert record("A")["state"]!="done"
        client.send(b"\\"+reply(first,"DONE"));done("A")
        assert record("B")["state"]=="done"
        clear(); command("B",query("same"),reply("same","EPERM"));second=outer_id();assert second!=first
        client.send(reply(first,"DONE")+reply(second,"EPERM"));done("B")
        # Replies larger than either 64 KiB input queue must stream in order.
        clear();chunk=base64.b64encode(bytes(range(256))*16)
        expected=reply("large","OK")+reply("large","DATA",chunk,True)*64+reply("large","DONE")
        command("C",query("large"),expected);third=outer_id()
        client.send(reply(third,"OK")+reply(third,"DATA",chunk,True)*64+reply(third,"DONE"));done("C")
        # Live disable cancels the outstanding read and discards delayed data.
        clear();command("A",query("reload"),reply("reload","EPERM"));old=outer_id()
        policy(False);done("A");client.send(reply(old,"OK")+reply(old,"DATA",payload,True)+reply(old,"DONE"))
        policy(True);clear()
        # Disconnect cancels a request; a new attachment cannot accept its old ID.
        command("B",query("disconnect"),reply("disconnect","EBUSY"));old=outer_id()
        detach();done("B")
        run("window select","-w",1);attach()
        command("A",query("new"),reply("new","ENOSYS"));fresh=outer_id();assert fresh!=old
        client.send(reply(old,"OK")+reply(fresh,"ENOSYS"));done("A")
        assert all(record(label)["pid"]==original[label] for label in counters)
        # A retained pane's CLI identity survives respawn, but its old request
        # must never enter the replacement process.
        clear();command("C",query("retired"),reply("retired","OK"));retired=outer_id()
        os.kill(original["C"],15)
        def exited():
            pane=next(p for p in tomllib.loads(run("pane list","--toml"))["panes"] if p["id"]==other)
            return pane["exited"] and pane["output_complete"]
        wait(exited)
        counters["R"]=0
        run("pane respawn","-p",other,"--command",launch("R"))
        wait(lambda: record("R"));clear()
        command("R",query("replacement"),reply("replacement","ENOSYS"));replacement=outer_id()
        client.send(reply(retired,"OK")+reply(retired,"DATA",payload,True)+reply(retired,"DONE")+reply(replacement,"ENOSYS"));done("R")
        assert record("R")["pid"]!=original["C"]
        detach()
        # Unnamed foreground sessions use the same routing without a server socket.
        config.write_text("clipboard_read=true\n")
        counters["F"]=0
        wrapper=root/"foreground.sh"
        wrapper.write_text("#!/bin/sh\n"+launch("F")+"\n");wrapper.chmod(0o755)
        client=Session(extra_env=dict(env,RUSTMUX_SHELL=str(wrapper)),lifetime=20)
        client.expect(b"PROBE_F");clear()
        command("F",query("local"),reply("local","OK")+reply("local","DONE"),exit=True);local=outer_id()
        client.send(reply(local,"OK")+reply(local,"DONE"));done("F")
        client.finish(0);client.close();client=None
    finally:
        if client: client.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)

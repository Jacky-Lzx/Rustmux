"""Completion desktop messages are bounded OSC 99, filtered and attachment-local."""
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

with tempfile.TemporaryDirectory(prefix="rustmux-desktop-") as temporary:
    root=Path(temporary)
    config=root/"rustmux/config.toml"
    config.parent.mkdir()
    env=dict(os.environ,XDG_CONFIG_HOME=str(root),XDG_STATE_HOME=str(root/"state"),
             RUSTMUX_SHELL="/bin/sh",PS1="RUSTMUX_READY> ",ENV="",BASH_ENV="")
    name=f"desktop-{os.getpid()}"
    client=None
    probe=root/"probe.py"
    probe.write_text("""import os,sys,time
from pathlib import Path
root,label=sys.argv[1:]
root=Path(root)
os.write(1,b'\\x1b]2;'+ '编译 task'.encode()+b'\\x1b\\\\\\x1b]133;C\\x1b\\\\'+(label+'_START').encode())
while not (root/(label+'.go')).exists(): time.sleep(0.01)
os.write(1,b'\\x1b]133;D;0\\x1b\\\\'+(label+'_DONE').encode())
""")
    def run(action,*args):
        p=subprocess.run([BINARY,action,"-s",name,*map(str,args)],env=env,capture_output=True,text=True,timeout=8)
        assert p.returncode==0,(action,args,p)
        return p.stdout
    def panes(): return tomllib.loads(run("list-panes","--toml"))["panes"]
    def status(): return tomllib.loads(run("show-config"))
    def wait(predicate,detail):
        deadline=time.monotonic()+5
        while not predicate():
            if client: client.read(0.01)
            assert time.monotonic()<deadline,detail
            time.sleep(0.01)
    def write(desktop=True,bell=False,enabled=True,excluded=None):
        config.write_text("remain_on_exit=true\n[notifications]\ncommand_duration_seconds=1\n"
                          +f"desktop={str(desktop).lower()}\nlong_command_bell={str(bell).lower()}\nenabled={str(enabled).lower()}\n"
                          +"exclude_applications="+json.dumps(excluded or [])+"\n")
        if client is not None or (root/"started").exists():
            wait(lambda: status()["settings"]["desktop_notifications"]==desktop
                 and status()["settings"]["long_command_bell"]==bell
                 and status()["settings"]["notifications_enabled"]==enabled
                 and status()["settings"]["notification_excluded_applications"]==(excluded or [])
                 and not status().get("error"),"configuration did not apply")
    def launch(label):
        command="python3 -u "+shlex.quote(str(probe))+" "+shlex.quote(str(root))+" "+label
        run("send-keys","-p",first,"--literal","--enter",command)
        wait(lambda: label+"_START" in run("capture-pane","-p",first),"probe start not parsed")
        return time.monotonic()
    def messages():
        return re.findall(rb"\x1b\]99;([^;]*);([^\x1b]*)\x1b\\",bytes(client.output))
    def complete(label,started,desktop,bell=False,short=False):
        if not short: wait(lambda: time.monotonic()-started>=1.15,"threshold")
        if client:
            client.read(0.05)
            client.output.clear()
        (root/(label+".go")).touch()
        wait(lambda: label+"_DONE" in run("capture-pane","-p",first),"probe did not finish")
        if client:
            client.read(0.1)
            assert (b"\x07" in client.output)==bell,(label,bytes(client.output))
            packets=messages()
            assert len(packets)==(2 if desktop else 0),(label,packets)
            if desktop:
                attrs=[dict(pair.split(b"=",1) for pair in meta.split(b":")) for meta,_ in packets]
                assert attrs[0][b"i"]==attrs[1][b"i"]
                assert attrs[0][b"d"]==b"0" and attrs[1][b"d"]==b"1"
                assert attrs[0][b"a"]==b"-focus" and attrs[0][b"c"]==b"0"
                assert attrs[0][b"e"]==attrs[1][b"e"]==b"1"
                assert base64.b64decode(packets[0][1])==b"rustmux: command finished"
                body=base64.b64decode(packets[1][1]).decode()
                assert f"Window work / pane {first} (编译 task) completed in " in body,body
                return attrs[0][b"i"]
    def detach():
        global client
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client=None
    try:
        write()
        client=Session(extra_env=env,arguments=("new",name))
        client.expect(b"RUSTMUX_READY>")
        (root/"started").touch()
        first=next(p["id"] for p in panes() if p["active"])
        run("rename-window","work")
        watcher=int(run("new-window","--name","watcher"))
        client.expect(b"RUSTMUX_READY>")
        original={p["id"]:p["pid"] for p in panes()}
        started=launch("desktop_only")
        first_id=complete("desktop_only",started,True)
        write(bell=True)
        started=launch("both")
        assert complete("both",started,True,True)!=first_id
        started=launch("short")
        complete("short",started,False,short=True)
        started=launch("inflight_off")
        write(desktop=False,bell=True)
        complete("inflight_off",started,False,True)
        detach()

        client=Session(extra_env=env,arguments=("attach",name))
        client.expect(b"RUSTMUX_READY>")
        started=launch("inflight_on")
        write()
        complete("inflight_on",started,True)
        excluded=list({value.casefold():value for value in ["python","python3",Path(sys.executable).name]}.values())
        write(excluded=excluded)
        started=launch("excluded")
        complete("excluded",started,False)
        write(enabled=False)
        started=launch("disabled")
        complete("disabled",started,False)
        before=status()["settings"]
        config.write_text("[notifications]\ndesktop=1\n")
        wait(lambda: status().get("error") is not None,"invalid desktop option not rejected")
        assert status()["settings"]==before
        run("select-pane","-p",first)
        run("select-pane","-p",watcher)
        wait(lambda: b"[!]" not in client.physical_rows[0],"old activity was not cleared")
        detach()

        write()
        started=launch("detached")
        complete("detached",started,False)
        client=Session(extra_env=env,arguments=("attach",name))
        client.expect(b"RUSTMUX_READY>")
        client.read(0.1)
        assert not messages(),"detached completion was replayed on attach"
        wait(lambda: b"[!]" in client.physical_rows[0],"detached completion lost its activity marker")
        assert all(next(p["pid"] for p in panes() if p["id"]==pane)==pid for pane,pid in original.items())
        detach()
    finally:
        if client: client.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)
print("desktop reminders: encoding, independent bell, thresholds, live policy, filtering, invalid isolation and detach passed")

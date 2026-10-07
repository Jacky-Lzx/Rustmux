"""Installed Yazi writes an actual dropped file through the receiving relay."""
import base64
import hashlib
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import time
from urllib.parse import quote
from terminal_loop_support import Session

PREFIX=b"\x1b]72;";ST=b"\x1b\\"
def packet(meta,payload=None):
    return PREFIX+meta.encode()+(b";"+payload if payload is not None else b"")+ST

def packets(data):
    result=[]
    for body in re.findall(rb"\x1b\]72;(.*?)\x1b\\",data):
        meta,_,payload=body.partition(b";")
        result.append((dict(p.split(b"=",1) for p in meta.split(b":")),payload))
    return result

def main(source_enabled=False):
    yazi=shutil.which("yazi");assert yazi,"an installed Yazi is required"
    with tempfile.TemporaryDirectory(prefix="rustmux-yazi-drop-") as temporary:
        root=Path(temporary);config=root/"rustmux/config.toml";config.parent.mkdir();config.write_text("drop_target=true\ndrag_source="+str(source_enabled).lower()+"\n")
        destination=root/"received";destination.mkdir();source=root/"drag-test.txt";source.write_bytes(bytes(range(256))*512)
        client=Session(extra_env=dict(XDG_CONFIG_HOME=str(root),YAZI_CONFIG_HOME=str(root/"yazi"),TERM="xterm-kitty"),pixels=(800,480),lifetime=45)
        def wait(predicate,detail,timeout=12):
            end=time.monotonic()+timeout
            while not predicate():
                client.read(.05)
                assert client.child.poll() is None,(detail,client.child.returncode)
                assert time.monotonic()<end,(detail,packets(client.output),client.last_rows)
        def command(kind,**fields):
            return next((f for f,_ in packets(client.output) if f.get(b"t")==kind.encode() and all(f.get(k.encode())==str(v).encode() for k,v in fields.items())),None)
        try:
            wait(lambda:sum(f.get(b"t")==b"q" for f,_ in packets(client.output))==(2 if source_enabled else 1),"independent capability probes")
            client.send(b"".join(packet("t=q:i="+f[b"i"].decode()) for f,_ in packets(client.output) if f.get(b"t")==b"q"))
            client.expect(b"RUSTMUX_READY>");client.output.clear()
            client.send(("exec "+shlex.quote(yazi)+" "+shlex.quote(str(destination))+"\n").encode())
            wait(lambda:command("a",m=0),"Yazi receiving registration");identifier=command("a",m=0)[b"i"].decode()
            wait(lambda:any(b"No items" in row for row in client.last_rows),"empty Yazi directory")
            client.output.clear();client.send(packet(f"t=m:i={identifier}:x=30:y=10:X=305:Y=207:o=1",b"text/uri-list"))
            caption=b"Drop to copy here"
            wait(lambda:any(caption in row for row in client.physical_rows),"Yazi drop area")
            row=next(i for i,text in enumerate(client.physical_rows) if caption in text)
            client.output.clear();client.send(packet(f"t=m:i={identifier}:x=30:y={row}:X=305:Y={row*20+7}:o=1"))
            wait(lambda:command("m",o=1),"Yazi accepting copy")
            client.output.clear();client.send(packet(f"t=M:i={identifier}:x=30:y={row}:X=305:Y={row*20+7}:o=1",b"text/uri-list"))
            wait(lambda:command("r",x=1),"Yazi URI data request")
            uri=("file://"+quote(str(source))).encode();encoded=base64.b64encode(uri)
            client.send(packet(f"t=r:i={identifier}:x=1:m=1",encoded[:10]))
            client.send(packet("m=0",encoded[10:]))
            client.send(packet(f"t=r:i={identifier}:x=1:m=0",b""))
            received=destination/source.name
            wait(lambda:received.exists(),"Yazi completed real file copy")
            wait(lambda:command("r",o=1),"Yazi drop completion")
            assert received.read_bytes()==source.read_bytes()
            print("PASS: drag_source="+str(source_enabled)+"; installed Yazi receives a split URI list, completes its copy, and writes a matching 128 KiB file; SHA-256="+hashlib.sha256(received.read_bytes()).hexdigest())
            client.send(b"q");client.finish(0)
        finally:client.close()
if __name__=="__main__":
    main()
    main(True)

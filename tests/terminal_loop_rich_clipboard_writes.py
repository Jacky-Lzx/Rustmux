"""Rich writes: real child PTYs with a simulated OSC 5522 outer terminal."""
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

def begin(identifier="app"):
    return packet("type=write:loc=primary:name=QXBw" + (":id=" + identifier if identifier else ""))

def data(mime, payload):
    return packet("type=wdata:mime=" + base64.b64encode(mime.encode()).decode(), payload)

def reply(identifier, status):
    return packet("type=write:status=" + status + (":id=" + identifier if identifier else ""))

END = packet("type=wdata")
with tempfile.TemporaryDirectory(prefix="rustmux-rich-writes-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("clipboard_read=true\nclipboard_write=false\nremain_on_exit=true\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"rich-writes-{os.getpid()}"
    probe = root / "probe.py"
    probe.write_text(r'''
import json, os, select, sys, time, tty
from pathlib import Path
tty.setraw(0)
root=Path(sys.argv[1]);label=sys.argv[2]
def record(index,state):
    path=root/(label+".json");temp=path.with_suffix(".tmp")
    temp.write_text(json.dumps(dict(index=index,state=state,pid=os.getpid())));temp.replace(path)
record(0,"ready");os.write(1,("PROBE_"+label).encode());index=1
while True:
    path=root/(label+"-"+str(index)+".json")
    if not path.exists():time.sleep(0.005);continue
    command=json.loads(path.read_text());parts=command["parts"]
    for i,part in enumerate(parts):
        pending=memoryview(bytes.fromhex(part))
        while pending:pending=pending[os.write(1,pending):]
        if i+1<len(parts):
            record(index,"paused")
            while not (root/(label+"-release-"+str(index))).exists():time.sleep(0.005)
    record(index,"emitted");expected=bytes.fromhex(command["expected"]);received=bytearray();deadline=time.monotonic()+15
    while len(received)<len(expected):
        assert time.monotonic()<deadline,(label,index,received[-200:],expected[-200:])
        if select.select([0],[],[],0.05)[0]:received.extend(os.read(0,4096))
    assert received==expected,(label,index,received[-200:],expected[-200:])
    record(index,"done");index+=1
    if command.get("exit"):sys.exit(0)
''')
    client = None
    counters = dict(A=0, B=0)
    def run(action, *args):
        target = [name] if action in ("new", "kill") else ["-s", name]
        result = subprocess.run([BINARY, action, *target, *map(str, args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, (action, result.stderr)
        return result.stdout
    def wait(predicate, detail="timeout", timeout=12):
        deadline = time.monotonic() + timeout
        while not predicate():
            if client:
                client.read(0.01)
                assert client.child.poll() is None, (client.child.returncode, detail)
            assert time.monotonic() < deadline, detail
            time.sleep(0.005)
    def record(label):
        path = root / (label + ".json")
        return json.loads(path.read_text()) if path.exists() else None
    def command(label, parts, expected, exit=False):
        counters[label] += 1
        index = counters[label]
        path = root / (label + "-" + str(index) + ".json")
        temp = path.with_suffix(".tmp")
        temp.write_text(json.dumps(dict(parts=[p.hex() for p in parts], expected=expected.hex(), exit=exit)))
        temp.replace(path)
        wait(lambda: record(label)["index"] == index, (label, "not emitted"))
    def release(label):
        (root / (label + "-release-" + str(counters[label]))).touch()
    def done(label):
        wait(lambda: record(label)["index"] == counters[label] and record(label)["state"] == "done", (label, "not done"))
    def packets():
        client.read(0.01)
        result = []
        for raw in re.findall(rb"\x1b\]5522;([ -~]*?)\x1b\\", client.output):
            metadata, separator, payload = raw.partition(b";")
            fields = dict(field.split(b"=", 1) for field in metadata.split(b":"))
            result.append((fields, payload))
        return result
    def clear():
        client.read(0.02)
        client.output.clear()
    def complete():
        wait(lambda: any(f[b"type"] == b"wdata" and b"mime" not in f for f, _ in packets()), "no outer end")
        received = packets()
        assert received[0][0][b"type"] == b"write", received[:2]
        identifier = received[0][0][b"id"].decode()
        assert received[0][0][b"loc"] == b"primary" and received[0][0][b"name"] == b"QXBw"
        values, aliases = {}, {}
        for fields, payload in received[1:]:
            assert fields[b"id"].decode() == identifier
            if b"mime" not in fields:
                assert fields[b"type"] == b"wdata" and not payload
                continue
            mime = base64.b64decode(fields[b"mime"], validate=True).decode()
            if fields[b"type"] == b"walias":
                for alias in base64.b64decode(payload, validate=True).decode().split(" "):
                    aliases[alias] = mime
            else:
                decoded = base64.b64decode(payload, validate=True)
                assert len(decoded) <= 4096
                values.setdefault(mime, bytearray()).extend(decoded)
        clear()
        return identifier, values, aliases
    def policy(write, read=False):
        config.write_text(f"clipboard_read={str(read).lower()}\nclipboard_write={str(write).lower()}\nremain_on_exit=true\n")
        wait(lambda: tomllib.loads(run("show-config"))["settings"]["clipboard_write"] == write, "policy reload")
    def attach():
        global client
        client = Session(extra_env=env, arguments=("attach", name), lifetime=80)
        client.expect(b"PROBE_B")
        clear()
    def detach():
        global client
        client.send(b"\x02d")
        client.finish(0)
        output = bytes(client.output)
        client.close()
        client = None
        return output
    launch = lambda label: "exec " + shlex.quote(sys.executable) + " -u " + shlex.quote(str(probe)) + " " + shlex.quote(str(root)) + " " + label
    try:
        run("new", "--detached")
        left = tomllib.loads(run("list-panes", "--toml"))["panes"][0]["id"]
        run("send-keys", "-p", left, "--literal", "--enter", launch("A"))
        right = int(run("split-pane", "-p", left, "--command", launch("B")))
        wait(lambda: all(record(label) for label in counters))
        command("A", [begin("detached") + data("text/plain", b"YQ==") + END], reply("detached", "ENOSYS"))
        done("A")
        attach()
        command("A", [begin("disabled") + END], reply("disabled", "EPERM"))
        done("A")
        assert not packets()
        policy(True, True)
        clear()
        # Stage in background A, contend from focused B, and finish after moving A.
        payload = b"\x00\xffbinary\n"
        encoded = base64.b64encode(payload)
        command("A", [begin() + data("text/plain", encoded[:3]), data("text/plain", encoded[3:]) + data("text/html", b"PGI+PC9iPg==") + packet("type=walias:mime=dGV4dC9wbGFpbg==", base64.b64encode(b"text/x-alias")) + END], reply("app", "DONE"))
        client.read(0.1)
        assert not packets(), "staging leaked before end"
        command("B", [begin("other") + data("text/plain", b"YmFk") + END], reply("other", "EBUSY"))
        done("B")
        query = packet("type=read:id=read", b"Lg==")
        command("B", [query], packet("type=read:status=EBUSY:id=read"))
        done("B")
        run("new-window", "--name", "moved")
        other = next(p["id"] for p in tomllib.loads(run("list-panes", "--toml"))["panes"] if p["id"] not in (left, right))
        run("join-pane", "-p", left, "--to-pane", other)
        release("A")
        identifier, values, aliases = complete()
        assert values == {"text/plain": payload, "text/html": b"<b></b>"}
        assert aliases == {"text/x-alias": "text/plain"}
        client.send(reply("unknown", "DONE") + reply(identifier, "DONE"))
        done("A")
        # Write policy works with read policy off; empty/missing-ID writes and large payloads.
        policy(True)
        clear()
        command("B", [begin(None) + END], reply(None, "DONE"))
        identifier, values, aliases = complete()
        assert not values and not aliases
        client.send(reply(identifier, "DONE"))
        done("B")
        large = bytes(range(256)) * 512
        wire = begin("large") + b"".join(data("application/octet-stream", base64.b64encode(large[i:i+4095])) for i in range(0, len(large), 4095)) + END
        command("A", [wire], reply("large", "DONE"))
        identifier, values, _ = complete()
        assert values == {"application/octet-stream": large}
        client.send(reply(identifier, "DONE"))
        done("A")
        # Force output backpressure, then cancel after BEGIN and before END.
        blocked = bytes(range(256)) * 8192
        def blocked_wire(tag):
            return begin(tag) + b"".join(data("application/octet-stream", base64.b64encode(blocked[i:i+4095])) for i in range(0, len(blocked), 4095)) + END
        def started():
            wait(lambda: any(f[b"type"] == b"write" for f, _ in packets()), "no outer begin")
            captured = packets()
            assert not any(f[b"type"] == b"wdata" and b"mime" not in f for f, _ in captured), "end raced cancellation"
            return next(f[b"id"].decode() for f, _ in captured if f[b"type"] == b"write")
        clear()
        command("A", [blocked_wire("early")], reply("early", "EPERM"))
        early = started()
        client.send(reply(early, "EPERM"))
        done("A")
        client.read(0.1)
        assert not any(f[b"type"] == b"wdata" and b"mime" not in f for f, _ in packets())
        clear()
        command("A", [blocked_wire("abort")], reply("abort", "EPERM"))
        aborted = started()
        policy(False)
        done("A")
        wait(lambda: any(payload == b"!" for _, payload in packets()), "missing cancellation abort")
        captured = packets()
        assert captured[-1][1] == b"!" and captured[-1][0][b"id"].decode() == aborted
        assert not any(f[b"type"] == b"wdata" and b"mime" not in f for f, _ in captured)
        policy(True)
        clear()
        command("A", [blocked_wire("relay-detach")], reply("relay-detach", "EBUSY"))
        retired = started()
        detached_output = detach()
        done("A")
        assert b";!" + ST in detached_output, "detach did not flush abort"
        assert packet("type=wdata:id=" + retired) not in detached_output
        run("select-window", "-w", 1)
        attach()
        client.send(reply(retired, "DONE"))
        clear()
        # Bad Base64, incomplete padding and canceled framing never reach the terminal.
        for index, bad in enumerate([data("text/plain", b"!"), data("text/plain", b"YQ"), PREFIX + b"type=wdata\x18"]):
            clear()
            tag = "bad" + str(index)
            command("A", [begin(tag) + data("text/plain", b"YWJj") + bad + END], reply(tag, "EINVAL"))
            done("A")
            assert not packets()
        # Disable while collecting; no partial write and no replay when enabled again.
        clear()
        command("A", [begin("reload") + data("text/plain", b"YQ=="), END], reply("reload", "EPERM"))
        client.read(0.1)
        assert not packets()
        policy(False)
        release("A")
        done("A")
        assert not packets()
        policy(True)
        clear()
        # Detach cancels staging and returns EBUSY to the original source.
        command("B", [begin("detach") + data("text/plain", b"YQ=="), END], reply("detach", "EBUSY"))
        detach()
        release("B")
        done("B")
        run("select-window", "-w", 1)
        attach()
        command("B", [begin("fresh") + END], reply("fresh", "ENOSYS"))
        fresh, _, _ = complete()
        assert fresh != identifier
        client.send(reply(identifier, "DONE") + reply(fresh, "ENOSYS"))
        done("B")
        detach()
        # Same implementation in an unnamed foreground session.
        config.write_text("clipboard_write=true\n")
        counters["F"] = 0
        wrapper = root / "foreground.sh"
        wrapper.write_text("#!/bin/sh\n" + launch("F") + "\n")
        wrapper.chmod(0o755)
        client = Session(extra_env=dict(env, RUSTMUX_SHELL=str(wrapper)), lifetime=25)
        client.expect(b"PROBE_F")
        clear()
        command("F", [begin("local") + data("text/plain", b"YQ==") + END], reply("local", "DONE"), exit=True)
        identifier, values, _ = complete()
        assert values == {"text/plain": b"a"}
        client.send(reply(identifier, "DONE"))
        done("F")
        client.finish(0)
        client.close()
        client = None
    finally:
        if client:
            client.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

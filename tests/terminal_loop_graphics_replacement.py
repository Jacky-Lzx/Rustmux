"""A regular placement update must keep its old outer image until publication."""
import base64
import ctypes
import os
import re
import shlex
import sys
import termios
import time
import tty

if "--child" in sys.argv:
    saved = termios.tcgetattr(0)
    tty.setraw(0)
    try:
        os.write(1, b"\x1b[2J\x1b[2;1H\x1b_Ga=T,f=32,s=2,v=2,i=7,p=1,c=2,r=2,C=1,q=2;"
                    b"/wAA//8AAP8A/wD/AP8A/w==\x1b\\\x1b[1;1HREPLACE_READY")
        for index in range(4):
            assert os.read(0, 1) == b"x"
            y = index % 2
            os.write(1, f"\x1b[2;1H\x1b_Ga=p,i=7,p=1,x=0,y={y},w=2,h=1,c=2,r=1,C=1,q=2\x1b\\"
                        f"\x1b[1;1HREPLACE_{index:02d}".encode())
        assert os.read(0, 1) == b"q"
    finally:
        termios.tcsetattr(0, termios.TCSANOW, saved)
    sys.exit(0)

from terminal_loop_support import BINARY, Session

begin, end = b"\x1b[?2026h", b"\x1b[?2026l"
apc = re.compile(rb"\x1b_G([^;\x1b]*)(?:;[^\x1b]*)?\x1b\\")

def controls(command):
    return dict(part.split(b"=", 1) for part in command.split(b",") if b"=" in part)

def run(shared):
    session = Session(pixels=(80, 24), lifetime=30)
    try:
        deadline = time.monotonic() + 8
        while b"\x1b_Gi=31" not in session.output:
            session.read()
            assert time.monotonic() < deadline
        session.send(b"\x1b_Gi=31;OK\x1b\\\x1b[?1;2c")
        if shared:
            deadline = time.monotonic() + 8
            while b"\x1b_Ga=q,t=s" not in session.output:
                session.read()
                assert time.monotonic() < deadline
            session.send(b"\x1b_Gi=32;OK\x1b\\\x1b[?1;2c")
        session.expect(b"RUSTMUX_READY> ")
        command = " ".join(shlex.quote(arg) for arg in (sys.executable, __file__, BINARY, "--child"))
        session.send(command.encode() + b"\n")
        pending = bytearray()
        visible = set()
        uploads = set()
        libc = ctypes.CDLL(None)
        libc.shm_unlink.argtypes = [ctypes.c_char_p]

        def receive(marker, replacing):
            pending.extend(session.expect(marker))
            deadline = time.monotonic() + 8
            while not pending.endswith(end):
                session.read()
                pending.extend(session.output)
                session.output.clear()
                assert time.monotonic() < deadline, pending[-1000:]
            frames = re.findall(re.escape(begin) + rb"(.*?)" + re.escape(end), pending, re.S)
            assert frames, pending[-1000:]
            actions = []
            for frame in frames:
                for match in apc.finditer(frame):
                    fields = controls(match.group(1))
                    action, image = fields.get(b"a"), fields.get(b"i")
                    if image is None or int(image) < 0x80000000:
                        continue
                    image = int(image)
                    actions.append(action)
                    if action in (b"t", b"T"):
                        uploads.add(image)
                        if fields.get(b"t") == b"s":
                            payload = match.group(0).split(b";", 1)[1][:-2]
                            assert libc.shm_unlink(base64.b64decode(payload)) == 0
                    if action in (b"p", b"T"):
                        assert image in uploads
                        visible.add((image, fields.get(b"p")))
                    if action == b"d":
                        visible.discard((image, fields.get(b"p")))
                if uploads:
                    assert visible, "a completed frame exposed an empty image scene"
            if replacing:
                assert b"p" in actions and b"d" in actions, actions
                assert actions.index(b"p") < actions.index(b"d"), actions
                assert b"T" not in actions, actions
                if shared:
                    assert b"t" not in actions, actions
                    assert len(uploads) == 1, uploads
                else:
                    assert b"t" in actions and actions.index(b"t") < actions.index(b"p"), actions
            pending.clear()

        receive(b"REPLACE_READY", False)
        for index in range(4):
            session.send(b"x")
            receive(f"REPLACE_{index:02d}".encode(), True)
        session.send(b"q")
        session.expect(b"RUSTMUX_READY> ")
        session.send(b"exit 0\n")
        session.finish(0)
    finally:
        session.close()

run(False)
run(True)

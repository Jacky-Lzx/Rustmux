"""Exercise regular-file Kitty transfers and ordered child PTY replies."""
import base64
import os
import select
import sys
import tempfile
import termios
import time
import tty
from pathlib import Path


def command(controls, path):
    name = base64.b64encode(os.fsencode(path))
    return b"\x1b_G" + controls.encode() + b";" + name + b"\x1b\\"


with tempfile.TemporaryDirectory(prefix="rustmux-file-child-") as directory:
    path = Path(directory) / "source"
    path.write_bytes(b"xx\x01\x02\x03tail")
    missing = Path(directory) / "missing"
    packet = (
        command("a=q,t=f,i=401,f=24,s=1,v=1,S=3,O=2", path)
        + command("a=T,t=f,i=401,p=4,f=24,s=1,v=1,S=3,O=2,C=1", path)
        + command("a=T,t=f,i=401,p=4,f=100", path)
        + command("a=q,t=f,i=402,f=100", missing)
        + command("a=q,t=f,i=402,f=100", directory)
        + command("a=q,t=f,i=402,f=100,q=2", missing)
        + command("a=q,t=t,i=402,f=100", path)
        + b"\x1b_Ga=p,i=401,p=4,C=1;\x1b\\"
        + b"\x1b_Ga=d,d=I,i=401;\x1b\\\x1b[c"
    )
    expected = (
        b"\x1b_Gi=401;OK\x1b\\"
        b"\x1b_Gi=401,p=4;OK\x1b\\"
        b"\x1b_Gi=401,p=4;EINVAL:invalid image\x1b\\"
        b"\x1b_Gi=402;EBADF:Failed to read image file\x1b\\"
        b"\x1b_Gi=402;EBADF:Failed to read image file\x1b\\"
        b"\x1b_Gi=402;EINVAL:unsupported medium\x1b\\"
        b"\x1b_Gi=401,p=4;OK\x1b\\"
        b"\x1b[?1;0c"
    )
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    reply = bytearray()
    try:
        tty.setraw(fd)
        os.write(1, packet)
        deadline = time.monotonic() + 3
        while b"\x1b[?1;0c" not in reply and time.monotonic() < deadline:
            if select.select([fd], [], [], 0.1)[0]:
                reply.extend(os.read(fd, 1024))
    finally:
        termios.tcsetattr(fd, termios.TCSANOW, saved)
    assert reply == expected, (reply.hex(), expected.hex())
    assert path.read_bytes() == b"xx\x01\x02\x03tail"
    print("CHILD_FILE_TRANSFER_OK", flush=True)

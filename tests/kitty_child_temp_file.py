"""Exercise temporary-file lifecycle and ordered graphics replies in a real pane."""
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
    return b"\x1b_G" + controls.encode() + b";" + base64.b64encode(os.fsencode(path)) + b"\x1b\\"


with tempfile.TemporaryDirectory(prefix="rustmux-temp-child-") as directory:
    root = Path(directory)
    paths = [root / f"tty-graphics-protocol-{name}" for name in ("query", "upload", "short", "png", "quiet", "missing")]
    query, upload, short, png, quiet, missing = paths
    for path in paths[:-1]:
        path.write_bytes(b"\x01\x02\x03")
    ordinary = root / "ordinary"
    ordinary.write_bytes(b"\x04\x05\x06")
    link = root / "tty-graphics-protocol-link"
    link.symlink_to(ordinary)
    packet = (
        command("a=q,t=t,i=411,f=24,s=1,v=1", query)
        + command("a=T,t=t,i=411,p=4,f=24,s=1,v=1,C=1", upload)
        + command("a=T,t=t,i=411,p=4,f=24,s=1,v=1,S=4", short)
        + command("a=T,t=t,i=411,p=4,f=100", png)
        + command("a=q,t=t,i=412,f=24,s=1,v=1,q=2", quiet)
        + command("a=q,t=t,i=412,f=24,s=1,v=1", missing)
        + command("a=q,t=t,i=412,f=24,s=1,v=1,q=1", ordinary)
        + command("a=q,t=t,i=412,f=24,s=1,v=1,q=2", link)
        + b"\x1b_Ga=p,i=411,p=4,C=1;\x1b\\"
        + b"\x1b_Ga=p,i=412,C=1;\x1b\\"
        + b"\x1b_Ga=d,d=I,i=411;\x1b\\\x1b[c"
    )
    expected = (
        b"\x1b_Gi=411;OK\x1b\\"
        b"\x1b_Gi=411,p=4;OK\x1b\\"
        b"\x1b_Gi=411,p=4;EBADF:Failed to read image file\x1b\\"
        b"\x1b_Gi=411,p=4;EINVAL:invalid image\x1b\\"
        b"\x1b_Gi=412;EBADF:Failed to read image file\x1b\\"
        b"\x1b_Gi=411,p=4;OK\x1b\\"
        b"\x1b_Gi=412;ENOENT:image not found\x1b\\"
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
    assert all(not path.exists() for path in paths)
    assert ordinary.read_bytes() == b"\x04\x05\x06"
    assert link.is_symlink()
    print("CHILD_TEMP_FILE_TRANSFER_OK", flush=True)

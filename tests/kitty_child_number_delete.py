"""Check newest-image-number deletion and fallback through a child PTY."""
import os
import re
import select
import sys
import termios
import time
import tty

fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
reply = bytearray()


def read_until(marker, deadline):
    while marker not in reply and time.monotonic() < deadline:
        if select.select([fd], [], [], 0.1)[0]:
            reply.extend(os.read(fd, 512))


try:
    tty.setraw(fd)
    upload = b"\x1b_Ga=t,I=986,f=32,s=1,v=1;AQIDBA==\x1b\\"
    os.write(1, upload + upload)
    deadline = time.monotonic() + 3
    while reply.count(b",I=986;OK\x1b\\") < 2 and time.monotonic() < deadline:
        if select.select([fd], [], [], 0.1)[0]:
            reply.extend(os.read(fd, 512))
    ids = re.findall(rb"\x1b_Gi=(\d+),I=986;OK\x1b\\", reply)
    if len(ids) != 2 or ids[0] == ids[1]:
        raise AssertionError(bytes(reply))
    older, newer = ids
    os.write(1, b"\x1b_Ga=p,I=986,p=1,C=1\x1b\\")
    os.write(1, b"\x1b_Ga=p,i=" + older + b",p=2,C=1\x1b\\")
    os.write(1, b"\x1b_Ga=d,d=n,I=986,p=1\x1b\\")
    os.write(1, b"\x1b_Ga=p,I=986,p=3,C=1\x1b\\")
    os.write(1, b"\x1b_Ga=d,d=N,I=986,p=3\x1b\\")
    os.write(1, b"\x1b_Ga=p,I=986,p=4,C=1\x1b\\")
    os.write(1, b"\x1b_Ga=d,d=N,I=986\x1b\\")
    os.write(1, b"\x1b_Ga=p,I=986,p=5,C=1\x1b\\\x1b[c")
    read_until(b"\x1b[?1;0c", time.monotonic() + 3)
finally:
    termios.tcsetattr(fd, termios.TCSANOW, saved)

expected = b"".join([
    b"\x1b_Gi=" + older + b",I=986;OK\x1b\\",
    b"\x1b_Gi=" + newer + b",I=986;OK\x1b\\",
    b"\x1b_Gi=" + newer + b",I=986,p=1;OK\x1b\\",
    b"\x1b_Gi=" + older + b",p=2;OK\x1b\\",
    b"\x1b_Gi=" + newer + b",I=986,p=3;OK\x1b\\",
    b"\x1b_Gi=" + older + b",I=986,p=4;OK\x1b\\",
    b"\x1b_GI=986,p=5;ENOENT:image not found\x1b\\",
    b"\x1b[?1;0c",
])
if reply == expected:
    print("NUMBER_DELETE_OK", flush=True)
else:
    print("NUMBER_DELETE_BAD:" + reply.hex(), flush=True)
    sys.exit(1)

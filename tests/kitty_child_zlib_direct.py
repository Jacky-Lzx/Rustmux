"""Check compressed direct Kitty uploads and replies through a child PTY."""
import base64
import os
import select
import sys
import termios
import time
import tty
import zlib

fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
reply = bytearray()


def command(controls, data):
    return b"\x1b_G" + controls + b";" + base64.b64encode(data) + b"\x1b\\"


try:
    tty.setraw(fd)
    rgba = zlib.compress(bytes([1, 2, 3, 4]))
    rgb = zlib.compress(bytes([5, 6, 7]))
    bad = bytearray(rgba)
    bad[-1] ^= 1
    os.write(1, command(b"a=T,i=68,p=1,f=32,s=1,v=1,o=z,C=1", rgba))
    os.write(1, command(b"a=t,i=69,f=24,s=1,v=1,o=z", rgb))
    os.write(1, command(b"a=t,i=70,f=32,s=1,v=1,o=z", bad) + b"\x1b[c")
    deadline = time.monotonic() + 3
    while b"\x1b[?1;0c" not in reply and time.monotonic() < deadline:
        if select.select([fd], [], [], 0.1)[0]:
            reply.extend(os.read(fd, 512))
finally:
    termios.tcsetattr(fd, termios.TCSANOW, saved)

expected = (
    b"\x1b_Gi=68,p=1;OK\x1b\\"
    b"\x1b_Gi=69;OK\x1b\\"
    b"\x1b[?1;0c"
)
if reply == expected:
    print("ZLIB_DIRECT_OK", flush=True)
else:
    print("ZLIB_DIRECT_BAD:" + reply.hex(), flush=True)
    sys.exit(1)

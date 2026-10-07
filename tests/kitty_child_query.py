"""Emit a Kitty graphics query from a child PTY and report ordered replies."""
import os
import select
import sys
import termios
import time
import tty

fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
reply = bytearray()
try:
    tty.setraw(fd)
    os.write(1, b"\x1b_Gi=41,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[>q\x1b[>0q\x1b[c")
    deadline = time.monotonic() + 3
    while b"\x1b[?1;0c" not in reply and time.monotonic() < deadline:
        if select.select([fd], [], [], 0.1)[0]:
            reply.extend(os.read(fd, 512))
finally:
    termios.tcsetattr(fd, termios.TCSANOW, saved)

expected = (b"\x1b_Gi=41;OK\x1b\\"
            + b"\x1bP>|rustmux-kitty 0.1.0\x1b\\" * 2
            + b"\x1b[?1;0c")
assert reply == expected, (reply, expected)
print("CHILD_GRAPHICS_REPLY_OK", flush=True)

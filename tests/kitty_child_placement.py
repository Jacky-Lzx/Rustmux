"""Check Kitty placement replies through a child PTY."""
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
    os.write(1, b"\x1b_Ga=t,i=43,f=32,s=1,v=1,q=2;AQIDBA==\x1b\\")
    os.write(1, b"\x1b_Ga=p,i=43,p=9,c=1,r=1,C=1\x1b\\")
    os.write(1, b"\x1b_Ga=p,i=999,p=8,c=1,r=1\x1b\\")
    os.write(1, b"\x1b_Ga=p,i=43,p=10,X=1,c=1,r=1\x1b\\\x1b[c")
    deadline = time.monotonic() + 3
    while b"\x1b[?1;0c" not in reply and time.monotonic() < deadline:
        if select.select([fd], [], [], 0.1)[0]:
            reply.extend(os.read(fd, 512))
finally:
    termios.tcsetattr(fd, termios.TCSANOW, saved)

expected = (
    b"\x1b_Gi=43,p=9;OK\x1b\\"
    b"\x1b_Gi=999,p=8;ENOENT:image not found\x1b\\"
    b"\x1b_Gi=43,p=10;EINVAL:invalid placement\x1b\\"
    b"\x1b[?1;0c"
)
if reply == expected:
    print("CHILD_PLACEMENT_REPLIES_OK", flush=True)
else:
    print("CHILD_PLACEMENT_REPLIES_BAD:" + reply.hex(), flush=True)
    sys.exit(1)

"""Check Kitty column deletion through a child PTY."""
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
    os.write(1, b"\x1b[1;1H\x1b_Ga=T,i=81,p=1,f=32,s=1,v=1,C=1,q=2;AQIDBA==\x1b\\")
    os.write(1, b"\x1b[1;2H\x1b_Ga=T,i=82,p=1,f=32,s=1,v=1,C=1,q=2;AQIDBA==\x1b\\")
    os.write(1, b"\x1b_Ga=d,d=x,x=1\x1b\\")
    os.write(1, b"\x1b[1;1H\x1b_Ga=p,i=81,p=2,C=1\x1b\\")
    os.write(1, b"\x1b_Ga=d,d=X,x=1\x1b\\")
    os.write(1, b"\x1b_Ga=p,i=81,p=3,C=1\x1b\\")
    os.write(1, b"\x1b[1;2H\x1b_Ga=p,i=82,p=2,C=1\x1b\\\x1b[c")
    deadline = time.monotonic() + 3
    while b"\x1b[?1;0c" not in reply and time.monotonic() < deadline:
        if select.select([fd], [], [], 0.1)[0]:
            reply.extend(os.read(fd, 512))
finally:
    termios.tcsetattr(fd, termios.TCSANOW, saved)

expected = (
    b"\x1b_Gi=81,p=2;OK\x1b\\"
    b"\x1b_Gi=81,p=3;ENOENT:image not found\x1b\\"
    b"\x1b_Gi=82,p=2;OK\x1b\\"
    b"\x1b[?1;0c"
)
if reply == expected:
    print("CHILD_COLUMN_DELETE_OK", flush=True)
else:
    print("CHILD_COLUMN_DELETE_BAD:" + reply.hex(), flush=True)
    sys.exit(1)

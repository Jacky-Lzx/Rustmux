"""Check Kitty image-number uploads and assigned-ID replies through a child PTY."""
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
    os.write(1, b"\x1b_Ga=t,i=1,f=32,s=1,v=1,q=2;AQIDBA==\x1b\\")
    os.write(1, b"\x1b_Ga=T,I=13,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\")
    os.write(1, b"\x1b_Ga=t,I=13,f=32,s=1,v=1;AQIDBA==\x1b\\")
    os.write(1, b"\x1b_Ga=p,i=2,p=2,C=1\x1b\\")
    os.write(1, b"\x1b_Ga=p,i=3,p=1,C=1\x1b\\\x1b[c")
    deadline = time.monotonic() + 3
    while b"\x1b[?1;0c" not in reply and time.monotonic() < deadline:
        if select.select([fd], [], [], 0.1)[0]:
            reply.extend(os.read(fd, 512))
finally:
    termios.tcsetattr(fd, termios.TCSANOW, saved)

expected = (
    b"\x1b_Gi=2,I=13,p=1;OK\x1b\\"
    b"\x1b_Gi=3,I=13;OK\x1b\\"
    b"\x1b_Gi=2,p=2;OK\x1b\\"
    b"\x1b_Gi=3,p=1;OK\x1b\\"
    b"\x1b[?1;0c"
)
if reply == expected:
    print("NUMBERED_UPLOAD_OK", flush=True)
else:
    print("NUMBERED_UPLOAD_BAD:" + reply.hex(), flush=True)
    sys.exit(1)

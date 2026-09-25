"""Check chunked Kitty transmit-and-place replies through a child PTY."""
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
    os.write(1, b"\x1b_Ga=T,i=65,p=9,f=32,s=1,v=1,C=1,m=1;AQID\x1b\\")
    os.write(1, b"\x1b_Gm=0;BA==\x1b\\")
    os.write(1, b"\x1b_Ga=T,i=65,p=10,f=32,s=1,v=1,X=1;AQIDBA==\x1b\\")
    os.write(1, b"\x1b_Ga=T,i=65,p=11,f=100;YQ==\x1b\\\x1b[c")
    deadline = time.monotonic() + 3
    while b"\x1b[?1;0c" not in reply and time.monotonic() < deadline:
        if select.select([fd], [], [], 0.1)[0]:
            reply.extend(os.read(fd, 512))
finally:
    termios.tcsetattr(fd, termios.TCSANOW, saved)

expected = (
    b"\x1b_Gi=65,p=9;OK\x1b\\"
    b"\x1b_Gi=65,p=10;EINVAL:invalid placement\x1b\\"
    b"\x1b_Gi=65,p=11;EINVAL:invalid image\x1b\\"
    b"\x1b[?1;0c"
)
if reply == expected:
    print("CHILD_TRANSMIT_PLACE_REPLIES_OK", flush=True)
else:
    print("CHILD_TRANSMIT_PLACE_REPLIES_BAD:" + reply.hex(), flush=True)
    sys.exit(1)

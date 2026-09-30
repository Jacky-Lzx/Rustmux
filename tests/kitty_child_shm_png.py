"""Compressed PNG through real POSIX SHM and ordered child PTY replies."""
import base64
import ctypes
import mmap
import os
import select
import struct
import sys
import termios
import time
import tty
import zlib

libc = ctypes.CDLL(None, use_errno=True)
# macOS shm_open has a variadic mode argument. Declaring it as fixed would
# pass it in the wrong place on Apple Silicon and create unreadable objects.
libc.shm_open.argtypes = [ctypes.c_char_p, ctypes.c_int]
if sys.platform != "darwin":
    libc.shm_open.argtypes = [ctypes.c_char_p, ctypes.c_int, ctypes.c_int]
libc.shm_open.restype = ctypes.c_int
libc.shm_unlink.argtypes = [ctypes.c_char_p]
libc.shm_unlink.restype = ctypes.c_int
names = []


def chunk(tag, data):
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))


def command(controls, data):
    name = f"/rmxpng-{os.getpid()}-{len(names)}".encode()
    fd = libc.shm_open(name, os.O_CREAT | os.O_EXCL | os.O_RDWR, ctypes.c_int(0o600))
    assert fd >= 0, ctypes.get_errno()
    names.append(name)
    try:
        payload = b"xx" + data + b"unselected trailing bytes"
        os.ftruncate(fd, len(payload))
        with mmap.mmap(fd, len(payload), access=mmap.ACCESS_WRITE) as mapping:
            mapping[:] = payload
    finally:
        os.close(fd)
    reopened = libc.shm_open(name, os.O_RDONLY, ctypes.c_int(0))
    assert reopened >= 0, ctypes.get_errno()
    try:
        with mmap.mmap(reopened, len(payload), access=mmap.ACCESS_READ) as mapping:
            assert mapping[:] == payload
    finally:
        os.close(reopened)
    controls += f",t=s,f=100,o=z,S={len(data)},O=2"
    return b"\x1b_G" + controls.encode() + b";" + base64.b64encode(name) + b"\x1b\\"


png = (
    b"\x89PNG\r\n\x1a\n"
    + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(b"\0\x05\x06\x07\xff"))
    + chunk(b"IEND", b"")
)
compressed = zlib.compress(png)
fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
reply = bytearray()
try:
    packet = (
        command("a=q,i=421", compressed)
        + command("a=T,i=421,p=4,C=1", compressed)
        + command("a=T,i=421,p=4", compressed + b"trailing")
        + command("a=T,i=421,p=4", zlib.compress(b"invalid PNG"))
        + command("a=q,i=421,q=1", compressed)
        + command("a=T,i=421,p=4,q=2", compressed[:-1])
        + command("a=t,i=422,q=2", compressed)
        + b"\x1b_Ga=p,i=421,p=4,C=1;\x1b\\"
        + b"\x1b_Ga=p,i=422,p=5,C=1;\x1b\\"
        + b"\x1b_Ga=d,d=I,i=421;\x1b\\\x1b_Ga=d,d=I,i=422;\x1b\\\x1b[c"
    )
    tty.setraw(fd)
    os.write(1, packet)
    deadline = time.monotonic() + 3
    while b"\x1b[?1;0c" not in reply and time.monotonic() < deadline:
        if select.select([fd], [], [], 0.1)[0]:
            reply.extend(os.read(fd, 1024))
    expected = (
        b"\x1b_Gi=421;OK\x1b\\"
        b"\x1b_Gi=421,p=4;OK\x1b\\"
        b"\x1b_Gi=421,p=4;EBADF:Failed to read image file\x1b\\"
        b"\x1b_Gi=421,p=4;EINVAL:invalid image\x1b\\"
        b"\x1b_Gi=421,p=4;OK\x1b\\"
        b"\x1b_Gi=422,p=5;OK\x1b\\"
        b"\x1b[?1;0c"
    )
    assert reply == expected, (reply.hex(), expected.hex())
    for name in names:
        opened = libc.shm_open(name, os.O_RDONLY, ctypes.c_int(0))
        if opened >= 0:
            os.close(opened)
        assert opened == -1, (name, "shared-memory source was not unlinked")
finally:
    termios.tcsetattr(fd, termios.TCSANOW, saved)
    for name in names:
        libc.shm_unlink(name)
print("CHILD_SHM_PNG_OK", flush=True)

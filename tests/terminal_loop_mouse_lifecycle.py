"""Outer-PTY mouse lifecycle integration scenarios."""
import fcntl
import os
import shlex
import signal
import struct
import subprocess
import sys
import termios
import time

from terminal_loop_support import (
    BINARY,
    Session,
)

mouse_probe = r"""
import os, select, time, tty
tty.setraw(0)
def receive(expected):
    data = bytearray()
    end = time.monotonic() + 6
    while len(data) < len(expected):
        assert time.monotonic() < end, repr(data)
        if select.select([0], [], [], 0.1)[0]:
            data.extend(os.read(0, len(expected) - len(data)))
    assert data == expected, repr(data)
for mode, encoding, payload in [
    (1000, 1006, b"\x1b[<0;10;4M\x1b[<0;10;4m"),
    (1002, 1006, b"\x1b[<32;11;5M\x1b[<0;11;5m"),
    (1003, 1006, b"\x1b[<35;12;6M\x1b[<64;12;6M\x1b[<65;12;6M"),
    (1000, 0, b"\x1b[M *$\x1b[M#*$"),
]:
    os.write(1, ("\x1b[?%dh\x1b[?1006%s\x1b[2J\x1b[HMOUSE_%d_%d" % (mode, 'h' if encoding else 'l', mode, encoding)).encode())
    receive(payload)
os.write(1, b"\x1b[?1000l\x1b[2J\x1b[HMOUSE_OFF")
receive(b"x")
os.write(1, b"\x1b[?1003;1006h\x1b[2J\x1b[HMOUSE_EXIT")
receive(b"x")
"""
for terminate in (False, True):
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(("exec python3 -c " + shlex.quote(mouse_probe) + "\n").encode())
        for mode, encoding, payload in [
            (1000, 1006, b"\x1b[<0;11;6M\x1b[<0;11;6m"),
            (1002, 1006, b"\x1b[<32;12;7M\x1b[<0;12;7m"),
            (1003, 1006, b"\x1b[<35;13;8M\x1b[<64;13;8M\x1b[<65;13;8M"),
            (1000, 0, b"\x1b[M +&\x1b[M#+&"),
        ]:
            s.expect(("\r\nMOUSE_%d_%d\r\n" % (mode, encoding)).encode())
            outer_mode = 1003 if mode == 1003 else 1002
            assert s.private_modes.get(outer_mode)
            assert s.private_modes.get(1006, False) == bool(encoding)
            s.send(payload[:3])
            s.send(payload[3:])
        s.expect(b"\r\nMOUSE_OFF\r\n")
        assert b"\x1b[?1000l" not in s.last_frame
        s.send(b"x")
        s.expect(b"\r\nMOUSE_EXIT\r\n")
        if terminate:
            os.kill(s.app_pid, signal.SIGTERM)
            s.finish(128 + signal.SIGTERM)
        else:
            s.send(b"x")
            s.finish(0)
        for mode in (1000, 1002, 1003, 1006):
            assert s.output.rfind(("\x1b[?%dl" % mode).encode()) > s.output.rfind(("\x1b[?%dh" % mode).encode())
    finally:
        s.close()

# Alternate scroll converts vertical wheel reports to cursor keys only while
# the alternate screen is active and the child has not requested mouse tracking.
alternate_scroll_probe = r"""
import os, select, time, tty
tty.setraw(0)
def receive(expected):
    data = bytearray()
    end = time.monotonic() + 6
    while len(data) < len(expected):
        assert time.monotonic() < end, repr(data)
        if select.select([0], [], [], 0.1)[0]:
            data.extend(os.read(0, len(expected) - len(data)))
    assert data == expected, repr(data)
os.write(1, b"\x1b[?1007h\x1b[?1049h\x1b[2J\x1b[HALT_SCROLL_NORMAL")
receive(b"\x1b[A\x1b[B")
os.write(1, b"\x1b[?1h\x1b[2J\x1b[HALT_SCROLL_APP")
receive(b"\x1bOA\x1bOB")
os.write(1, b"\x1b[?1000;1006h\x1b[2J\x1b[HALT_SCROLL_MOUSE")
receive(b"\x1b[<64;10;4M")
os.write(1, b"\x1b[?1000;1006l\x1b[?1l\x1b[?1049l\x1b[?1007l")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(("exec python3 -c " + shlex.quote(alternate_scroll_probe) + "\n").encode())
    s.expect(b"\r\nALT_SCROLL_NORMAL\r\n")
    assert s.private_modes.get(1002)
    assert s.private_modes.get(1006)
    s.send(b"\x1b[<64;11;6M\x1b[<65;11;6M")
    s.expect(b"\r\nALT_SCROLL_APP\r\n")
    s.send(b"\x1b[<64;11;6M\x1b[<65;11;6M")
    s.expect(b"\r\nALT_SCROLL_MOUSE\r\n")
    s.send(b"\x1b[<64;11;6M")
    s.finish(0)
finally:
    s.close()

# Queries and input continue during a batch; intermediate screen text stays hidden.
sync_probe = r"""
import os, select, time, tty
tty.setraw(0)
def receive(expected):
    data = bytearray()
    end = time.monotonic() + 6
    while len(data) < len(expected):
        assert time.monotonic() < end, repr(data)
        if select.select([0], [], [], 0.1)[0]:
            data.extend(os.read(0, len(expected) - len(data)))
    assert data == expected, repr(data)
os.write(1, b"\x1b[2J\x1b[HSYNC_READY")
receive(b"x")
os.write(1, b"\x1b[?2026h\x1b[2J\x1b[HPARTIAL_HIDDEN\x1b[?2026$p\x1b[c\x1b[>c")
receive(b"\x1b[?2026;1$y\x1b[?1;0c\x1b[>0;0;0c")
time.sleep(0.2)
os.write(1, b"\x1b[2J\x1b[HSYNC_COMPLETE\x1b[?2026l")
receive(b"x")
os.write(1, b"\x1b[?2026h\x1b[2J\x1b[HSYNC_TIMEOUT")
receive(b"x")
os.write(1, b"\x1b[?2026$p")
receive(b"\x1b[?2026;2$y")
os.write(1, b"\x1b[?2026h\x1b[2J\x1b[HSYNC_RESIZE")
receive(b"x")
os.write(1, b"\x1b[?2026$p")
receive(b"\x1b[?2026;2$y")
os.write(1, b"\x1b[?2026h\x1b[2J\x1b[HSYNC_EOF")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(("exec python3 -c " + shlex.quote(sync_probe) + "\n").encode())
    s.expect(b"\r\nSYNC_READY\r\n")
    s.send(b"x")
    end = time.monotonic() + 6
    while b"SYNC_COMPLETE" not in s.last_rows:
        s.read()
        assert not any(b"PARTIAL_HIDDEN" in row for rows in s.frames for row in rows)
        assert time.monotonic() < end, "batch did not complete"
    s.expect(b"\r\nSYNC_COMPLETE\r\n")
    s.send(b"x")
    s.expect(b"\r\nSYNC_TIMEOUT\r\n")
    s.send(b"x")
    # Let the child enter another batch, then resize before its timeout.
    s.read(0.15)
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 25, 81, 0, 0))
    s.expect(b"\r\nSYNC_RESIZE\r\n")
    s.send(b"x")
    s.expect(b"\r\nSYNC_EOF\r\n")
    s.finish(0)
finally:
    s.close()

s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"exec python3 -c 'import os,time; os.write(1,b\"\\x1b[?2026h\"); time.sleep(5)'\n")
    s.read(0.2)
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

row_probe = r"""
import os, select, tty
tty.setraw(0)
os.write(1, b"\x1b[2J\x1b[HUNCHANGED_ROW\x1b[2;1HOLD")
assert select.select([0], [], [], 6)[0]
assert os.read(0, 1) == b"x"
os.write(1, b"\x1b[2;1HNEW")
assert select.select([0], [], [], 6)[0]
assert os.read(0, 1) == b"x"
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(("exec python3 -c " + shlex.quote(row_probe) + "\n").encode())
    s.expect(b"\r\nOLD\r\n")
    s.send(b"x")
    s.expect(b"\r\nNEW\r\n")
    assert s.last_rows[1] == b"UNCHANGED_ROW"
    assert b"\x1b[1;1H" not in s.last_frame
    assert b"\x1b[2;1H" not in s.last_frame
    assert b"\x1b[4;2H" in s.last_frame
    assert len(s.last_frame) < 100
    s.send(b"x")
    s.finish(0)
finally:
    s.close()

# A model-allocation limit error during resize must restore the terminal too.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 257, 256, 0, 0))
    s.finish(1)
    assert b"at most 65536 cells" in s.output
finally:
    s.close()

s = Session("/rustmux-no-such-shell")
try:
    s.finish(1)
    assert b"rustmux:" in s.output
    assert b"\x1b[?1049h" not in s.output
finally:
    s.close()

s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
    assert b"\x1b[?1049l" in s.output
finally:
    s.close()

# A running process that closes its PTY triggers the error-return cleanup path.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"exec python3 -c 'import os,time; os.closerange(0,256); time.sleep(5)'\n")
    # Linux reports slave-close promptly; macOS may keep the controlling
    # terminal alive until the session leader exits despite closed stdio.
    s.finish(1 if sys.platform.startswith("linux") else 0)
    if sys.platform.startswith("linux"):
        assert b"shell kept running after PTY closed" in s.output
finally:
    s.close()

# Full output queues must not prevent termination-signal handling.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"exec python3 -c 'import os;\nwhile True: os.write(1, b\"X\" * 65536)'\n")
    s.expect(b"X" * 78)
    time.sleep(0.2)
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

result = subprocess.run([BINARY], stdin=subprocess.DEVNULL, capture_output=True, timeout=5)
assert result.returncode == 1 and b"must be terminals" in result.stderr
print("Nested PTY: Unicode, backspace, Ctrl-C, 200KB output, exit tail, termios, startup failure and SIGTERM passed.")

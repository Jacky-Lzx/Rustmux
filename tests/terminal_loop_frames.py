"""Deterministic replay of graphics-interleaved, sparse terminal frames."""
import struct
from unittest.mock import patch

from terminal_loop_support import Session


def session():
    # Exercise the same reader and row cache as live PTY tests, with byte chunks
    # supplied deterministically instead of depending on process scheduling.
    result = Session.__new__(Session)
    result.master = 0
    result.slave = 0
    result.output = bytearray()
    result.frame_pending = bytearray()
    result.frames = []
    result.physical_rows = []
    result.last_rows = []
    result.last_frame = b""
    result.cursor_shape = None
    result.private_modes = {}
    return result


def feed(result, data):
    with patch("terminal_loop_support.select.select", return_value=([0], [], [])), \
         patch("terminal_loop_support.os.read", return_value=data), \
         patch("terminal_loop_support.fcntl.ioctl", return_value=struct.pack("HHHH", 24, 80, 80, 24)):
        result.read(0)


initial = (b"\x1b[?25l\x1b[0m\x1b[3;2HCHILD_CELL_DELETE_OK"
           b"\x1b[0m\x1b[4;2H\x1b[?25h")
# Like the failed macOS CI frame, repaint only the suffix at column 8. The
# unchanged CHILD_ prefix remains in the row cache from the previous command.
incremental = (b"\x1b[3;2H\x1b_Ga=T,f=32,s=1,v=1,i=2147483651,z=0,C=1,q=2,m=0;AQIDBA==\x1b\\"
               b"\x1b[3;2H\x1b[?25l\x1b[0m\x1b[3;8HZ_CELL_DELETE_OK"
               b"\x1b[4;2HRUSTMUX_READY> \x1b[0m\x1b[4;17H\x1b[?25h")
marker = b"CHILD_Z_CELL_DELETE_OK"
for split in range(len(incremental) + 1):
    result = session()
    feed(result, initial)
    result.output.clear()
    result.frames.clear()
    feed(result, incremental[:split])
    feed(result, incremental[split:])
    assert marker not in result.output, "fixture must require frame reconstruction"
    assert any(marker in row for row in result.last_rows), (split, result.last_rows)
    assert not any(b"AQIDBA==" in row for row in result.last_rows), result.last_rows
    result.expect(marker)
    assert not result.frames and not result.output

result = session()
feed(result, initial)
result.output.clear()
result.frames.clear()
for byte in incremental:
    feed(result, bytes([byte]))
result.expect(marker)

"""Outer-PTY panes integration scenarios."""
import fcntl
import os
import shlex
import signal
import struct
import termios
import tempfile
import time

from terminal_loop_support import (
    Session,
    background_query,
)

# Interactive splits retain all screens, route keys, resize and close only one pane.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"stty -echo; VAR=A; printf '\\033[2J\\033[H%s%s\\n' MARK _A\n")
    s.expect(b"MARK_A")
    s.send(b"\x02%")
    s.expect(b"RUSTMUX_READY>")
    s.send(b"stty -echo; VAR=B; printf '\\033[2J\\033[H%s%s:%s\\n' RIGHT _B \"$(stty size)\"\n")
    s.expect(b"RIGHT_B:20 38")
    assert any(b"MARK_A" in row for row in s.last_rows)
    s.send(b"\x02hprintf '\\033[2J\\033[HLEFT:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"LEFT:A:20 38")
    s.send(b'\x02"')
    s.expect(b"RUSTMUX_READY>")
    s.send(b"stty -echo; VAR=C; printf '\\033[2J\\033[HLOWER:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"LOWER:C:9 38")
    assert any(b"RIGHT_B:20 38" in row for row in s.last_rows)
    s.send(b"\x02kprintf '\\033[2J\\033[HUP:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"UP:A:9 38")
    s.send(b"\x02lprintf '\\033[2J\\033[HFOCUS:%s\\n' $VAR\n")
    s.expect(b"FOCUS:B")
    s.send(b"\x02oprintf '\\033[2J\\033[HCYCLE:%s\\n' $VAR\n")
    s.expect(b"CYCLE:A")
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    s.read(0.1)
    s.send(b"printf '\\033[2J\\033[HRESIZED:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"RESIZED:A:12 48")
    s.send(b"\x02jexit 7\n")
    end = time.monotonic() + 3
    while any(b"LOWER:C" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < end, s.last_rows
    s.send(b"printf '\\033[2J\\033[HAFTER:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"AFTER:B:26 48")
    s.send(b"\x02hprintf '\\033[2J\\033[HRESTORED:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"RESTORED:A:26 48")
    s.send(b"exit 0\n")
    end = time.monotonic() + 3
    while any(b"RESTORED:A" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < end, s.last_rows
    s.send(b"printf '\\033[2J\\033[HLAST_PANE:%s:%s\\n' $VAR \"$(stty size)\"; exit 9\n")
    s.finish(9)
    assert any(b"LAST_PANE:B:26 98" in row for row in s.last_rows)
finally:
    s.close()

# Mouse events target the active right pane in local coordinates; other panes are ignored.
split_mouse = r"""
import os, select, time, tty
tty.setraw(0)
os.write(1, b"\x1b[?1000;1006h\x1b[2J\x1b[HSPLIT_MOUSE_READY")
expected = b'\x1b[<0;1;2Mx'
data = bytearray()
end = time.monotonic() + 4
while len(data) < len(expected):
    assert time.monotonic() < end, repr(data)
    if select.select([0], [], [], 0.1)[0]:
        data.extend(os.read(0, len(expected) - len(data)))
assert data == expected, repr(data)
os.write(1, b"\x1b[?1000;1006l\x1b[2J\x1b[HSPLIT_MOUSE_OK")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"stty -echo; printf '\\033[2J\\033[H%s%s\\n' BASE _READY\n")
    s.expect(b"BASE_READY")
    s.send(b"\x02%")
    s.expect(b"RUSTMUX_READY>")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(split_mouse)
        source.flush()
        s.send(("python3 " + shlex.quote(source.name) + "\n").encode())
        s.expect(b"SPLIT_MOUSE_READY")
        s.send(b'\x1b[<0;42;4M\x1b[<0;1;1M\x1b[<0;1;1m\x1b[M I$x')
        s.expect(b"SPLIT_MOUSE_OK")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

# Clicking an inactive pane focuses it without sending the click to either shell.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"VAR=A\n\x02%")
    s.expect(b"RUSTMUX_READY>")
    s.send(b"VAR=B\n")
    s.expect(b"RUSTMUX_READY>")
    s.send(b"\x1b[M *%\x1b[M#*%printf '\nCLICK_PANE:%s\n' $VAR\n")
    s.expect(b"CLICK_PANE:A")
    s.send(b"\x1b[M f%\x1b[M#f%printf '\nCLICK_PANE:%s\n' $VAR\n")
    s.expect(b"CLICK_PANE:B")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

# Dragging a separator resizes that split and keeps focus on the active pane.
drag_monitor = r"""
import os, signal
count = 0
def on_resize(*_):
    global count
    count += 1
    size = os.get_terminal_size()
    print("DRAG_WINCH:%s:%s:%s" % (count, size.lines, size.columns), flush=True)
signal.signal(signal.SIGWINCH, on_resize)
print("DRAG_MONITOR_READY", flush=True)
while True:
    signal.pause()
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"VAR=A\n\x02%")
    s.expect(b"RUSTMUX_READY>")
    s.send(b"VAR=B\n")
    s.expect(b"RUSTMUX_READY>")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(drag_monitor)
        source.flush()
        s.send(("python3 " + shlex.quote(source.name) + "\n").encode())
        s.expect(b"DRAG_MONITOR_READY")
        # A foreground process reports resize signals directly, independent of
        # when an interactive shell chooses to run its trap.
        s.send(b"\x1b[M H%\x1b[M@M%\x1b[M@W%\x1b[M@R%\x1b[M#R%")
        observed = s.expect(b"DRAG_WINCH:1:20:28")
        end = time.monotonic() + 0.1
        while time.monotonic() < end:
            s.read(max(0, end - time.monotonic()))
        assert b"DRAG_WINCH:2" not in observed + s.output, bytes(s.output[-1000:])
        s.send(b"\x03")
        s.expect(b"RUSTMUX_READY> ")
    s.send(b"printf 'DRAG:%s:%s\n' $VAR \"$(stty size)\"\n")
    s.expect(b"DRAG:B:20 28")
    s.send(b"\x02hprintf 'DRAG:%s:%s\n' $VAR \"$(stty size)\"\n")
    s.expect(b"DRAG:A:20 48")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

# Zoom resizes only its target; changing targets preserves shells and restores tiling.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"stty -echo; VAR=A; printf '\\033[2J\\033[H%s%s\\n' ZOOM _BASE\n")
    s.expect(b"ZOOM_BASE")
    s.send(b"\x02%")
    s.expect(b"RUSTMUX_READY>")
    s.send(b"stty -echo; VAR=B; printf '\\033[2J\\033[H%s%s\\n' ZOOM _RIGHT\n")
    s.expect(b"ZOOM_RIGHT")
    s.send(b"\x02Zprintf '\\033[2J\\033[HZOOM:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"ZOOM:B:20 78")
    assert not any(b"ZOOM_BASE" in row for row in s.last_rows)
    s.send(b"\x02hprintf '\\033[2J\\033[HTARGET:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"TARGET:A:20 78")
    assert not any(b"ZOOM:B" in row for row in s.last_rows)
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    s.read(0.1)
    s.send(b"printf '\\033[2J\\033[HZOOM_RESIZE:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"ZOOM_RESIZE:A:26 98")
    s.send(b"\x02oprintf '\\033[2J\\033[HCYCLE_ZOOM:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"CYCLE_ZOOM:B:26 98")
    s.send(b"\x02Zprintf '\\033[2J\\033[HTILED:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"TILED:B:26 48")
    assert any(b"ZOOM_RESIZE:A" in row for row in s.last_rows)
    s.send(b"\x02hprintf '\\033[2J\\033[HRESTORED_ZOOM:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"RESTORED_ZOOM:A:26 48")
    s.send(b"\x02Zexit 0\n")
    end = time.monotonic() + 3
    while any(b"RESTORED_ZOOM:A" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < end, s.last_rows
    s.send(b"printf '\\033[2J\\033[HZOOM_SURVIVOR:%s:%s\\n' $VAR \"$(stty size)\"; exit 8\n")
    s.finish(8)
    assert any(b"ZOOM_SURVIVOR:B:26 98" in row for row in s.last_rows)
finally:
    s.close()

# Zoomed mouse coordinates have no tiled column offset.
zoom_mouse = split_mouse.replace(
    "expected = b'\\x1b[<0;1;2Mx'",
    "expected = b'\\x1b[<0;2;2M\\x1b[M !\"x'",
)
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"stty -echo; printf '\\033[2J\\033[H%s%s\\n' BASE _READY\n")
    s.expect(b"BASE_READY")
    s.send(b"\x02%\x02Z")
    s.expect(b"RUSTMUX_READY>")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(zoom_mouse)
        source.flush()
        s.send(("python3 " + shlex.quote(source.name) + "\n").encode())
        s.expect(b"SPLIT_MOUSE_READY")
        s.send(b'\x1b[<0;3;4M\x1b[<0;1;1M\x1b[<0;1;1m\x1b[M "$x')
        s.expect(b"SPLIT_MOUSE_OK")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()


# Hidden panes still answer queries and retain output while another pane is zoomed.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(background_query)
        source.flush()
        s.send(("exec python3 " + shlex.quote(source.name) + "\n").encode())
        s.expect(b"QUERY_WAIT")
        s.send(b"\x02%\x02Z")
        s.expect(b"RUSTMUX_READY> ")
        s.read(0.5)
        assert not any(b"QUERY_BG_OK" in row for row in s.last_rows)
        s.send(b"\x02h")
        s.expect(b"QUERY_BG_OK")
        s.send(b"x")
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"exit 0\n")
        s.finish(0)
finally:
    s.close()

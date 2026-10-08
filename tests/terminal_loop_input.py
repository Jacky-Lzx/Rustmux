"""Outer-PTY input integration scenarios."""
import os
import shlex
import signal
import tempfile

from terminal_loop_support import (
    Session,
)

# Query replies must reach the child, including a burst larger than the queue.
probe = r"""
import os, select, threading, time, tty
tty.setraw(0)
def receive(expected):
    data = bytearray()
    end = time.monotonic() + 6
    while len(data) < len(expected):
        assert time.monotonic() < end, (len(data), len(expected))
        if select.select([0], [], [], 0.1)[0]:
            data.extend(os.read(0, len(expected) - len(data)))
    assert data == expected, repr(data[:80])
os.write(1, b"\x1b[2;3H\x1b[5n\x1b[6n\x1b[?6n\x1b[?15n\x1b[?25n")
receive(b"\x1b[0n\x1b[2;3R\x1b[?2;3R\x1b[?11n\x1b[?21n")
# Real child DSR observes sequence widths even when components arrive in
# separate reads. A query/reply between writes forces that boundary.
for base, suffix, base_column in [("👍", "🏽", 3), ("☝", "🏿", 2), ("🇨", "🇳", 2)]:
    os.write(1, ("\x1b[2J\x1b[H" + base + "\x1b[6n").encode())
    receive(f"\x1b[1;{base_column}R".encode())
    os.write(1, (suffix + "\x1b[6n").encode())
    receive(b"\x1b[1;3R")
    os.write(1, b"X\x1b[6n")
    receive(b"\x1b[1;4R")
# DSR between every component verifies incremental ZWJ joining through the
# real child/server parser, rather than only replaying completed UTF-8 strings.
for base, suffix in [("👩", "‍💻"), ("👨", "‍👩‍👧‍👦"), ("🧑", "🏻‍🤝‍🧑🏿"), ("🏳️", "‍🌈")]:
    os.write(1, ("\x1b[2J\x1b[H" + base + "\x1b[6n").encode())
    receive(b"\x1b[1;3R")
    for component in suffix:
        os.write(1, (component + "\x1b[6n").encode())
        receive(b"\x1b[1;3R")
    os.write(1, b"X\x1b[6n")
    receive(b"\x1b[1;4R")
# A pending wrap belongs to the complete emoji, not its next joined component.
os.write(1, "\x1b[2J\x1b[1;77H👩\x1b[6n".encode())
receive(b"\x1b[1;78R")
os.write(1, "‍💻\x1b[6n".encode())
receive(b"\x1b[1;78R")
os.write(1, b"X\x1b[6n")
receive(b"\x1b[2;2R")
# A late flag suffix grows past the 78-column pane edge as one whole glyph.
os.write(1, "\x1b[2J\x1b[1;78H🇨\x1b[6n".encode())
receive(b"\x1b[1;78R")
os.write(1, "🇳\x1b[6n".encode())
receive(b"\x1b[2;3R")
os.write(1, b"\x1b]10;?;?;?\x07")
receive(b"\x1b]10;rgb:cdcd/d6d6/f4f4\x07\x1b]11;rgb:1e1e/1e1e/2e2e\x07\x1b]12;rgb:f5f5/e0e0/dcdc\x07")
os.write(1, b"\x1b]10;#010203;rgb:1111/2222/3333;#a0b0c0\x1b\\\x1b]10;?;?;?\x1b\\")
receive(b"\x1b]10;rgb:0101/0202/0303\x1b\\\x1b]11;rgb:1111/2222/3333\x1b\\\x1b]12;rgb:a0a0/b0b0/c0c0\x1b\\")
os.write(1, b"\x1b]110\x1b\\\x1b]111\x07\x1b]112\x1b\\")
os.write(1, b"\x1b]4;1;?;1;#010203;1;?\x07")
receive(b"\x1b]4;1;rgb:cdcd/0000/0000\x07\x1b]4;1;rgb:0101/0202/0303\x07")
os.write(1, b"\x1b]104;1;2\x07")
os.write(1, b"\x1b[3;10r\x1b[?6h\x1b[2;4H\x1b[6n\x1b[?6n")
receive(b"\x1b[2;4R\x1b[?2;4R")
def flood():
    data = b"\x1b[5n" * 20000
    while data:
        data = data[os.write(1, data):]
writer = threading.Thread(target=flood)
writer.start()
time.sleep(0.1)
receive(b"\x1b[0n" * 20000)
writer.join(timeout=2)
assert not writer.is_alive()
# Mode replies share the same bounded queue with DSR replies and keyboard input.
os.write(1, b"\x1b[4$p\x1b[4h\x1b[4$p\x1b[4l\x1b[?2027$p")
receive(b"\x1b[4;2$y\x1b[4;1$y\x1b[?2027;0$y")
os.write(1, b"\x1b[?2004h\x1b[?2004$p\x1b[?2004l\x1b[?2004$p")
receive(b"\x1b[?2004;1$y\x1b[?2004;2$y")
os.write(1, b"\x1bP$q q\x1b\\\x1b[6 q\x1bP$q q\x1b\\\x1b[1 q\x1bP$qr\x1b\\\x1b[1;38;5;123m\x1bP$qm\x1b\\\x1b[0m")
receive(b"\x1bP1$r1 q\x1b\\\x1bP1$r6 q\x1b\\\x1bP1$r3;10r\x1b\\\x1bP1$r0;1;38;5;123m\x1b\\")
def mode_flood():
    data = b"\x1b[?7$p\x1b[5n\x1b[?15n\x1b[?25n" * 10000
    while data:
        data = data[os.write(1, data):]
writer = threading.Thread(target=mode_flood)
writer.start()
time.sleep(0.1)
receive(b"\x1b[?7;1$y\x1b[0n\x1b[?11n\x1b[?21n" * 10000)
writer.join(timeout=2)
assert not writer.is_alive()
os.write(1, b"\x1b[c\x1b[0c\x1bZ\x1b[>c\x1b[>0c\x1b[=c\x1b[=0c\x1b[>q\x1b[>0q\x1b[18t\x1b[19t\x1b[\"v")
receive(b"\x1b[?1;0c" * 3 + b"\x1b[>0;0;0c" * 2 + b"\x1bP!|00000000\x1b\\" * 2 + b"\x1bP>|rustmux(0.1.0)\x1b\\" * 2 + b"\x1b[8;20;78t\x1b[9;20;78t\x1b[20;78;1;1;1\"w")
def identity_flood():
    data = b"\x1b[c\x1b[>c\x1b[=c\x1b[>q\x1b[18t\x1b[19t\x1b[\"v\x1bP$q q\x1b\\\x1bP$qr\x1b\\\x1bP$qm\x1b\\" * 10000
    while data:
        data = data[os.write(1, data):]
writer = threading.Thread(target=identity_flood)
writer.start()
time.sleep(0.1)
receive(b"\x1b[?1;0c\x1b[>0;0;0c\x1bP!|00000000\x1b\\\x1bP>|rustmux(0.1.0)\x1b\\\x1b[8;20;78t\x1b[9;20;78t\x1b[20;78;1;1;1\"w\x1bP1$r1 q\x1b\\\x1bP1$r3;10r\x1b\\\x1bP1$r0m\x1b\\" * 10000)
writer.join(timeout=2)
assert not writer.is_alive()
# CSI s/u share the DEC save slot, and the restored position reaches real DSR.
os.write(1, b"\x1b[?6l\x1b[2;3H\x1b[s\x1b[7;8H\x1b[u\x1b[6n")
receive(b"\x1b[2;3R")
os.write(1, b"\x1b[3;4H\x1b7\x1b[H\x1b[u\x1b[6n")
receive(b"\x1b[3;4R")
# DEC private mode 1048 shares the same save slot and restores through DECRST.
os.write(1, b"\x1b[4;5H\x1b[?1048h\x1b[8;9H\x1b[?1048l\x1b[6n")
receive(b"\x1b[4;5R")
os.write(1, b"\x1b[?6l\x1b[r\x1b[2J\x1b[HREPLIES_OK")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    # This large query fixture can overflow the shell's interactive line editor
    # when pasted as source. Run a file so the test measures reply handling.
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(probe)
        source.flush()
        s.send(("exec python3 " + shlex.quote(source.name) + "\n").encode())
        # The probe emits this marker only after validating every reply. The
        # simplified frame cache can retain a ZWJ suffix from the wide-glyph
        # cases above, so success does not require an otherwise empty row.
        s.expect(b"REPLIES_OK")
        s.finish(0)
finally:
    s.close()

# Paste markers and multiline UTF-8 payload travel unchanged to the child.
paste_probe = r"""
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
os.write(1, b"\x1b[?2004h\x1b[2J\x1b[HPASTE_READY")
receive("\x1b[200~中文\nsecond line\x1b[201~".encode())
os.write(1, b"\x1b[?2004l\x1b[HPASTE_PASSED")
receive(b"continue")
os.write(1, b"\x1b[?2004h\x1b[HPASTE_EXIT")
receive(b"exit")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(("exec python3 -c " + shlex.quote(paste_probe) + "\n").encode())
    s.expect(b"\r\nPASTE_READY\r\n")
    assert b"\x1b[?2004h" in s.last_frame
    s.send(b"\x1b[20")
    s.send("0~中文\nsecond line\x1b[201~".encode())
    s.expect(b"\r\nPASTE_PASSED\r\n")
    assert b"\x1b[?2004l" in s.last_frame
    s.send(b"continue")
    s.expect(b"PASTE_EXIT")
    assert b"\x1b[?2004h" in s.last_frame
    s.send(b"exit")
    s.finish(0)
    assert s.output.rfind(b"\x1b[?2004l") > s.output.rfind(b"\x1b[?2004h")
finally:
    s.close()

# Model an outer terminal's unmodified cursor keys in each requested mode.
cursor_probe = r"""
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
os.write(1, b"\x1b[?1h\x1b[2J\x1b[HAPP_KEYS")
receive(b"\x1bOA\x1bOB\x1bOC\x1bOD\x1bOH\x1bOF")
os.write(1, b"\x1b[!p\x1b[2J\x1b[HNORMAL_KEYS")
receive(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1b[H\x1b[F")
os.write(1, b"\x1b[?1h\x1b[2J\x1b[HEXIT_KEYS")
receive(b"exit")
"""
for terminate in (False, True):
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(("exec python3 -c " + shlex.quote(cursor_probe) + "\n").encode())
        s.expect(b"\r\nAPP_KEYS\r\n")
        assert b"\x1b[?1h" in s.last_frame
        if terminate:
            os.kill(s.app_pid, signal.SIGTERM)
            s.finish(128 + signal.SIGTERM)
        else:
            s.send(b"\x1bO")
            s.send(b"A\x1bOB\x1bOC\x1bOD\x1bOH\x1bOF")
            s.expect(b"\r\nNORMAL_KEYS\r\n")
            assert b"\x1b[?1l" in s.last_frame
            s.send(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1b[H\x1b[F")
            s.expect(b"\r\nEXIT_KEYS\r\n")
            assert b"\x1b[?1h" in s.last_frame
            s.send(b"exit")
            s.finish(0)
        assert s.output.rfind(b"\x1b[?1l") > s.output.rfind(b"\x1b[?1h")
    finally:
        s.close()

# Model the outer terminal's Backspace key in both DECBKM states.
backarrow_probe = r"""
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
os.write(1, b"\x1b[?67h\x1b[2J\x1b[HBACKSPACE_BS")
receive(b"\x08")
os.write(1, b"\x1b[?67l\x1b[2J\x1b[HBACKSPACE_DEL")
receive(b"\x7f")
os.write(1, b"\x1b[?67h\x1b[2J\x1b[HBACKSPACE_EXIT")
receive(b"exit")
"""
for terminate in (False, True):
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(("exec python3 -c " + shlex.quote(backarrow_probe) + "\n").encode())
        s.expect(b"\r\nBACKSPACE_BS\r\n")
        assert b"\x1b[?67h" in s.last_frame
        if terminate:
            os.kill(s.app_pid, signal.SIGTERM)
            s.finish(128 + signal.SIGTERM)
        else:
            s.send(b"\x08")
            s.expect(b"\r\nBACKSPACE_DEL\r\n")
            assert b"\x1b[?67l" in s.last_frame
            s.send(b"\x7f")
            s.expect(b"\r\nBACKSPACE_EXIT\r\n")
            assert b"\x1b[?67h" in s.last_frame
            s.send(b"exit")
            s.finish(0)
        assert s.output.rfind(b"\x1b[?67l") > s.output.rfind(b"\x1b[?67h")
    finally:
        s.close()

# Model keypad 0, 1, 9, decimal and Enter in numeric/application modes.
keypad_probe = r"""
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
os.write(1, b"\x1b[?66h\x1b[2J\x1b[HAPP_PAD")
receive(b"\x1bOp\x1bOq\x1bOy\x1bOn\x1bOM")
os.write(1, b"\x1b[?66l\x1b[2J\x1b[HNORMAL_PAD")
receive(b"019.\r")
os.write(1, b"\x1b[?66h\x1b[2J\x1b[HEXIT_PAD")
receive(b"exit")
"""
for terminate in (False, True):
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(("exec python3 -c " + shlex.quote(keypad_probe) + "\n").encode())
        s.expect(b"\r\nAPP_PAD\r\n")
        assert b"\x1b=" in s.last_frame
        if terminate:
            os.kill(s.app_pid, signal.SIGTERM)
            s.finish(128 + signal.SIGTERM)
        else:
            s.send(b"\x1bO")
            s.send(b"p\x1bOq\x1bOy\x1bOn\x1bOM")
            s.expect(b"\r\nNORMAL_PAD\r\n")
            assert b"\x1b>" in s.last_frame
            s.send(b"019.\r")
            s.expect(b"\r\nEXIT_PAD\r\n")
            assert b"\x1b=" in s.last_frame
            s.send(b"exit")
            s.finish(0)
        assert s.output.rfind(b"\x1b>") > s.output.rfind(b"\x1b=")
    finally:
        s.close()

# Synchronize each shape with a child handshake so frames cannot be coalesced.
shape_probe = r"""
import os, select, tty
tty.setraw(0)
for code in (1, 2, 3, 4, 5, 6):
    os.write(1, ("\x1b[?25l\x1b[%d q\x1b[2J\x1b[HSHAPE_%d" % (code, code)).encode())
    assert select.select([0], [], [], 6)[0]
    assert os.read(0, 1) == b"x"
os.write(1, b"\x1b[!p\x1b[2J\x1b[HSHAPE_RESET")
assert select.select([0], [], [], 6)[0]
assert os.read(0, 1) == b"x"
os.write(1, b"\x1b[6 q\x1b[2J\x1b[HSHAPE_EXIT")
assert select.select([0], [], [], 6)[0]
assert os.read(0, 1) == b"x"
"""
for terminate in (False, True):
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(("exec python3 -c " + shlex.quote(shape_probe) + "\n").encode())
        for code in range(1, 7):
            s.expect(("\r\nSHAPE_%d\r\n" % code).encode())
            assert s.cursor_shape == code
            assert s.last_frame.endswith(b"\x1b[?25l")
            s.send(b"x")
        s.expect(b"\r\nSHAPE_RESET\r\n")
        assert s.cursor_shape == 1
        assert s.last_frame.endswith(b"\x1b[?25h")
        s.send(b"x")
        s.expect(b"\r\nSHAPE_EXIT\r\n")
        assert s.cursor_shape == 6
        if terminate:
            os.kill(s.app_pid, signal.SIGTERM)
            s.finish(128 + signal.SIGTERM)
        else:
            s.send(b"x")
            s.finish(0)
        assert s.output.rfind(b"\x1b[0 q") > s.output.rfind(b"\x1b[6 q")
    finally:
        s.close()

# Kitty keyboard flags, stacks and queries are virtualized per pane. The active
# pane controls outer encoding, while an encoded Ctrl-B remains Rustmux's prefix.
keyboard_probe = r"""
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
os.write(1, b"\x1b[=3u\x1b[?u\x1b[2J\x1b[HKEYBOARD_" + b"READY")
receive(b"\x1b[?3u")
receive(b"x")
os.write(1, b"\x1b[=0u\x1b[2J\x1b[HKEYBOARD_" + b"EXIT")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(("exec python3 -c " + shlex.quote(keyboard_probe) + "\n").encode())
    output = s.expect(b"KEYBOARD_READY")
    assert b"\x1b[=3u" in output, output
    s.send(b"\x1b[98;5u\x1b[99;1u")
    output = s.expect(b"RUSTMUX_READY> ")
    assert b"\x1b[=0u" in output, output
    s.send(b"\x02p")
    output = s.expect(b"KEYBOARD_READY")
    assert b"\x1b[=3u" in output, output
    s.send(b"x")
    s.expect(b"KEYBOARD_EXIT")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
    assert s.output.rfind(b"\x1b[=0u") > s.output.rfind(b"\x1b[=3u")
finally:
    s.close()

# Focus events are input; output-side CSI I still means forward tabulation.
focus_probe = r"""
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
os.write(1, b"\x1b[?1004h\x1b[2J\x1b[HFOCUS_READY")
receive(b"\x1b[I\x1b[O\x1b[I")
os.write(1, b"\x1b[2J\x1b[HFOCUS_REDRAW")
receive(b"x")
os.write(1, b"\x1b[?1004l\x1b[2J\x1b[HFOCUS_DISABLED")
receive(b"x")
os.write(1, b"\x1b[?1004h\x1b[2J\x1b[HFOCUS_EXIT")
receive(b"x")
"""
for terminate in (False, True):
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(("exec python3 -c " + shlex.quote(focus_probe) + "\n").encode())
        s.expect(b"\r\nFOCUS_READY\r\n")
        assert b"\x1b[?1004h" in s.last_frame
        s.send(b"\x1b[")
        s.send(b"I\x1b[O\x1b[I")
        s.expect(b"\r\nFOCUS_REDRAW\r\n")
        assert b"\x1b[?1004" not in s.last_frame
        s.send(b"x")
        s.expect(b"\r\nFOCUS_DISABLED\r\n")
        assert b"\x1b[?1004l" in s.last_frame
        s.send(b"x")
        s.expect(b"\r\nFOCUS_EXIT\r\n")
        assert b"\x1b[?1004h" in s.last_frame
        if terminate:
            os.kill(s.app_pid, signal.SIGTERM)
            s.finish(128 + signal.SIGTERM)
        else:
            s.send(b"x")
            s.finish(0)
        assert s.output.rfind(b"\x1b[?1004l") > s.output.rfind(b"\x1b[?1004h")
    finally:
        s.close()

# Local pane focus changes synthesize reports for each pane that requested them.
pane_focus_probe = r"""
import os, select, sys, time, tty
label = sys.argv[1].encode()
tty.setraw(0)
def receive(expected):
    data = bytearray()
    end = time.monotonic() + 6
    while len(data) < len(expected):
        assert time.monotonic() < end, (label, repr(data))
        if select.select([0], [], [], 0.1)[0]:
            data.extend(os.read(0, len(expected) - len(data)))
    assert data == expected, (label, repr(data))
os.write(1, b"\x1b[?1004h\x1b[2J\x1b[H" + label + b"_READY")
receive(b"\x1b[O")
os.write(1, b"\x1b[2J\x1b[H" + label + b"_OUT")
receive(b"\x1b[I")
os.write(1, b"\x1b[2J\x1b[H" + label + b"_IN")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(pane_focus_probe)
        source.flush()
        s.send(("python3 " + shlex.quote(source.name) + " LEFT\n").encode())
        s.expect(b"LEFT_READY")
        s.send(b"\x02%")
        s.expect(b"LEFT_OUT")
        s.send(("python3 " + shlex.quote(source.name) + " RIGHT\n").encode())
        s.expect(b"RIGHT_READY")
        s.send(b"\x02h")
        s.expect(b"LEFT_IN")
        assert any(b"RIGHT_OUT" in row for row in s.last_rows), s.last_rows
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

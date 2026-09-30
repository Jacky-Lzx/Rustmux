"""Outer theme inheritance, per-pane overrides and reattachment through a real PTY."""
import os
import itertools
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from terminal_loop_support import BINARY, Session, expect_footer

# Catppuccin Mocha ANSI colors, followed by a distinguishable extended palette.
ANSI = [
    "45475a", "f38ba8", "a6e3a1", "f9e2af", "89b4fa", "f5c2e7", "94e2d5", "bac2de",
    "585b70", "f38ba8", "a6e3a1", "f9e2af", "89b4fa", "f5c2e7", "94e2d5", "a6adc8",
]
PALETTE = [tuple(bytes.fromhex(value)) for value in ANSI]
PALETTE += [(index * 13 % 256, index * 31 % 256, index * 7 % 256) for index in range(16, 256)]
FOREGROUND, BACKGROUND, CURSOR = (205, 214, 244), (30, 30, 46), (18, 52, 86)


def reply_to_probe(session, palette=PALETTE, ready_marker=b"RUSTMUX_READY>", reply_delay=0):
    deadline = time.monotonic() + 3
    while b"\x1b]12;?\x1b\\" not in session.output:
        session.read()
        assert time.monotonic() < deadline, bytes(session.output[-1000:])
    assert b"\x1b]4;0;?\x1b\\" in session.output
    assert b"\x1b]4;255;?\x1b\\" in session.output
    delayed_until = time.monotonic() + reply_delay
    while time.monotonic() < delayed_until:
        session.read(0.01)
    reply = bytearray()
    for index, color in enumerate(palette):
        value = "/".join(f"{component:02x}{component:02x}" for component in color)
        reply.extend(f"\x1b]4;{index};rgb:{value}\x1b\\".encode())
    for code, color in ((10, FOREGROUND), (11, BACKGROUND), (12, CURSOR)):
        value = "/".join(f"{component:02x}{component:02x}" for component in color)
        reply.extend(f"\x1b]{code};rgb:{value}\x07".encode())
    # An unsupported graphics reply still completes the shared FIFO barrier.
    reply.extend(b"\x1b[?1;2c")
    session.send(reply)
    session.expect(ready_marker)


def assert_color(raw, marker, foreground=None, background=None):
    position = raw.rfind(marker)
    assert position >= 0, (marker, raw[-2000:])
    colors = {38: None, 48: None}
    for match in re.finditer(rb"\x1b\[([0-9;]*)m", raw[:position]):
        values = [int(value or b"0") for value in match.group(1).split(b";")]
        index = 0
        while index < len(values):
            code = values[index]
            if code == 0:
                colors = {38: None, 48: None}
            elif code in (38, 48) and values[index + 1:index + 2] == [2]:
                colors[code] = tuple(values[index + 2:index + 5])
                index += 4
            index += 1
    if foreground is not None:
        assert colors[38] == foreground, (marker, colors, foreground)
    if background is not None:
        assert colors[48] == background, (marker, colors, background)


QUIET_IDS = itertools.count()


def quiet_shell(session):
    identity = next(QUIET_IDS)
    session.send(f"stty -echo; printf 'QUIET_%s\\n' {identity}\n".encode())
    session.expect(f"QUIET_{identity}".encode())


def printf(session, value, marker):
    session.send(f"printf '{value}'\n".encode())
    return session.expect(marker)


with tempfile.TemporaryDirectory(prefix="rustmux-colors-") as directory:
    # A child query checks actual replies, including that outer discovery data
    # never leaks to the shell. It also exercises the full 256-color inheritance.
    query_script = os.path.join(directory, "query.py")
    with open(query_script, "w") as file:
        file.write("#!" + sys.executable + "\n")
        file.write(f'''import os, select, termios, time, tty
original = termios.tcgetattr(0)
try:
    tty.setraw(0)
    os.write(1, b"\\x1b]4;1;?;200;?;255;?\\x07\\x1b]10;?;?;?\\x07")
    colors = {[(4, 1, PALETTE[1]), (4, 200, PALETTE[200]), (4, 255, PALETTE[255]), (10, None, FOREGROUND), (11, None, BACKGROUND), (12, None, CURSOR)]!r}
    expected = bytearray()
    for code, index, color in colors:
        prefix = str(code) + (";" + str(index) if index is not None else "")
        rgb = "/".join(f"{{component:02x}}{{component:02x}}" for component in color)
        expected.extend(f"\\x1b]{{prefix}};rgb:{{rgb}}\\x07".encode())
    received = bytearray()
    deadline = time.monotonic() + 3
    while len(received) < len(expected):
        assert time.monotonic() < deadline, repr(received)
        if select.select([0], [], [], 0.05)[0]:
            received.extend(os.read(0, 4096))
    assert received == expected, (received, expected)
finally:
    termios.tcsetattr(0, termios.TCSANOW, original)
os.write(1, b"\\r\\nCHILD_COLORS_OK\\r\\n")
''')

    os.chmod(query_script, 0o755)
    # Child queries are already waiting before the mock terminal answers. Both
    # startup paths must defer those queries until discovery is complete.
    for arguments in ((), ("new", f"colors-startup-{os.getpid()}")):
        startup = Session(shell=query_script, arguments=arguments)
        try:
            reply_to_probe(startup, ready_marker=b"CHILD_COLORS_OK", reply_delay=0.15)
            startup.finish(0)
        finally:
            startup.close()
            if arguments:
                subprocess.run([BINARY, "kill", arguments[1]], capture_output=True, timeout=5)

    session = Session()
    try:
        reply_to_probe(session)
        quiet_shell(session)
        # These are the same indexed SGR codes used by Fastfetch's text and table.
        text = "\\033[2J\\033[H"
        for index in range(16):
            code = 30 + index if index < 8 else 90 + index - 8
            text += f"\\033[{code}mF{index:02d} "
        text += "\\033[38;5;200mEXTENDED\\033[0m\\n"
        raw = printf(session, text, b"EXTENDED")
        for index in range(16):
            assert_color(raw, f"F{index:02d}".encode(), foreground=PALETTE[index])
        assert_color(raw, b"EXTENDED", foreground=PALETTE[200])
        text = "\\033[0m"
        for index in range(16):
            code = 40 + index if index < 8 else 100 + index - 8
            text += f"\\033[{code}mB{index:02d} "
        text += "\\033[0mBG_DONE\\n"
        raw = printf(session, text, b"BG_DONE")
        for index in range(16):
            assert_color(raw, f"B{index:02d}".encode(), background=PALETTE[index])
        assert_color(raw, b"BG_DONE", foreground=FOREGROUND, background=BACKGROUND)
        session.send(f"python3 {shlex.quote(query_script)}\n".encode())
        session.expect(b"CHILD_COLORS_OK")

        # Exercise the installed Fastfetch when available; CI needs no extra app.
        if shutil.which("fastfetch"):
            session.send(b"fastfetch --pipe false --logo none --structure Colors; printf 'FASTFETCH_%s\\n' OK\n")
            raw = session.expect(b"FASTFETCH_OK")
            for red, green, blue in PALETTE[:16]:
                assert f";48;2;{red};{green};{blue}".encode() in raw, raw[-2000:]

        raw = printf(session, "\\033]4;1;#010203\\007\\033]10;#112233;#445566;#778899\\007\\033[31mLOCAL_RED\\033[0m LOCAL_DEFAULT\\n", b"LOCAL_DEFAULT")
        assert_color(raw, b"LOCAL_RED", foreground=(1, 2, 3))
        assert_color(raw, b"LOCAL_DEFAULT", foreground=(17, 34, 51), background=(68, 85, 102))
        session.send(b"\x02%")
        session.expect(b"RUSTMUX_READY> ")
        quiet_shell(session)
        raw = printf(session, "\\033[31mNEW_RED\\033[0m NEW_DEFAULT\\n", b"NEW_DEFAULT")
        assert_color(raw, b"NEW_RED", foreground=PALETTE[1])
        assert_color(raw, b"NEW_DEFAULT", foreground=FOREGROUND, background=BACKGROUND)
        session.send(b"exit 0\n")
        session.expect(b"RUSTMUX_READY> ")
        raw = printf(session, "\\033[31mKEPT_RED\\033[0m\\n", b"KEPT_RED")
        assert_color(raw, b"KEPT_RED", foreground=(1, 2, 3))
        session.send(b"\x02[")
        expect_footer(session, b"HISTORY")
        session.send(b"q")
        expect_footer(session, b"LOCKED")
        raw = printf(session, "\\033]104\\007\\033]110\\007\\033]111\\007\\033]112\\007\\033[31mRESET_RED\\033[0m RESET_DEFAULT\\n", b"RESET_DEFAULT")
        assert_color(raw, b"RESET_RED", foreground=PALETTE[1])
        assert_color(raw, b"RESET_DEFAULT", foreground=FOREGROUND, background=BACKGROUND)
        session.send(b"exit 0\n")
        session.finish(0)
    finally:
        session.close()

    name = f"colors-{os.getpid()}"
    session = None
    try:
        session = Session(arguments=("new", name))
        reply_to_probe(session)
        quiet_shell(session)
        raw = printf(session, "\\033]4;1;#010203\\007\\033[31mBEFORE_DETACH\\033[0m\\n", b"BEFORE_DETACH")
        assert_color(raw, b"BEFORE_DETACH", foreground=(1, 2, 3))
        session.send(b"\x02d")
        session.finish(0)
        session.close()
        session = None

        changed = list(PALETTE)
        changed[1] = (11, 22, 33)
        changed[2] = (44, 55, 66)
        session = Session(arguments=("attach", name))
        reply_to_probe(session, changed)
        raw = printf(session, "\\033[31mKEPT_OVERRIDE\\033[32mNEW_THEME\\033[0m\\n", b"NEW_THEME")
        assert_color(raw, b"KEPT_OVERRIDE", foreground=(1, 2, 3))
        assert_color(raw, b"NEW_THEME", foreground=changed[2])
        raw = printf(session, "\\033]104;1\\007\\033[31mRESET_THEME\\033[0m\\n", b"RESET_THEME")
        assert_color(raw, b"RESET_THEME", foreground=changed[1])
        session.send(b"exit 0\n")
        session.finish(0)
    finally:
        if session is not None:
            session.close()
        subprocess.run([BINARY, "kill", name], capture_output=True, timeout=5)

"""Outer-PTY history integration scenarios."""
import base64
import fcntl
import os
import re
import shlex
import signal
import struct
import termios
import tempfile
import time

from terminal_loop_support import (
    Session,
    expect_bar,
    expect_bar_without,
    expect_footer,
)


def expect_history_top(session):
    """Wait for the current screen, not an earlier cached frame, to show HIST_00."""
    deadline = time.monotonic() + 3
    while True:
        footer = session.physical_rows[-1] if session.physical_rows else b""
        if re.search(rb"HISTORY  ([1-9][0-9]*)/\1(?:\D|$)", footer):
            for index, row in enumerate(session.physical_rows[1:-1], start=1):
                if b"HIST_00" in row:
                    session.output.clear()
                    session.frames.clear()
                    return index + 1  # SGR mouse coordinates are one-based.
        session.read()
        assert time.monotonic() < deadline, (session.physical_rows, bytes(session.output[-1000:]))


# A partially filled primary screen can enter history before it has scrollback.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"\x02[")
    expect_footer(s, b"HISTORY  0/0")
    assert b"1 shell" in s.physical_rows[0]
    assert b"RUSTMUX_READY>" in b"".join(s.last_rows)
    assert b"\x1b[?1002h" in s.last_frame
    s.output.clear()
    s.send(b"/RUSTMUX_READY\r")
    expect_footer(s, b"1/1 /RUSTMUX_READY")
    s.output.clear()
    s.send(b"\x1b")
    deadline = time.monotonic() + 3
    while b"/RUSTMUX_READY" in s.physical_rows[-1]:
        s.read()
        assert time.monotonic() < deadline, bytes(s.output[-1000:])
    assert b"HISTORY  0/0" in s.physical_rows[-1]
    s.output.clear()
    s.send(b"/draft")
    expect_footer(s, b"/draft")
    s.output.clear()
    s.send(b"\x1b")
    deadline = time.monotonic() + 3
    while b"/draft" in s.physical_rows[-1]:
        s.read()
        assert time.monotonic() < deadline, bytes(s.output[-1000:])
    assert b"HISTORY  0/0" in s.physical_rows[-1]
    s.send(b"/still-in-history")
    expect_footer(s, b"Search /still-in-history")
    s.send(b"\x03")
    expect_footer(s, b"HISTORY  0/0")
    s.output.clear()
    s.send(b"\x1b")
    deadline = time.monotonic() + 3
    while b"\x1b[?1002l" not in s.output:
        s.read()
        assert time.monotonic() < deadline, bytes(s.output[-1000:])
    s.send(b"printf 'ESCAPE_HISTORY_OK\\n'\n")
    s.expect(b"ESCAPE_HISTORY_OK")
    s.send(b"exit 0\n")
    s.finish(0)
finally:
    s.close()

# Browse a frozen pane snapshot while new output arrives; navigation never types into the shell.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"stty -echo; printf '\\033[2J\\033[H%s%s\\n' HISTORY _LEFT\n")
    s.expect(b"HISTORY_LEFT")
    s.send(b"\x02%")
    s.expect(b"RUSTMUX_READY>")
    with tempfile.TemporaryDirectory(prefix="rustmux-history-") as directory:
        trigger = os.path.join(directory, "continue")
        command = (
            "stty -echo; printf 'SELECT_%s\\n' TARGET; i=0; "
            "while [ $i -lt 45 ]; do printf 'HIST_%02d\\n' $i; i=$((i+1)); done; "
            "(while [ ! -f " + shlex.quote(trigger) + " ]; do sleep 0.05; done; "
            "printf '\\nLATE_HISTORY_OUTPUT\\n') &\n"
        )
        s.send(command.encode())
        s.expect(b"HIST_44")
        s.send(b"\x02[g")
        s.expect(b"HIST_00")
        expect_footer(s, b"HISTORY ")
        assert b"\x1b[?1002h" in s.last_frame
        assert b"\x1b[?1006h" in s.last_frame
        assert any(b"HISTORY_LEFT" in row for row in s.last_rows)
        s.send(b"yyy")
        deadline = time.monotonic() + 3
        copies = []
        while len(copies) < 3:
            s.read()
            copies = re.findall(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        decoded = [base64.b64decode(payload, validate=True) for payload in copies]
        assert len(decoded) == 3 and all(text == decoded[0] for text in decoded)
        assert b"HIST_00" in decoded[0] and b"HISTORY_LEFT" not in decoded[0]
        s.output.clear()
        s.send(b"/SELECT_TARGET\r")
        expect_footer(s, b"1/1 /SELECT_TARGET")
        s.output.clear()
        s.send(b"y")
        deadline = time.monotonic() + 3
        while not (matched := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        assert base64.b64decode(matched.group(1), validate=True) == b"SELECT_TARGET"
        s.output.clear()
        s.send(b"v")
        expect_footer(s, b"Select ")
        s.output.clear()
        s.send(b"\x1b")
        deadline = time.monotonic() + 3
        while (b"/SELECT_TARGET" not in s.physical_rows[-1]
               or b"Select " in s.physical_rows[-1]):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        s.output.clear()
        s.send(b"voy")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert selected_text == b"SELECT_TARGET", (selected_text, s.last_rows, bytes(s.output[-500:]))
        expect_footer(s, b"Copy sent to terminal")
        s.output.clear()
        s.send(b"\x1b")
        deadline = time.monotonic() + 3
        while not (
            re.search(rb"HISTORY  [0-9]+/[0-9]+", s.physical_rows[-1])
            and b"/SELECT_TARGET" not in s.physical_rows[-1]
        ):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        s.output.clear()
        s.send(b"v\x1b[1;5Cy")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert selected_text == b"SELECT_TARGET", (selected_text, s.last_rows, bytes(s.output[-500:]))
        s.output.clear()
        s.send(b"\x1b[1;2Cy")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert selected_text == b"SE", (selected_text, s.last_rows, bytes(s.output[-500:]))
        s.output.clear()
        s.send(b"\x1b[1;6Cy")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert selected_text == b"SELECT_TARGET", (selected_text, s.last_rows, bytes(s.output[-500:]))
        s.output.clear()
        s.send(b"vey")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert selected_text == b"SELECT_TARGET", (selected_text, s.last_rows, bytes(s.output[-500:]))
        s.output.clear()
        s.send(b"v$y")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert selected_text == b"SELECT_TARGET", (selected_text, s.last_rows, bytes(s.output[-500:]))
        s.output.clear()
        s.send(b"v$\r")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert selected_text == b"SELECT_TARGET", (selected_text, s.last_rows, bytes(s.output[-500:]))
        s.output.clear()
        s.send(b"\x1b[1;2F\r")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert selected_text == b"SELECT_TARGET", (selected_text, s.last_rows, bytes(s.output[-500:]))
        s.send(b"g")
        expect_history_top(s)
        s.send(b"\x1b[6;2~\r")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert b"\nHIST_00\n" in selected_text, (selected_text, s.last_rows, bytes(s.output[-500:]))
        assert selected_text.endswith(b"\nH"), (selected_text, s.last_rows, bytes(s.output[-500:]))
        assert selected_text.count(b"\n") >= 2, (selected_text, s.last_rows, bytes(s.output[-500:]))
        s.send(b"g")
        expect_history_top(s)
        s.send(b"\x1b[1;6F\r")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert b"\nHIST_00\n" in selected_text, (selected_text, s.last_rows, bytes(s.output[-500:]))
        assert b"\nHIST_44\n" in selected_text, (selected_text, s.last_rows, bytes(s.output[-500:]))
        s.send(b"g")
        target_row = expect_history_top(s)
        # Drag over HIST_00 in the right pane using its current outer-terminal row.
        # Its first four cells copy HIST and leave shell input untouched.
        s.send((f"\x1b[<0;42;{target_row}M\x1b[<32;45;{target_row}M"
                f"\x1b[<0;45;{target_row}m").encode())
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert selected_text == b"HIST", (selected_text, s.last_rows, bytes(s.output[-500:]))
        expect_footer(s, b"Copy sent to terminal")
        s.send(b"G")
        s.expect(b"HIST_44")
        s.output.clear()
        # One motion report above the pane keeps scrolling while the button is held.
        s.send(b"\x1b[<0;42;13M\x1b[<32;42;1M")
        deadline = time.monotonic() + 3
        while not re.search(rb"HISTORY  [1-9][0-9]*/", s.physical_rows[-1]):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        s.output.clear()
        s.send(b"\x1b[<0;42;1m")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert b"\n" in selected_text, (selected_text, s.last_rows, bytes(s.output[-500:]))
        expect_footer(s, b"Copy sent to terminal")
        deadline = time.monotonic() + 3
        while b"Copy sent to terminal" in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, (s.last_rows, bytes(s.output[-1000:]))
        assert re.search(rb"HISTORY  [0-9]+/[0-9]+", s.physical_rows[-1]), s.physical_rows
        frozen = list(s.last_rows)
        with open(trigger, "w") as file:
            file.write("go")
        end = time.monotonic() + 0.3
        while time.monotonic() < end:
            s.read(0.05)
        assert s.last_rows == frozen
        s.send(b"\x1b[200~qjG\x03\x02c\x1b[201~")
        s.read(0.1)
        assert s.last_rows == frozen
        s.send(b"/HIST_0\r")
        expect_footer(s, b"1/10 /HIST_0")
        s.send(b"N")
        expect_footer(s, b"10/10 /HIST_0")
        s.send(b"n")
        expect_footer(s, b"1/10 /HIST_0")
        assert any(b"HIST_00" in row for row in s.last_rows[1:])
        s.send(b"G?HIST_0\r")
        expect_footer(s, b"10/10 ?HIST_0")
        s.send(b"n")
        expect_footer(s, b"9/10 ?HIST_0")
        s.send(b"N")
        expect_footer(s, b"10/10 ?HIST_0")
        s.send(b"/cancelled\x03n")
        expect_footer(s, b"9/10 ?HIST_0")
        s.send(b"?HIST_0\rn")
        expect_footer(s, b"8/10 ?HIST_0")
        s.send(b"/DOES_NOT_EXIST\r")
        expect_footer(s, b"no match")
        s.send(b"/qjk\x03")
        expect_footer(s, b"no match")
        s.send(b"?DRAFT\x1b[A")
        expect_footer(s, b"Search ?DOES_NOT_EXIST")
        s.send(b"\x1bOA")
        expect_footer(s, b"Search ?HIST_0")
        s.send(b"\x1bOB\x1b[B")
        expect_footer(s, b"Search ?DRAFT")
        s.send(b"\x1b[A\x1b[A\r")
        expect_footer(s, b"8/10 ?HIST_0")
        assert b"Search ?HIST_0" not in s.physical_rows[-1]
        s.send(b"/HIST_X\x1b[D\x1b[3~0")
        expect_footer(s, b"Search /HIST_0")
        assert b"\x1b[?25h" in s.last_frame
        s.send(b"\x1b[H\x1b[C\x1b[F\r")
        expect_footer(s, b"8/10 /HIST_0")
        assert b"\x1b[?25l" in s.last_frame
        s.send(b"/HIST_\x1b[200~0\r\n\x03\x02\x15\x7f\x1b[201~")
        expect_footer(s, b"Search /HIST_0")
        assert b"\x1b[?25h" in s.last_frame
        s.send(b"\r")
        expect_footer(s, b"8/10 /HIST_0")
        s.send(b"G")
        expect_footer(s, b"HISTORY  0/")
        assert not any(b"LATE_HISTORY_OUTPUT" in row for row in s.last_rows)
        # Mouse coordinates are outer-terminal coordinates: bar row 1 and the
        # left pane / column-40 separator must not scroll the right snapshot.
        frozen = list(s.last_rows)
        s.send(b"\x1b[<64;10;5M\x1b[<64;40;5M\x1b[<64;50;1M")
        s.read(0.1)
        assert s.last_rows == frozen
        s.send(b"\x1b[<64;50;5M")
        expect_footer(s, b"HISTORY  3/")
        assert any(b"HISTORY_LEFT" in row for row in s.last_rows)
        s.send(b"\x1b[<65;50;5M")
        expect_footer(s, b"HISTORY  0/")
        s.send(b"q")
        s.expect(b"LATE_HISTORY_OUTPUT")
        deadline = time.monotonic() + 3
        while s.private_modes.get(1006) is not False:
            s.read()
            assert time.monotonic() < deadline, (s.private_modes, s.last_frame, s.output[-1000:])
        assert s.private_modes.get(1002) is True
        s.send(b"printf '\\n%s%s\\n' INPUT_ INTACT\n")
        s.expect(b"INPUT_INTACT")
        s.send(b"\x02[\x1b[5~")
        expect_footer(s, b"HISTORY ")
        fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        expect_bar(s, b"1 shell")
        deadline = time.monotonic() + 3
        while (b"HISTORY " in s.physical_rows[-1]
               or s.private_modes.get(1006) is not False):
            s.read()
            assert time.monotonic() < deadline, (s.private_modes, s.last_frame, s.output[-1000:])
        assert b"LOCKED" in s.physical_rows[-1], s.physical_rows[-1]
        assert s.private_modes.get(1002) is True
        s.send(b"printf '\\n%s%s\\n' RESIZE_ LIVE\n")
        s.expect(b"RESIZE_LIVE")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

# Unzooming a vertically split pane retains its prompt and archives departed rows.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b'\x02"\x02Z')
    s.expect(b"RUSTMUX_READY>")
    s.send(b"stty -echo; printf '\\033[2J\\033[H'; i=0; while [ $i -lt 20 ]; do printf 'RESIZE_HIST_%02d\\n' $i; i=$((i+1)); done\n")
    s.expect(b"RESIZE_HIST_19")
    s.send(b"\x02Z")
    end = time.monotonic() + 2
    # Wait for the restored separator, not a frame from before the shortcut.
    while not any(b"\xe2\x94\x80" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < end, s.last_rows
    assert any(b"RESIZE_HIST_19" in row for row in s.last_rows)
    assert not any(b"RESIZE_HIST_00" in row for row in s.last_rows)
    s.send(b"\x02[g")
    s.expect(b"RESIZE_HIST_00")
    expect_footer(s, b"HISTORY ")
    s.send(b"qprintf '\\n%s%s\\n' RESIZE_PROMPT_ OK\n")
    s.expect(b"RESIZE_PROMPT_OK")
    s.send(b"\x02Z")
    s.expect(b"RESIZE_HIST_03")
    assert any(b"RESIZE_PROMPT_OK" in row for row in s.last_rows)
    s.send(b"printf '\\n%s%s\\n' REGROWN_ INPUT_OK\n")
    s.expect(b"REGROWN_INPUT_OK")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

# Primary output reflows across real SIGWINCH width changes without truncating text.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    payload = b"REFLOW_BEGIN_" + b"0123456789" * 12 + b"_END"
    s.send(b"stty -echo; printf '\\033[2J\\033[H%s\\n' " + payload + b"\n")
    s.expect(b"_END")
    for width in [32, 53, 80]:
        fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, width, 0, 0))
        end = time.monotonic() + 3
        content_width = width - 2
        count = (len(payload) + content_width - 1) // content_width
        while True:
            s.read()
            visible = b"".join(row[:content_width] for row in s.last_rows[1:1 + count])
            if visible == payload:
                break
            assert time.monotonic() < end, (width, s.last_rows)
    s.send(b"printf '\\n%s%s\\n' REFLOW_ INPUT_OK; exit 0\n")
    s.finish(0)
    assert any(b"REFLOW_INPUT_OK" in row for row in s.last_rows)
finally:
    s.close()

# Manual separator movement updates both children without replacing their shells.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"stty -echo; resize_token=left; printf 'RESIZE_%s\\n' READY\n")
    s.expect(b"RESIZE_READY")
    s.send(b"\x02%")
    s.expect(b"RUSTMUX_READY>")
    s.send(b"stty -echo; resize_token=right; printf 'RIGHT_%s\\n' READY\n")
    s.expect(b"RIGHT_READY")
    s.send(b"\x02\x0c")
    s.send(b"printf 'RIGHT_%s_' \"$resize_token\"; stty size\n")
    s.expect(b"RIGHT_right_20 37")
    s.send(b"\x02h")
    s.send(b"printf 'LEFT_%s_' \"$resize_token\"; stty size\n")
    s.expect(b"LEFT_left_20 39")
    s.send(b'\x02\x08\x02"')
    s.expect(b"RUSTMUX_READY>")
    s.send(b"stty -echo; printf 'BOTTOM_%s\\n' READY\n")
    s.expect(b"BOTTOM_READY")
    s.send(b"\x02\x0b")
    s.send(b"printf 'BOTTOM_%s_' SIZE; stty size\n")
    s.expect(b"BOTTOM_SIZE_10 38")
    s.send(b"\x02Z\x02\x0a\x02Z")
    s.send(b"printf 'RESTORED_%s_' SIZE; stty size\n")
    s.expect(b"RESTORED_SIZE_10 38")
    s.send(b"\x02\x0a")
    s.send(b"printf 'DOWN_%s_' SIZE; stty size\n")
    s.expect(b"DOWN_SIZE_9 38")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

# Swaps move live pane identities and focus, including across unequal rectangles.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"stty -echo; swap_token=LEFT; swap_pid=$$; printf 'LEFT_%s\\n' READY\n")
    s.expect(b"LEFT_READY")
    s.send(b"\x02%")
    s.expect(b"RUSTMUX_READY>")
    s.send(b"stty -echo; swap_token=RIGHT; swap_pid=$$; printf 'RIGHT_%s\\n' READY\n")
    s.expect(b"RIGHT_READY")
    # Wrap the last pane into the first slot; same-batch input stays with RIGHT.
    s.send(b"\x02}test \"$swap_pid\" = \"$$\" && printf '%s_' \"$swap_token\"; stty size\n")
    s.expect(b"RIGHT_20 38")
    assert any(b"RIGHT_20 38" in row[:38] for row in s.last_rows[1:])
    s.send(b"\x02{test \"$swap_pid\" = \"$$\" && printf '%s_BACK_' \"$swap_token\"; stty size\n")
    s.expect(b"RIGHT_BACK_20 38")
    # Focus LEFT by geometry: its shell and variables survived both exchanges.
    s.send(b"\x02htest \"$swap_pid\" = \"$$\" && printf '%s_STILL_' \"$swap_token\"; stty size\n")
    s.expect(b"LEFT_STILL_20 38")
    s.send(b"exit 0\n")
    # Wait until removal is rendered before sending input to the surviving shell.
    end = time.monotonic() + 3
    while any("│".encode() in row for row in s.last_rows[1:]):
        s.read()
        assert time.monotonic() < end, s.last_rows
    s.send(b"printf '%s_SURVIVES_' \"$swap_token\"; stty size\n")
    s.expect(b"RIGHT_SURVIVES_20 78")
    s.send(b"exit 0\n")
    s.finish(0)
finally:
    s.close()

# Confirmed close hides one shell for undo and preserves existing cancellation checks.
with tempfile.TemporaryDirectory() as directory:
    record = os.path.join(directory, "pane.pid")
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"stty -echo; KEEP=survivor; printf 'KEEP_%s\\n' READY\n")
        s.expect(b"KEEP_READY")
        s.send(b"\x02%")
        s.expect(b"RUSTMUX_READY>")
        s.send(("stty -echo; UNDO_KEEP=restored; cd " + shlex.quote(directory) + "; echo $$ > " + shlex.quote(record) + "; printf 'PANE_%s\\n' READY\n").encode())
        s.expect(b"PANE_READY")
        with open(record) as source:
            closing_pid = int(source.read())
        for answer in (b"\r", b"no\r", b"YES\r", b"yes\x03", b"yes\x07", b"yes\x1b"):
            s.send(b"\x02x")
            s.expect(b"Close pane? Type yes:")
            s.send(answer)
            end = time.monotonic() + 3
            while s.last_rows[0].startswith(b"Close pane?"):
                s.read()
                assert time.monotonic() < end
            os.kill(closing_pid, 0)
        # Close the zoomed pane: removal unzooms and restores the sibling layout.
        s.send(b"\x02Z\x02x")
        s.expect(b"Close pane? Type yes:")
        s.send(b"\x1b[200~yes\r\n\x1b[201~")
        s.expect(b"Close pane? Type yes: yes")
        os.kill(closing_pid, 0)
        s.send(b"\rLEAK=1\n")
        s.expect(b"KEEP_READY")
        os.kill(closing_pid, 0)  # Undo keeps this shell alive and hidden.
        s.send(b"printf '\\nSURVIVOR:%s:%s_' $KEEP ${LEAK-unset}; stty size\n")
        s.expect(b"SURVIVOR:survivor:unset_20 78")
        s.send(b"\x02z")
        command = ("test \"$PWD\" = " + shlex.quote(directory) +
                   " && printf '\\nUNDO:%s:%s\\n' $UNDO_KEEP $$\n")
        s.send(command.encode())
        s.expect(("UNDO:restored:" + str(closing_pid)).encode())
        s.send(b"\x02x")
        s.expect(b"Close pane? Type yes:")
        s.send(b"yes\r")
        s.expect(b"SURVIVOR:survivor:unset_20 78")
        # A target that exits naturally while confirming must not close its sibling.
        s.send(b"\x02%")
        s.expect(b"RUSTMUX_READY>")
        s.send(b"sleep 0.2; exit 7\n\x02x")
        s.expect(b"Close pane? Type yes:")
        end = time.monotonic() + 3
        while (s.last_rows[0].startswith(b"Close pane?") or
               any("│".encode() in row for row in s.last_rows[1:])):
            s.read()
            assert time.monotonic() < end, s.last_rows
        s.send(b"printf '\\nSTILL_%s\\n' $KEEP\n")
        s.expect(b"STILL_survivor")
        # Sole-pane close selects another window; the final close exits even with undo cached.
        s.send(b"\x02c")
        s.expect(b"RUSTMUX_READY>")
        s.send(b"\x02x")
        s.expect(b"Close pane? Type yes:")
        s.send(b"yes\r")
        expect_bar(s, b"1 shell")
        s.send(b"\x02x")
        s.expect(b"Close pane? Type yes:")
        s.send(b"yes\r")
        s.finish(0)
    finally:
        s.close()

# Kill a separate foreground job, retain the shell, and replace only one undo slot.
with tempfile.TemporaryDirectory() as directory:
    job_record = os.path.join(directory, "job.pid")
    shell_record = os.path.join(directory, "shell.pid")
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; printf 'BASE_%s\\n' READY\n")
        s.expect(b"BASE_READY")
        s.send(b"\x02%")
        s.expect(b"RUSTMUX_READY>")
        s.send(("stty -echo; SAVED=original; echo $$ > " + shlex.quote(shell_record) +
                "; printf 'HISTORY_%s\\n' SAVED\n").encode())
        s.expect(b"HISTORY_SAVED")
        with open(shell_record) as source:
            shell_pid = int(source.read())
        script = ("import os,time; open(" + repr(job_record) + ", 'w').write(str(os.getpid())); "
                  "print('\\x1b[?1049h\\x1b[?1003hJOB_RUNNING', flush=True); time.sleep(60)")
        s.send(("python3 -c " + shlex.quote(script) + "\n").encode())
        s.expect(b"JOB_RUNNING")
        with open(job_record) as source:
            job_pid = int(source.read())
        s.send(b"\x02x")
        s.expect(b"Close pane? Type yes:")
        s.send(b"yes\r")
        s.expect(b"BASE_READY")
        end = time.monotonic() + 3
        while True:
            try:
                os.kill(job_pid, 0)
            except ProcessLookupError:
                break
            s.read()
            assert time.monotonic() < end, "foreground job survived close"
        os.kill(shell_pid, 0)
        s.send(b"\x02z")
        s.expect(b"HISTORY_SAVED")
        assert not s.private_modes.get(1003, False)
        s.send(b"printf '\\nRESTORED:%s:%s\\n' $SAVED $$\n")
        s.expect(("RESTORED:original:" + str(shell_pid)).encode())
        s.send(b"\x02x")
        s.expect(b"Close pane? Type yes:")
        s.send(b"yes\r")
        s.expect(b"BASE_READY")
        # New pane C supersedes hidden B; B is finally killed/reaped.
        s.send(b'\x02"')
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; SAVED=newest; printf 'NEWEST_%s\\n' READY\n")
        s.expect(b"NEWEST_READY")
        trigger = os.path.join(directory, "hidden-trigger")
        done = os.path.join(directory, "hidden-done")
        writer = ("import pathlib,sys,time; trigger=pathlib.Path(" + repr(trigger) + "); "
                  "exec('while not trigger.exists(): time.sleep(0.01)'); "
                  "sys.stdout.write('hidden-output' * 16000); sys.stdout.flush(); "
                  "pathlib.Path(" + repr(done) + ").write_text('done')")
        s.send(("python3 -c " + shlex.quote(writer) + " &\n").encode())
        s.expect(b"RUSTMUX_READY>")
        s.send(b"\x02x")
        s.expect(b"Close pane? Type yes:")
        s.send(b"yes\r")
        s.expect(b"BASE_READY")
        try:
            os.kill(shell_pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError("older hidden shell was not discarded")
        with open(trigger, "w") as output:
            output.write("go")
        end = time.monotonic() + 4
        while not os.path.exists(done):
            s.read()
            assert time.monotonic() < end, "hidden output stopped draining"
        assert not any(b"hidden-output" in row for row in s.last_rows)
        s.send(b"\x02zprintf '\\nONLY_%s\\n' $SAVED\n")
        s.expect(b"ONLY_newest")
        s.send(b"\x02&")
        s.expect(b"Close window? Type yes:")
        s.send(b"yes\r")
        s.finish(0)
    finally:
        s.close()

# Break a pane into a new window without interrupting its foreground program.
with tempfile.TemporaryDirectory() as directory:
    record = os.path.join(directory, "moving-job.pid")
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; KEEP=source; printf 'SOURCE_%s\\n' READY\n")
        s.expect(b"SOURCE_READY")
        s.send(b"\x02%")
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; KEEP=moved; printf 'HISTORY_%s\\n' MOVED\n")
        s.expect(b"HISTORY_MOVED")
        script = ("import os; open(" + repr(record) + ", 'w').write(str(os.getpid())); "
                  "print('JOB_READY', flush=True); "
                  "exec('while input() != \"quit\": print(\"JOB:%s:%s\" % "
                  "(os.getpid(), os.get_terminal_size().columns), flush=True)')")
        s.send(("python3 -c " + shlex.quote(script) + "\n").encode())
        s.expect(b"JOB_READY")
        with open(record) as source:
            moving_pid = int(source.read())
        s.send(b"report\n")
        s.expect(("JOB:%s:38" % moving_pid).encode())
        s.send(b"\x02!report\n")
        s.expect(("JOB:%s:78" % moving_pid).encode())
        expect_bar(s, b"2 shell")
        assert any(b"HISTORY_MOVED" in row for row in s.last_rows[1:])
        os.kill(moving_pid, 0)
        s.send(b"\x02\tprintf '\\nKEEP:%s_' $KEEP; stty size\n")
        s.expect(b"KEEP:source_20 78")
        expect_bar(s, b"1 shell")
        s.send(b"\x02\treport\n")
        s.expect(("JOB:%s:78" % moving_pid).encode())
        s.send(b"quit\n")
        s.expect(b"RUSTMUX_READY>")
        s.send(b"printf '\\nKEEP:%s\\n' $KEEP\n")
        s.expect(b"KEEP:moved")
        s.send(b"exit 0\n")
        expect_bar_without(s, b"2 shell")
        expect_bar(s, b"1 shell")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()

# Join a running pane to an existing window; pasted Enter cannot submit the target.
with tempfile.TemporaryDirectory() as directory:
    record = os.path.join(directory, "join-job.pid")
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; KEEP=target; printf 'TARGET_%s\\n' READY\n")
        s.expect(b"TARGET_READY")
        s.send(b"\x02c")
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; KEEP=moved; printf 'JOIN_%s\\n' HISTORY\n")
        s.expect(b"JOIN_HISTORY")
        for answer in (b"999\r", b"2\r", b"1\x03", b"1\x1b"):
            s.send(b"\x02m")
            s.expect(b"Move to window #:")
            s.send(answer)
            expect_bar(s, b"2 shell")
        script = ("import os; open(" + repr(record) + ", 'w').write(str(os.getpid())); "
                  "print('JOIN_JOB_READY', flush=True); "
                  "exec('while input() != \"quit\": print(\"JOINJOB:%s:%s\" % "
                  "(os.getpid(), os.get_terminal_size().columns), flush=True)')")
        s.send(("python3 -c " + shlex.quote(script) + "\n").encode())
        s.expect(b"JOIN_JOB_READY")
        with open(record) as source:
            pid = int(source.read())
        s.send(b"\x02m\x1b[200~1\r\n\x1b[201~")
        s.expect(b"Move to window #: 1")
        os.kill(pid, 0)
        s.send(b"\rreport\n")
        s.expect(("JOINJOB:%s:38" % pid).encode())
        expect_bar_without(s, b"2 shell")
        expect_bar(s, b"1 shell")
        assert any(b"JOIN_HISTORY" in row for row in s.last_rows[1:])
        s.send(b"\x02hprintf '\\nKEEP:%s_' $KEEP; stty size\n")
        s.expect(b"KEEP:target_20 38")
        s.send(b"\x02lquit\n")
        s.expect(b"RUSTMUX_READY>")
        s.send(b"printf '\\nKEEP:%s\\n' $KEEP\n")
        s.expect(b"KEEP:moved")
        s.send(b"\x02&")
        s.expect(b"Close window? Type yes:")
        s.send(b"yes\r")
        s.finish(0)
    finally:
        s.close()

# Export retained and visible primary text to an editor window without replacing the source shell.
with tempfile.TemporaryDirectory(prefix="rustmux-history-editor-") as directory:
    capture = os.path.join(directory, "capture.txt")
    path_capture = os.path.join(directory, "path.txt")
    editor = os.path.join(directory, "editor.sh")
    with open(editor, "w") as script:
        script.write("#!/bin/sh\ncp \"$1\" \"$CAPTURE\"\nprintf '%s' \"$1\" > \"$PATH_CAPTURE\"\n")
    os.chmod(editor, 0o700)
    s = Session(extra_env={
        "VISUAL": "",
        "EDITOR": editor,
        "CAPTURE": capture,
        "PATH_CAPTURE": path_capture,
    })
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; KEEP=alive; i=0; while [ $i -lt 30 ]; do printf 'EDIT_%02d\\n' $i; i=$((i+1)); done\n")
        s.expect(b"EDIT_29")
        s.send(b"\x02E")
        end = time.monotonic() + 8
        while not (os.path.exists(capture) and os.path.exists(path_capture)):
            s.read()
            assert time.monotonic() < end, bytes(s.output[-1000:])
        with open(capture, "rb") as file:
            exported = file.read()
        assert b"EDIT_00\n" in exported and b"EDIT_29\n" in exported, exported
        with open(path_capture) as file:
            snapshot_path = file.read()
        end = time.monotonic() + 3
        while os.path.exists(snapshot_path):
            s.read()
            assert time.monotonic() < end, snapshot_path
        s.send(b"printf 'ORIGINAL_%s\\n' \"$KEEP\"\n")
        s.expect(b"ORIGINAL_alive")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()


# Open only a completed OSC 133 command-output region in a temporary editor window.
with tempfile.TemporaryDirectory(prefix="rustmux-command-editor-") as directory:
    capture = os.path.join(directory, "capture.txt")
    path_capture = os.path.join(directory, "path.txt")
    editor = os.path.join(directory, "editor.sh")
    with open(editor, "w") as script:
        script.write("#!/bin/sh\ncp \"$1\" \"$CAPTURE\"\nprintf '%s' \"$1\" > \"$PATH_CAPTURE\"\n")
    os.chmod(editor, 0o700)
    editor_env = {
        "VISUAL": "",
        "EDITOR": editor,
        "CAPTURE": capture,
        "PATH_CAPTURE": path_capture,
    }
    s = Session(extra_env=editor_env)
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; KEEP=semantic\n")
        s.expect(b"RUSTMUX_READY>")
        s.send(
            b"printf '\\033]133;C\\007'; "
            b"printf 'FIRST \\033[31mred\\033[0m\\nSECOND\\n'; "
            b"printf '\\033]133;D;0\\007'\n"
        )
        s.expect(b"SECOND")
        s.send(b"\x02e")
        end = time.monotonic() + 8
        while not (os.path.exists(capture) and os.path.exists(path_capture)):
            s.read()
            assert time.monotonic() < end, bytes(s.output[-1000:])
        with open(capture, "rb") as file:
            assert file.read() == b"FIRST red\nSECOND\n"
        with open(path_capture) as file:
            snapshot_path = file.read()
        end = time.monotonic() + 3
        while os.path.exists(snapshot_path):
            s.read()
            assert time.monotonic() < end, snapshot_path
        s.send(b"printf 'SOURCE_%s\\n' \"$KEEP\"\n")
        s.expect(b"SOURCE_semantic")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()

    # Exercise fallback boundaries in a fresh shell whose command echo has not
    # been changed by the exact OSC 133 scenario above.
    os.remove(capture)
    os.remove(path_capture)
    fallback = Session(extra_env=editor_env)
    try:
        fallback.expect(b"RUSTMUX_READY>")
        fallback.send(b"printf 'FALLBACK one\\nFALLBACK two\\n'\n")
        fallback.expect(b"RUSTMUX_READY>")
        fallback.send(b"\x02e")
        end = time.monotonic() + 8
        while not (os.path.exists(capture) and os.path.exists(path_capture)):
            fallback.read()
            assert time.monotonic() < end, bytes(fallback.output[-1000:])
        with open(capture, "rb") as file:
            exported = file.read()
        assert exported == b"FALLBACK one\nFALLBACK two", exported
        with open(path_capture) as file:
            snapshot_path = file.read()
        end = time.monotonic() + 3
        while os.path.exists(snapshot_path):
            fallback.read()
            assert time.monotonic() < end, snapshot_path
        fallback.send(b"exit 0\n")
        fallback.finish(0)
    finally:
        fallback.close()


# Without shell integration, the shell process cwd supplies the inherited directory.
with tempfile.TemporaryDirectory(prefix="rustmux-process-cwd-") as directory:
    target = os.path.join(directory, "cwd without osc")
    os.mkdir(target)
    quoted = shlex.quote(target)
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(("cd " + quoted + " && printf 'PROCESS_CWD_READY\\n'\n").encode())
        s.expect(b"PROCESS_CWD_READY")
        s.send(b"\x02c")
        s.expect(b"RUSTMUX_READY>")
        s.send(("test \"$PWD\" = " + quoted + " && printf 'PROCESS_CWD_OK\\n'\n").encode())
        s.expect(b"PROCESS_CWD_OK")
        s.send(b"exit 0\n")
        s.expect(b"RUSTMUX_READY>")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()


# OSC 7 directories are percent-decoded, isolated in pane metadata and inherited by new shells.
with tempfile.TemporaryDirectory(prefix="rustmux-osc7-") as directory:
    target = os.path.join(directory, "cwd with spaces")
    os.mkdir(target)
    encoded = target.replace("%", "%25").replace(" ", "%20")
    quoted = shlex.quote(target)
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; KEEP=source\n")
        s.expect(b"RUSTMUX_READY>")
        s.send((
            "printf '\\033]7;file://localhost%s\\007' " + shlex.quote(encoded) + "; "
            "printf 'OSC7_READY\\n'\n"
        ).encode())
        s.expect(b"OSC7_READY")
        s.send(b"printf '\\033]7;file://localhost%s\\007' '/tmp/%GG'; printf 'BAD_OSC7_DONE\\n'\n")
        s.expect(b"BAD_OSC7_DONE")

        s.send(b"\x02c")
        s.expect(b"RUSTMUX_READY>")
        s.send(("test \"$PWD\" = " + quoted + " && printf 'WINDOW_OSC7_OK\\n'\n").encode())
        s.expect(b"WINDOW_OSC7_OK")

        # The inherited directory is initial pane metadata, so a split made before
        # that shell emits its own OSC 7 still inherits the same directory.
        s.send(b"\x02%")
        s.expect(b"RUSTMUX_READY>")
        s.send(("test \"$PWD\" = " + quoted + " && printf 'SPLIT_OSC7_OK\\n'\n").encode())
        s.expect(b"SPLIT_OSC7_OK")
        assert s.physical_rows[1].count("┌".encode()) == 2, s.physical_rows
        s.send(b"exit 0\n")
        # The surviving pane already has a prompt, so another rendered prompt
        # does not prove the split pane has closed. Wait for the layout change
        # before routing the next exit to the surviving pane.
        deadline = time.monotonic() + 8
        while not s.physical_rows or s.physical_rows[1].count("┌".encode()) != 1:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"exit 0\n")
        expect_bar_without(s, b"2 shell")
        expect_bar(s, b"1 shell")
        s.send(b"printf 'SOURCE_%s\\n' \"$KEEP\"\n")
        s.expect(b"SOURCE_source")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()

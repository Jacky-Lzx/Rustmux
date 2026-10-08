"""Display metadata changes footer/help only; real physical and clicked actions survive."""
import base64
import fcntl
from pathlib import Path
import re
import struct
import subprocess
import tempfile
import termios
import time

from terminal_loop_support import BINARY, Session, expect_bar, expect_footer

CONFIG = '''
clear_defaults=true
[keybinds.locked]
"Ctrl b"={actions=[{action="switch-mode",mode="normal"}],display="always"}
[keybinds.normal]
N={actions=["new-window",{action="switch-mode",mode="locked"}],display="help"}
D={actions=["new-pane-down",{action="switch-mode",mode="locked"}],display="hidden"}
H={actions=[{action="switch-mode",mode="history"}],display="always"}
P={actions=[{action="switch-mode",mode="pane"}],display="always"}
T={actions=[{action="switch-mode",mode="tab"}],display="always"}
"?"={actions=["show-help",{action="switch-mode",mode="locked"}],display="always"}
esc={actions=[{action="switch-mode",mode="locked"}],display="help"}
[keybinds.pane]
r={actions=["new-pane-right",{action="switch-mode",mode="locked"}],display="help"}
up={actions=["focus-up"],display="always"}
"?"={actions=["show-help"],display="always"}
esc={actions=[{action="switch-mode",mode="locked"}],display="always"}
[keybinds.tab]
n={actions=["new-window"],display="help"}
"?"={actions=["show-help"],display="always"}
esc={actions=[{action="switch-mode",mode="locked"}],display="always"}
[keybinds.history]
H={actions=["show-help"],display="always"}
y={actions=["copy-history"],display="help"}
q={actions=[{action="switch-mode",mode="locked"}],display="always"}
"/"={actions=["history-search-forward"],display="hidden"}
'''


def wait(s, predicate):
    deadline = time.monotonic() + 5
    while not predicate():
        s.read()
        assert time.monotonic() < deadline, bytes(s.output[-2000:])


def body(s):
    return b"\n".join(s.physical_rows)


def click_label(s, label):
    wait(s, lambda: label in body(s))
    rows = [(row, text.index(label)) for row, text in enumerate(s.physical_rows) if label in text]
    assert len(rows) == 1, rows
    row, column = rows[0]
    column = len(s.physical_rows[row][:column].decode("utf-8"))
    # Labels here are ASCII and lie within the panel's whole-row click target.
    s.send(f"\x1b[<0;{column+1};{row+1}M\x1b[<0;{column+1};{row+1}m".encode())


with tempfile.TemporaryDirectory(prefix="rustmux-display-") as directory:
    config = Path(directory) / "config.toml"
    config.write_text('tab_name="title"\n' + CONFIG)
    s = Session(arguments=("--config", str(config)), lifetime=40)
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(b"\x02")
        expect_footer(s, b"NORMAL")
        assert b"New window" not in s.physical_rows[-1]
        assert b"Split down" not in s.physical_rows[-1]
        assert b"History" in s.physical_rows[-1]
        s.send(b"?")
        wait(s, lambda: b"Shortcut Help" in body(s) and b"New window" in body(s))
        assert b"Split down" not in body(s)
        # Paste and unknown keys stay local. Click executes the displayed remapped key.
        s.send(b"\x1b[200~ND\x1b[201~F")
        for _ in range(3): s.read()
        assert b"Shortcut Help" in body(s)
        s.send(b"\x1b")
        expect_footer(s, b"LOCKED")
        wait(s, lambda: b"Shortcut Help" not in body(s))
        s.send(b"\x02?")
        click_label(s, b"New window")
        expect_bar(s, b"2 shell")
        expect_footer(s, b"LOCKED")
        s.send(b"\x02D")  # Hidden binding remains active.
        expect_footer(s, b"LOCKED")
        s.send(b"printf 'HIDDEN_%s\\n' WORKS\n")
        s.expect(b"HIDDEN_WORKS")
        s.send(b"exit 0\n")  # Close split pane.
        wait(s, lambda: body(s).count(b"RUSTMUX_READY>") == 1)
        s.send(b"exit 0\n")  # Close window 2.
        wait(s, lambda: b"2 shell" not in s.physical_rows[0])
        s.send(b"\x02P?")
        wait(s, lambda: b"Shortcut Help" in body(s) and b"Split right" in body(s))
        assert b"New window" not in body(s)
        s.send(b"\x1b[A")  # Arrow returns to original Pane mode and dispatches there.
        expect_footer(s, b"PANE")
        assert b"Shortcut Help" not in body(s)
        s.send(b"?")
        wait(s, lambda: b"Shortcut Help" in body(s))
        s.send(b"\x1b")
        expect_footer(s, b"PANE")
        s.send(b"?")
        click_label(s, b"Split right")
        expect_footer(s, b"LOCKED")
        wait(s, lambda: body(s).count(b"RUSTMUX_READY>") == 2)
        s.send(b"exit 0\n")
        wait(s, lambda: body(s).count(b"RUSTMUX_READY>") == 1 and
             b"Shortcut Help" not in body(s))
        # Tab's help action originally switches the input to Locked; retain its origin.
        s.send(b"\x02T?")
        wait(s, lambda: b"Shortcut Help" in body(s) and b"New window" in body(s))
        s.send(b"n")
        expect_bar(s, b"2 shell")
        expect_footer(s, b"TAB")
        s.send(b"\x1b")
        expect_footer(s, b"LOCKED")
        s.send(b"exit 0\n")
        wait(s, lambda: b"2 shell" not in s.physical_rows[0])
        s.send(b"\x02H")
        expect_footer(s, b"HISTORY")
        assert b"Copy" not in s.physical_rows[-1]
        s.send(b"H")
        wait(s, lambda: b"Shortcut Help" in body(s) and b"Copy" in body(s))
        assert b"Search forward" not in body(s)
        s.send(b"y")
        wait(s, lambda: b"\x1b]52;c;" in s.output)
        expect_footer(s, b"HISTORY")
        wait(s, lambda: b"Shortcut Help" not in body(s))
        s.send(b"/")  # Hidden search continues to work, and H is now query text.
        s.send(b"H")
        expect_footer(s, b"/H")
        assert b"Shortcut Help" not in body(s)
        s.send(b"\x03q")
        expect_footer(s, b"LOCKED")
        s.send(b"printf 'DISPLAY_%s\\n' SURVIVED\n")
        s.expect(b"DISPLAY_SURVIVED")
        # Display-only reload applies with the same startup parser and keeps the binding.
        config.write_text('tab_name="title"\n' + CONFIG.replace('H={actions=[{action="switch-mode",mode="history"}],display="always"}', 'H={actions=[{action="switch-mode",mode="history"}],display="hidden"}'))
        deadline = time.monotonic() + 5
        while True:
            s.send(b"\x02")
            expect_footer(s, b"NORMAL")
            if b"History" not in s.physical_rows[-1]: break
            s.send(b"\x1b")
            expect_footer(s, b"LOCKED")
            s.read(seconds=0.1)
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"H")
        expect_footer(s, b"HISTORY")
        s.send(b"q")
        expect_footer(s, b"LOCKED")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()


# Load the maintained development example itself: synthetic bindings above cannot
# catch a working action that was accidentally omitted from the shipped config.
with tempfile.TemporaryDirectory(prefix="rustmux-dev-config-") as directory:
    root = Path(directory)
    config = Path(__file__).resolve().parent.parent / "examples/config-dev.toml"
    checked = subprocess.run([BINARY, "check-config", "--config", str(config), "--strict"],
                             capture_output=True, text=True, timeout=8)
    assert checked.returncode == 0, checked
    capture = root / "capture"
    editor = root / "editor.sh"
    editor.write_text('#!/bin/sh\ncp "$1" "$CAPTURE"\nprintf "DEV_EDITOR_RUNNING\\n"\nread reply\n')
    editor.chmod(0o700)
    s = Session(arguments=("--config", str(config)), lifetime=40,
                extra_env={"VISUAL": "", "EDITOR": str(editor), "CAPTURE": str(capture)})
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; KEEP=dev; printf 'DEV_CONFIG_%s\\n' READY\n")
        s.expect(b"DEV_CONFIG_READY")
        for enter, mode, label, normal in [
            (b"\x10", b"PANE", b"Split right", b"p"),
            (b"r", b"RESIZE", b"Resize left", b"r"),
            (b"\x16", b"MOVE", b"Move left", b"m"),
        ]:
            s.send(b"\x02" + enter + b"?")
            wait(s, lambda: b"Shortcut Help" in body(s) and label in body(s))
            assert b"Help" not in s.physical_rows[-1]  # display=help omits footer.
            s.send(b"\x1b")
            wait(s, lambda: b"Shortcut Help" not in body(s))
            expect_footer(s, mode)
            s.send(b"?")
            wait(s, lambda: b"Shortcut Help" in body(s) and label in body(s))
            s.send(normal)  # The panel dispatches to the mode that opened it.
            wait(s, lambda: b"Shortcut Help" not in body(s))
            expect_footer(s, b"NORMAL")
            s.send(b"\x1b")
            expect_footer(s, b"LOCKED")

        # Wide footer exposes both display-only editor entries. Their defaults
        # must still distinguish selection word motion from output editing.
        fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 200, 0, 0))
        s.send(b"printf '\\033[3J\\033[2J\\033[H\\033]133;C\\007FIRST SECOND\\n\\033]133;D;0\\007'\n")
        s.expect(b"FIRST SECOND")
        s.send(b"\x02s")
        expect_footer(s, b"HISTORY")
        wait(s, lambda: b"Edit history" in s.physical_rows[-1]
             and b"Edit output / word" in s.physical_rows[-1])
        s.output.clear()
        s.send(b"gvey")
        wait(s, lambda: b"\x1b]52;c;" in s.output)
        clips = re.findall(rb"\x1b\]52;c;([^\x07]*)\x07", s.output)
        assert [base64.b64decode(value) for value in clips] == [b"FIRST"]
        assert not capture.exists(), "selection e unexpectedly opened an editor"
        expect_footer(s, b"HISTORY")
        s.send(b"E")
        s.expect(b"DEV_EDITOR_RUNNING")
        assert b"FIRST SECOND" in capture.read_bytes()
        s.send(b"\r")
        wait(s, lambda: b"2 history" not in s.physical_rows[0])
        expect_footer(s, b"LOCKED")
        capture.unlink()
        s.frames.clear()
        s.send(b"\x02se")
        s.expect(b"DEV_EDITOR_RUNNING")
        assert capture.read_bytes() == b"FIRST SECOND\n"
        s.send(b"\r")
        wait(s, lambda: b"2 output" not in s.physical_rows[0])
        expect_footer(s, b"LOCKED")
        s.send(b"printf 'DEV_CONFIG_%s_%s\\n' SURVIVED \"$KEEP\"\n")
        s.expect(b"DEV_CONFIG_SURVIVED_dev")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()

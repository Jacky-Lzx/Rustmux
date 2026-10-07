"""History output actions preserve modal input, snapshots and editor cleanup."""
import base64
import os
from pathlib import Path
import re
import shlex
import tempfile
import time

from terminal_loop_support import Session, expect_bar, expect_bar_without, expect_footer

CONFIG = '''
[keybinds.normal]
esc = { actions = [{action="switch-mode", mode="locked"}] }
s = { actions = [{action="switch-mode", mode="history"}] }
[keybinds.history]
E = { actions = ["scroll-bottom", {action="switch-mode", mode="locked"}, "edit-history"] }
e = { actions = ["scroll-bottom", {action="switch-mode", mode="locked"}, "edit-last-output"] }
Y = { actions = ["copy-last-output"] }
y = { actions = ["copy-last-output", "scroll-bottom", {action="switch-mode", mode="locked"}] }
c = { actions = ["copy-history", {action="switch-mode", mode="normal"}] }
q = { actions = [{action="switch-mode", mode="locked"}] }
esc = { actions = [{action="switch-mode", mode="locked"}] }
'''


def wait(session, predicate, seconds=5):
    deadline = time.monotonic() + seconds
    while not predicate():
        session.read()
        assert time.monotonic() < deadline, bytes(session.output[-1500:])


def clips(session):
    return [base64.b64decode(data) for data in re.findall(rb"\x1b\]52;c;([^\x07]*)\x07", session.output)]


with tempfile.TemporaryDirectory(prefix="rustmux-history-actions-") as temporary:
    root = Path(temporary)
    config = root / "config.toml"
    config.write_text(CONFIG)
    capture, filename = root / "capture", root / "filename"
    editor = root / "editor.sh"
    editor.write_text('#!/bin/sh\ncp "$1" "$CAPTURE"\nprintf "%s" "$1" > "$FILENAME"\nprintf "EDITOR_RUNNING\\n"\nread reply\n')
    editor.chmod(0o700)
    env = {"VISUAL": "", "EDITOR": str(editor), "CAPTURE": str(capture), "FILENAME": str(filename)}

    s = Session(arguments=("--config", str(config)), extra_env=env, lifetime=30)
    try:
        s.expect(b"RUSTMUX_READY> ")
        trigger = root / "trigger"
        # The late record completes while browsing. Clipboard output must still be FIRST.
        command = (
            "stty -echo; KEEP=source; printf '\\033]133;C\\007'; "
            "printf 'FIRST \\033[31mred\\033[0m 中\\n'; printf '\\033]133;D;0\\007'; "
            "(while [ ! -f " + shlex.quote(str(trigger)) + " ]; do sleep 0.02; done; "
            "printf '\\033]133;C\\007SECOND %s\\n\\033]133;D;0\\007' record) &\n"
        )
        s.send(command.encode())
        s.expect("FIRST red 中".encode())
        s.send(b"\x02s")
        expect_footer(s, b"HISTORY")
        # Action-looking query and pasted text stay local.
        s.send(b"/EeyY\x1b[200~EeyY\x1b[201~")
        expect_footer(s, b"EeyYEeyY")
        assert not clips(s) and not capture.exists()
        s.send(b"\x03")
        expect_footer(s, b"HISTORY")
        trigger.touch()
        # Background output remains hidden by the frozen History screen.
        for _ in range(8):
            s.read(seconds=0.03)
        assert b"SECOND record" not in b"".join(s.physical_rows)
        s.output.clear()
        s.send(b"Y")
        wait(s, lambda: len(clips(s)) == 1)
        assert clips(s) == ["FIRST red 中\n".encode()]
        expect_footer(s, b"HISTORY")
        s.send(b"y")
        wait(s, lambda: len(clips(s)) == 2)
        assert clips(s) == ["FIRST red 中\n".encode()] * 2
        expect_footer(s, b"LOCKED")
        s.send(b"\x02se")
        expect_bar(s, b"2 output")
        wait(s, lambda: capture.exists() and filename.exists())
        assert capture.read_bytes() == b"SECOND record\n"
        snapshot = Path(filename.read_text())
        assert snapshot.stat().st_mode & 0o777 == 0o600
        s.send(b"\r")
        expect_bar_without(s, b"2 output")
        wait(s, lambda: not snapshot.exists())
        capture.unlink()
        filename.unlink()
        s.send(b"\x02sE")
        expect_bar(s, b"2 history")
        wait(s, lambda: capture.exists() and filename.exists())
        assert b"SECOND record" in capture.read_bytes()
        snapshot = Path(filename.read_text())
        s.send(b"\r")
        expect_bar_without(s, b"2 history")
        wait(s, lambda: not snapshot.exists())
        s.send(b"\x02svc")  # Copy selection then enter NORMAL.
        expect_footer(s, b"NORMAL")
        s.send(b"\x1b")
        expect_footer(s, b"LOCKED")
        s.send(b"printf 'SOURCE_%s\\n' \"$KEEP\"\n")
        s.expect(b"SOURCE_source")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()

    # Missing output and over-limit output cannot exit or write a partial OSC 52.
    for oversized in (False, True):
        s = Session(arguments=("--config", str(config)), lifetime=20)
        try:
            s.expect(b"RUSTMUX_READY> ")
            if oversized:
                s.send(b"stty -echo; printf '\\033]133;C\\007'; head -c 33000 /dev/zero | tr '\\000' x; printf '\\033]133;D;0\\007\\n'; printf 'LARGE_%s\\n' DONE\n")
                s.expect(b"LARGE_DONE")
            s.send(b"\x02sy")
            expect_footer(s, b"Copy too large" if oversized else b"No command output")
            assert not clips(s)
            expect_footer(s, b"HISTORY")
            s.send(b"q")
            expect_footer(s, b"LOCKED")
            s.send(b"exit 0\n")
            s.finish(0)
        finally:
            s.close()

    # A failing editor closes its window and preserves the original pane.
    failed_editor = root / "failed-editor.sh"
    failed_editor.write_text("#!/bin/sh\nprintf 'EDITOR_FAILED_START\\n'\nsleep 0.05\nexit 7\n")
    failed_editor.chmod(0o700)
    s = Session(arguments=("--config", str(config)), extra_env={"VISUAL": "", "EDITOR": str(failed_editor)})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02s")
        expect_footer(s, b"HISTORY")
        s.send(b"E")
        s.expect(b"EDITOR_FAILED_START")
        expect_footer(s, b"LOCKED")
        expect_bar_without(s, b"2 history")
        s.send(b"printf 'EDITOR_FAILURE_%s\\n' SURVIVED\n")
        s.expect(b"EDITOR_FAILURE_SURVIVED")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()

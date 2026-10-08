"""A prompt repaint must not publish its temporary carriage-return cursor."""
import os
from pathlib import Path
import re
import shlex
import sys
import tempfile
import termios
import time
import tty


if "--child" in sys.argv:
    original = termios.tcgetattr(0)
    tty.setraw(0)
    try:
        os.write(1, b"\x1b[2J\x1b[1;20HCOALESCE_READY\x1b[2;1H< mode_probe\x1b[2;8H")
        for index in range(12):
            assert os.read(0, 1) == b"x"
            os.write(1, b"\r\x1b[6n")
            reply = bytearray()
            while not reply.endswith(b"R"):
                reply.extend(os.read(0, 1))
            assert reply == b"\x1b[2;1R", reply
            # Wait for a model query, not a sleep: this forces CR and the repaint
            # into separate PTY reads while queries remain live during batching.
            marker = f"FRAME_{index:02d}".encode()
            symbol = b">" if index % 2 == 0 else b"<"
            os.write(1, b"\r" + symbol + b" mode_probe\x1b[1;40H" + marker + b"\x1b[2;8H")
        assert os.read(0, 1) == b"q"
    finally:
        termios.tcsetattr(0, termios.TCSANOW, original)
    sys.exit(0)


from terminal_loop_support import BINARY, Session


with tempfile.TemporaryDirectory(prefix="rustmux-coalescing-") as temporary:
    config = Path(temporary) / "config.toml"
    for setting in ("", "idle_frame_coalescing=false", "idle_frame_coalescing=true"):
        config.write_text(setting + '\ntab_name="title"\n')
        session = Session(arguments=("--config", str(config)), lifetime=30)
        try:
            session.expect(b"RUSTMUX_READY> ")
            command = " ".join(shlex.quote(arg) for arg in
                               (sys.executable, __file__, BINARY, "--child"))
            session.send(command.encode() + b"\n")
            session.expect(b"COALESCE_READY")
            for index in range(12):
                # Each repaint starts after the previous frame cadence has expired.
                time.sleep(0.025)
                session.send(b"x")
                output = session.expect(f"FRAME_{index:02d}".encode())
                cursors = [(int(row), int(column)) for row, column in
                           re.findall(rb"\x1b\[([0-9]+);([0-9]+)H\x1b\[\?25h", output)]
                assert (4, 9) in cursors, (setting, index, cursors, output)
                if setting == "idle_frame_coalescing=true":
                    assert (4, 2) not in cursors, (index, cursors, output)
            session.send(b"q")
            session.expect(b"RUSTMUX_READY> ")
            session.send(b"exit\n")
            session.finish(0)
        finally:
            session.close()

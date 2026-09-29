"""Run the real binary inside an outer PTY and inspect the outer termios."""
import errno
import fcntl
import json
import os
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import tempfile
import time
import unicodedata

if sys.argv[1] == "--supervisor":
    # Keep the outer session leader alive while inspecting restored termios.
    # macOS revokes the slave when its controlling session leader exits.
    report = int(sys.argv[3])
    original = termios.tcgetattr(0)
    app = subprocess.Popen([sys.argv[2], *sys.argv[4:]])
    os.write(report, (json.dumps({"pid": app.pid}) + "\n").encode())
    try:
        code = app.wait(timeout=12)
    except subprocess.TimeoutExpired:
        app.kill()
        code = app.wait()
    current = termios.tcgetattr(0)
    # PENDIN is kernel-maintained pending-input state, not a user mode setting.
    original[3] &= ~getattr(termios, "PENDIN", 0)
    current[3] &= ~getattr(termios, "PENDIN", 0)
    restored = current == original
    os.write(report, (json.dumps({"restored": restored, "before": repr(original), "after": repr(termios.tcgetattr(0))}) + "\n").encode())
    sys.exit(code)

BINARY = sys.argv[1]
DEFAULT_CONFIG_DIR = tempfile.TemporaryDirectory(prefix="rustmux-test-default-config-")

class Session:
    def __init__(self, shell="/bin/sh", extra_env=None, arguments=(), pixels=(0, 0)):
        self.master, self.slave = os.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, *pixels))
        self.original = termios.tcgetattr(self.slave)
        env = dict(os.environ, RUSTMUX_SHELL=shell, PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="",
                   XDG_CONFIG_HOME=DEFAULT_CONFIG_DIR.name)
        if extra_env:
            env.update(extra_env)
        def child_setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        read_report, write_report = os.pipe()
        self.child = subprocess.Popen(
            [sys.executable, __file__, "--supervisor", BINARY, str(write_report), *arguments],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, env=env,
            preexec_fn=child_setup, pass_fds=(write_report,))
        os.close(write_report)
        self.report = os.fdopen(read_report)
        self.app_pid = json.loads(self.report.readline())["pid"]
        os.set_blocking(self.master, False)
        self.output = bytearray()
        self.frame_pending = bytearray()
        self.frames = []
        self.physical_rows = []
        self.last_rows = []
        self.last_frame = b""
        self.cursor_shape = None
        self.private_modes = {}

    def read(self, seconds=0.05):
        if select.select([self.master], [], [], seconds)[0]:
            try:
                chunk = os.read(self.master, 65536)
                self.output.extend(chunk)
                self.frame_pending.extend(chunk)
                # A frame ends in cursor positioning plus its visibility mode. Decode rows
                # payloads independently of the Rust parser; SGR does not occupy cells.
                while (match := re.search(rb"\x1b\[[0-9]+;[0-9]+H\x1b\[\?25[hl]", self.frame_pending)):
                    end = match.end()
                    frame = bytes(self.frame_pending[:end])
                    del self.frame_pending[:end]
                    if b"\x1b[?25l" not in frame:
                        continue
                    self.last_frame = frame
                    for mode in re.finditer(rb"\x1b\[\?([0-9;]+)([hl])", frame):
                        for number in mode.group(1).split(b";"):
                            self.private_modes[int(number)] = mode.group(2) == b"h"
                    for shape in re.finditer(rb"\x1b\[([0-6]) q", frame):
                        self.cursor_shape = int(shape.group(1))
                    # Drawing CUPs replace cell spans. The final CUP only positions
                    # the cursor; retain all untouched cells and rows.
                    positions = list(re.finditer(rb"\x1b\[([0-9]+);([0-9]+)H", frame))
                    height = struct.unpack("HHHH", fcntl.ioctl(self.slave, termios.TIOCGWINSZ, b"\0" * 8))[0]
                    height = height or len(self.last_rows)
                    rows = (self.physical_rows + [b""] * height)[:height]
                    for pos, following in zip(positions, positions[1:]):
                        row = int(pos.group(1)) - 1
                        if row < height:
                            def cells(text):
                                result = []
                                # This simplified incremental-frame reconstruction
                                # can observe an incomplete scalar at a frame edge.
                                # Keep the harness progressing; Rustmux's parser has
                                # separate malformed/incomplete UTF-8 coverage.
                                for character in text.decode("utf-8", errors="replace"):
                                    if unicodedata.combining(character):
                                        index = len(result) - 1
                                        while index >= 0 and result[index] is None:
                                            index -= 1
                                        if index >= 0:
                                            result[index] += character
                                    else:
                                        result.append(character)
                                        if unicodedata.east_asian_width(character) in ("W", "F"):
                                            result.append(None)
                                return result
                            column = int(pos.group(2)) - 1
                            payload = frame[pos.end():following.start()]
                            replacement = cells(re.sub(rb"\x1b\[[0-9;:]*m", b"", payload))
                            previous = cells(rows[row])
                            length = column + len(replacement)
                            previous += [" "] * max(0, length - len(previous))
                            previous[column:length] = replacement
                            rows[row] = "".join(c for c in previous if c is not None).rstrip(" ").encode()
                    self.physical_rows = rows
                    self.last_rows = self.logical_rows(rows)
                    self.frames.append(self.last_rows)
                    self.frames = self.frames[-64:]
            except OSError as error:
                if error.errno not in (errno.EIO, errno.EAGAIN):
                    raise

    @staticmethod
    def logical_rows(rows):
        """Expose pane contents without the outer frame to existing assertions."""
        if len(rows) < 4 or not rows[1].startswith("┌".encode()):
            return rows
        bottom = len(rows) - 1
        if not rows[bottom].startswith("└".encode()):
            bottom -= 1
        if bottom <= 1 or not rows[bottom].startswith("└".encode()):
            return rows
        result = [rows[0]]
        border = set("│├┤┼" )
        for raw in rows[2:bottom]:
            text = raw.decode("utf-8").rstrip(" ")
            if text and text[0] in border:
                text = text[1:]
            text = text.rstrip(" │├┤┼")
            result.append(text.encode())
        result.extend([b""] * (len(rows) - len(result)))
        return result

    def expect(self, text):
        def matches():
            target = text.strip(b"\r\n")
            for rows in self.frames:
                if text.startswith(b"\r\n"):
                    if target in rows:
                        return True
                elif target == b"RUSTMUX_READY> ":
                    # Alternate-screen restoration can leave stale cells on and
                    # below the prompt row, confusing the simplified row cache.
                    # Match the renderer's exact prompt write instead; an echoed
                    # "prompt + command" has command bytes before the SGR reset.
                    prompt = (rb"\x1b\[[0-9]+;[0-9]+H(?:\x1b\[[0-9;:]*m)*"
                              + re.escape(target.rstrip()) + rb" *\x1b\[0m")
                    content = rows[1:] if len(rows) > 1 else rows
                    if (any(row.rstrip() == target.rstrip() for row in content)
                            or re.search(prompt, self.output)):
                        return True
                elif any(target in row for row in rows):
                    return True
            return False
        end = time.monotonic() + 8
        while not matches():
            self.read()
            if time.monotonic() > end:
                raise AssertionError((text, self.last_rows, bytes(self.output[-1000:]), self.child.poll()))
        matched_output = bytes(self.output)
        self.output.clear()
        self.frames.clear()
        return matched_output

    def send(self, data):
        end = time.monotonic() + 8
        while data:
            try:
                n = os.write(self.master, data)
                data = data[n:]
            except BlockingIOError:
                self.read()
            assert time.monotonic() < end, "input stalled"

    def finish(self, expected):
        end = time.monotonic() + 8
        while self.child.poll() is None:
            self.read()
            assert time.monotonic() < end, "process did not exit"
        self.read(0)
        assert self.child.returncode == expected, (self.child.returncode, self.output[-2000:])
        report = json.loads(self.report.readline())
        assert report["restored"], report

    def close(self):
        # Release the PTY before waiting; macOS can block exit on terminal drain.
        os.close(self.master)
        os.close(self.slave)
        if self.child.poll() is None:
            try:
                os.kill(self.app_pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                try:
                    os.kill(self.app_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.child.kill()
                self.child.wait()
        self.report.close()


# Shared helpers for the window, pane, and session scenarios.
background_query = r"""
import os, select, time, tty
tty.setraw(0)
os.write(1, b"\x1b[2J\x1b[HQUERY_WAIT")
time.sleep(0.3)
os.write(1, b"\x1b[4;5H\x1b[6n")
data = bytearray()
end = time.monotonic() + 5
while len(data) < len(b"\x1b[4;5R"):
    assert time.monotonic() < end, repr(data)
    if select.select([0], [], [], 0.1)[0]:
        data.extend(os.read(0, 1))
assert data == b"\x1b[4;5R", repr(data)
os.write(1, b"\x1b[2J\x1b[HQUERY_BG_OK")
assert select.select([0], [], [], 5)[0]
assert os.read(0, 1) == b"x"
"""

def expect_bar(session, marker):
    end = time.monotonic() + 3
    while not session.last_rows or marker not in session.last_rows[0]:
        session.read()
        assert time.monotonic() < end, session.last_rows

def expect_bar_without(session, marker):
    end = time.monotonic() + 3
    while not session.last_rows or marker in session.last_rows[0]:
        session.read()
        assert time.monotonic() < end, session.last_rows

def expect_emitted_footer(session, marker):
    # The harness removes the pane frame and footer from logical rows. Match the
    # emitted terminal bytes so the footer remains covered by the real PTY test.
    end = time.monotonic() + 8
    while marker not in session.output:
        session.read()
        assert time.monotonic() < end, (marker, session.last_rows, session.output[-1000:])
    session.output.clear()
    session.frames.clear()

def expect_footer(session, marker):
    end = time.monotonic() + 3
    while not session.physical_rows or marker not in session.physical_rows[-1]:
        session.read()
        assert time.monotonic() < end, session.physical_rows

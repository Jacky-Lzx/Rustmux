"""Run the real binary inside an outer PTY and inspect the outer termios."""
import errno
import base64
import fcntl
import json
import os
import re
import select
import shlex
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

class Session:
    def __init__(self, shell="/bin/sh", extra_env=None, arguments=()):
        self.master, self.slave = os.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        env = dict(os.environ, RUSTMUX_SHELL=shell, PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
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
                                for character in text.decode("utf-8"):
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
                            replacement = cells(re.sub(rb"\x1b\[[0-9;]*m", b"", payload))
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
        if len(rows) < 4 or not rows[1].startswith("┌".encode()) or not rows[-1].startswith("└".encode()):
            return rows
        result = [rows[0]]
        border = set("│├┤┼" )
        for raw in rows[2:-1]:
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
                    nonempty = [row for row in (rows[1:] if len(rows) > 1 else rows) if row]
                    if nonempty and target.rstrip() in nonempty[-1]:
                        return True
                elif any(target in row for row in rows):
                    return True
            return False
        end = time.monotonic() + 8
        while not matches():
            self.read()
            if time.monotonic() > end:
                raise AssertionError((text, self.last_rows, bytes(self.output[-1000:]), self.child.poll()))
        self.output.clear()
        self.frames.clear()

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

s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    raw = termios.tcgetattr(s.slave)
    assert not raw[3] & (termios.ECHO | termios.ICANON | termios.ISIG)
    # Pane children carry an environment marker. Interactive nested entry fails
    # before touching terminal modes and returns control to the existing shell.
    nested = shlex.quote(BINARY) + "; printf 'NESTED_STATUS:%s\\n' \"$?\"\n"
    s.send(nested.encode())
    s.expect(b"NESTED_STATUS:1")
    assert any(b"rustmux: nested Rustmux sessions are not supported" in row
               for row in s.last_rows), s.last_rows
    assert any(b"RUSTMUX_READY>" in row for row in s.last_rows), s.last_rows
    s.send("printf '\\n%s\\n' '中文输入'\n".encode())
    s.expect("\r\n中文输入\r\n".encode())
    s.send(b"printf '\\n%s\\n' 'backspacX\x7fe'\n")
    s.expect(b"\r\nbackspace\r\n")
    s.send(b"sleep 30\n")
    s.expect(b"sleep 30\r\n")
    time.sleep(0.1)
    s.send(b"\x03")
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"printf '\\nINT:%s\\n' \"$?\"\n")
    s.expect(b"\r\nINT:130\r\n")
    # A foreground process must receive the kernel's SIGWINCH and see the new size.
    s.send(b"python3 -c 'import os,signal; signal.signal(signal.SIGWINCH, lambda *_: print(\"SIZE:%s:%s\" % (os.get_terminal_size().lines, os.get_terminal_size().columns), flush=True)); print(\"WATCH_READY\", flush=True); exec(\"while True: signal.pause()\")'\n")
    s.expect(b"\r\nWATCH_READY\r\n")
    for rows, columns in [(40, 120), (18, 60), (55, 150)]:
        fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        s.expect(f"SIZE:{max(1, rows - 3)}:{max(1, columns - 2)}\r\n".encode())
    # Invalid transient dimensions must not terminate Rustmux or reach the child.
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 0, 0, 0, 0))
    s.read(0.15)
    assert b"SIZE:0:0" not in s.output
    assert s.child.poll() is None
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    s.expect(b"SIZE:21:78\r\n")
    s.send(b"\x03")
    s.expect(b"RUSTMUX_READY> ")
    # Output larger than the grid must still be parsed through the final marker.
    s.send(b"python3 -c 'import os; os.write(1, b\"Z\" * 200000); print(\"BURST_DONE\")'; printf '\\nLAST_OUTPUT\\n'; exit 7\n")
    s.finish(7)
    assert any(row.endswith(b"BURST_DONE") for row in s.last_rows), s.last_rows
    assert b"LAST_OUTPUT" in s.last_rows, s.last_rows
    assert b"\x1b[?1049l" in s.output
finally:
    s.close()

# Model operations must change the rendered screen, rather than pass through.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"printf '\\033[2J\\033[Habc\\033[1;2H\\033[31mX\\033[0m\\n'\n")
    s.expect(b"\r\naXc\r\n")
    assert b"\x1b[0;38;5;1mX" in s.last_frame, s.last_frame
    s.send(b"printf '\\033[?1049h\\033[HALTSCREEN'; read answer; printf '\\033[?1049l'\n")
    s.expect(b"\r\nALTSCREEN\r\n")
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")
    assert b"aXc" in s.last_rows, s.last_rows
    s.send(b"printf '\\033[?25l\\nHIDDEN_CURSOR\\n'; read answer; printf '\\033[?25h'\n")
    s.expect(b"\r\nHIDDEN_CURSOR\r\n")
    assert s.last_frame.endswith(b"\x1b[?25l"), s.last_frame
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")
    assert s.last_frame.endswith(b"\x1b[?25h")

    # Keep header/footer fixed while LF then RI scroll only the middle rows.
    s.send(b"stty -echo; printf '\\033[2J\\033[1;1HHEADER\\033[2;1HONE\\033[3;1HTWO"
           b"\\033[4;1HTHREE\\033[5;1HFOOTER\\033[2;4r\\033[4;1H\\nSCROLLED'; "
           b"read answer; printf '\\033[2;1H\\033MREVERSED'; read answer; stty echo; printf '\\033[r\\033[6;1H'\n")
    s.expect(b"\r\nSCROLLED\r\n")
    assert s.last_rows[1:6] == [b"HEADER", b"TWO", b"THREE", b"SCROLLED", b"FOOTER"], s.last_rows
    s.send(b"\n")
    s.expect(b"\r\nREVERSED\r\n")
    assert s.last_rows[1:6] == [b"HEADER", b"REVERSED", b"TWO", b"THREE", b"FOOTER"], s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    for command, expected in [
        (b"L", [b"HEADER", b"ONE", b"", b"TWO", b"FOOTER"]),
        (b"M", [b"HEADER", b"ONE", b"THREE", b"", b"FOOTER"]),
        (b"S", [b"HEADER", b"TWO", b"THREE", b"", b"FOOTER"]),
        (b"T", [b"HEADER", b"", b"ONE", b"TWO", b"FOOTER"]),
    ]:
        s.send(b"stty -echo; printf '\\033[2J\\033[1;1HHEADER\\033[2;1HONE"
               b"\\033[3;1HTWO\\033[4;1HTHREE\\033[5;1HFOOTER"
               b"\\033[2;4r\\033[3;2H\\033[" + command +
               b"\\033[6;1HLINE_EDIT_DONE'; read answer; "
               b"stty echo; printf '\\033[r\\033[6;1H'\n")
        s.expect(b"\r\nLINE_EDIT_DONE\r\n")
        assert s.last_rows[1:6] == expected, (command, s.last_rows)
        s.send(b"\n")
        s.expect(b"RUSTMUX_READY> ")

    for command, expected in [(b"@", b"ab cdefgh"), (b"P", b"abdefgh"), (b"X", b"ab defgh")]:
        s.send(b"stty -echo; printf '\\033[r\\033[2J\\033[1;1Habcdefgh"
               b"\\033[1;3H\\033[" + command +
               b"\\033[2;1HCHAR_EDIT_DONE'; read answer; "
               b"stty echo; printf '\\033[2;1H'\n")
        s.expect(b"\r\nCHAR_EDIT_DONE\r\n")
        assert s.last_rows[1] == expected, (command, s.last_rows)
        s.send(b"\n")
        s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[2;4r\\033[?6h"
           b"\\033[HORIGIN\\033[2d\\033[4GCOLUMN\\033[?6lABS"
           b"\\033[6;1HORIGIN_DONE'; read answer; "
           b"stty echo; printf '\\033[r\\033[6;1H'\n")
    s.expect(b"\r\nORIGIN_DONE\r\n")
    assert s.last_rows[1:4] == [b"ABS", b"ORIGIN", b"   COLUMN"], s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[1;1Habcdefgh"
           b"\\033[1;3H\\033[4hXY\\033[4lZ"
           b"\\033[2;1HINSERT_DONE'; read answer; "
           b"stty echo; printf '\\033[2;1H'\n")
    s.expect(b"\r\nINSERT_DONE\r\n")
    assert s.last_rows[1] == b"abXYZdefgh", s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[1;1HA\\033[1;80H"
           b"\\033[?7lBC\\033[?7hD\\033[3;1HWRAP_DONE'; read answer; "
           b"stty echo; printf '\\033[3;1H'\n")
    s.expect(b"\r\nWRAP_DONE\r\n")
    assert s.last_rows[1:3] == [b"A" + b" " * 76 + b"C", b"D"], s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[3g\\033[4G\\033H"
           b"\\033[12G\\033H\\033[H\\tA\\tB\\033[ZC"
           b"\\033[2;1HTABS_DONE'; read answer; "
           b"stty echo; printf '\\033[2;1H'\n")
    s.expect(b"\r\nTABS_DONE\r\n")
    assert s.last_rows[1] == b"   A       C", s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[H\\033(0lqqk"
           b"\\033(B\\033[2;1H\\033)0\\016x\\017q"
           b"\\033[3;1HCHARSET_DONE'; read answer; "
           b"stty echo; printf '\\033[3;1H'\n")
    s.expect(b"\r\nCHARSET_DONE\r\n")
    assert s.last_rows[1:3] == ["┌──┐".encode(), "│q".encode()], s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[?1049h\\033[31;44m"
           b"\\033[?25l\\033[4h\\033[3g\\033(0lqqk"
           b"\\033cRESET_DONE'; read answer; stty echo; printf '\\033[2;1H'\n")
    s.expect(b"\r\nRESET_DONE\r\n")
    assert s.last_rows[1] == b"RESET_DONE" and not any(s.last_rows[2:]), s.last_rows
    assert s.last_frame.endswith(b"\x1b[?25h"), s.last_frame
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[H\\033[31;44mKEPT"
           b"\\033[?25l\\033[4h\\033(0\\033[!pq"
           b"\\033[2;1HSOFT_RESET_DONE'; read answer; "
           b"stty echo; printf '\\033[3;1H'\n")
    s.expect(b"\r\nSOFT_RESET_DONE\r\n")
    assert s.last_rows[1] == b"KEPTq", s.last_rows
    assert s.last_frame.endswith(b"\x1b[?25h"), s.last_frame
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"exit\n")
    s.finish(0)
finally:
    s.close()


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
os.write(1, b"\x1b[2;3H\x1b[5n\x1b[6n")
receive(b"\x1b[0n\x1b[2;3R")
os.write(1, b"\x1b[3;10r\x1b[?6h\x1b[2;4H\x1b[6n")
receive(b"\x1b[2;4R")
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
def mode_flood():
    data = b"\x1b[?7$p\x1b[5n" * 10000
    while data:
        data = data[os.write(1, data):]
writer = threading.Thread(target=mode_flood)
writer.start()
time.sleep(0.1)
receive(b"\x1b[?7;1$y\x1b[0n" * 10000)
writer.join(timeout=2)
assert not writer.is_alive()
os.write(1, b"\x1b[c\x1b[0c\x1bZ")
receive(b"\x1b[?1;0c" * 3)
def identity_flood():
    data = b"\x1b[c" * 10000
    while data:
        data = data[os.write(1, data):]
writer = threading.Thread(target=identity_flood)
writer.start()
time.sleep(0.1)
receive(b"\x1b[?1;0c" * 10000)
writer.join(timeout=2)
assert not writer.is_alive()
# CSI s/u share the DEC save slot, and the restored position reaches real DSR.
os.write(1, b"\x1b[?6l\x1b[2;3H\x1b[s\x1b[7;8H\x1b[u\x1b[6n")
receive(b"\x1b[2;3R")
os.write(1, b"\x1b[3;4H\x1b7\x1b[H\x1b[u\x1b[6n")
receive(b"\x1b[3;4R")
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
        s.expect(b"\r\nREPLIES_OK\r\n")
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
os.write(1, b"\x1b=\x1b[2J\x1b[HAPP_PAD")
receive(b"\x1bOp\x1bOq\x1bOy\x1bOn\x1bOM")
os.write(1, b"\x1b[!p\x1b[2J\x1b[HNORMAL_PAD")
receive(b"019.\r")
os.write(1, b"\x1b=\x1b[2J\x1b[HEXIT_PAD")
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

# Physical mouse rows are translated past the top bar; other event fields are preserved.
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
os.write(1, b"\x1b[?2026h\x1b[2J\x1b[HPARTIAL_HIDDEN\x1b[?2026$p\x1b[c")
receive(b"\x1b[?2026;1$y\x1b[?1;0c")
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

# Real interactive windows: retain shell variables, background output and size.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"WIN=A; printf '\\033[2J\\033[H%s%s\\n' READY _A\n")
    s.expect(b"READY_A")
    s.send(b"sleep 0.2; printf '\\033[?2004h\\033[2J\\033[H%s%s\\n' BACK _A\n")
    s.send(b"\x02")
    s.send(b"c")
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"printf '\\033[2J\\033[H%s:%s\\n' WINDOW_B ${WIN-unset}\n")
    s.expect(b"WINDOW_B:unset")
    s.read(0.3)
    assert not any(b"BACK_A" in row for row in s.last_rows)
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 100, 0, 0))
    s.read(0.1)
    s.send(b"\x02p")
    s.expect(b"BACK_A")
    assert s.private_modes[2004]
    s.send(b"printf '\\n%s:%s:%s\\n' RETAINED $WIN \"$(stty size)\"\n")
    s.expect(b"RETAINED:A:37 98")
    s.send(b"\x02n")
    s.expect(b"WINDOW_B:unset")
    assert not s.private_modes[2004]
    s.send(b"exit 4\n")
    s.expect(b"RETAINED:A")
    s.send(b"printf '\\n%s%s\\n' LAST _WINDOW; exit 7\n")
    s.finish(7)
    assert any(b"LAST_WINDOW" in row for row in s.last_rows)
finally:
    s.close()

# Prefix escaping and bracketed paste containing window commands reach the child.
prefix_probe = r"""
import os, select, time, tty
tty.setraw(0)
os.write(1, b"\x1b[?2004h\x1b[2J\x1b[HPREFIX_READY")
expected = b"\x02n\x02q\x1b[200~paste\x02c\x02n\x02p\x021\x020\x02\t\x02&\x02<\x02>\x1b[201~"
data = bytearray()
end = time.monotonic() + 5
while len(data) < len(expected):
    assert time.monotonic() < end, repr(data)
    if select.select([0], [], [], 0.1)[0]:
        data.extend(os.read(0, len(expected) - len(data)))
assert data == expected, repr(data)
os.write(1, b"\x1b[2J\x1b[HPREFIX_PASSED")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(prefix_probe)
        source.flush()
        s.send(("exec python3 " + shlex.quote(source.name) + "\n").encode())
        s.expect(b"PREFIX_READY")
        s.send(b"\x02\x02n\x02q\x1b[20")
        s.send(b"0~paste\x02c\x02n\x02p\x021\x020\x02\t\x02&\x02<\x02>\x1b[201~")
        s.finish(0)
        assert any(b"PREFIX_PASSED" in row for row in s.last_rows)
finally:
    s.close()

# A later spawn failure must not close or replace the existing window.
with tempfile.TemporaryDirectory(prefix="rustmux-window-spawn-") as directory:
    shell = os.path.join(directory, "shell")
    with open(shell, "w") as source:
        source.write('#!/bin/sh\nrm -- "$0"\nexport PS1="RUSTMUX_READY> "\nexec /bin/sh -i\n')
    os.chmod(shell, 0o700)
    s = Session(shell=shell)
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b'\x02c\x02%\x02"')
        s.send(b"printf '\\n%s%s\\n' SPAWN_ SURVIVED; exit 0\n")
        s.finish(0)
        assert any(b"SPAWN_SURVIVED" in row for row in s.last_rows)
    finally:
        s.close()

# An inactive child's terminal query must be answered without stealing focus.
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
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(background_query)
        source.flush()
        s.send(("exec python3 " + shlex.quote(source.name) + "\n").encode())
        s.expect(b"QUERY_WAIT")
        s.send(b"\x02c")
        s.expect(b"RUSTMUX_READY> ")
        s.read(0.5)
        assert not any(b"QUERY_BG_OK" in row for row in s.last_rows)
        s.send(b"\x02p")
        s.expect(b"QUERY_BG_OK")
        s.send(b"x")
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"exit 0\n")
        s.finish(0)
finally:
    s.close()

# The resident-window cap and global termination cover every owned direct child.
with tempfile.TemporaryDirectory(prefix="rustmux-window-limit-") as directory:
    shell = os.path.join(directory, "shell")
    record = os.path.join(directory, "pids")
    with open(shell, "w") as source:
        source.write('#!/bin/sh\nprintf "%s\\n" "$$" >> ' + shlex.quote(record) + '\n'
                     'export PS1="RUSTMUX_READY> "\nexec /bin/sh -i\n')
    os.chmod(shell, 0o700)
    s = Session(shell=shell)
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02c" * 15)
        end = time.monotonic() + 5
        while True:
            s.read()
            with open(record) as source:
                pids = [int(line) for line in source if line.strip()]
            if len(pids) == 16:
                break
            assert time.monotonic() < end, pids
        s.send(b"\x02c")
        for _ in range(4):
            s.read(0.05)
        with open(record) as source:
            assert len(source.readlines()) == 16
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
        for pid in pids:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                pass
            else:
                raise AssertionError(("child still alive after global shutdown", pid))
    finally:
        s.close()

# Rename edits window metadata while the child continues writing its own screen.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"sleep 0.2; printf '\\033[2J\\033[H%s%s\\n' WORK _DONE\n")
    s.send(b"\x02,")
    s.expect(b"Rename: shell")
    s.send("\x15中文e\u0301\x7f".encode())
    s.expect("Rename: 中文e".encode())
    end = time.monotonic() + 3
    while not any(b"WORK_DONE" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < end, s.last_rows
    assert s.last_rows[0].startswith("Rename: 中文e".encode())
    s.send(b"\r")
    s.send(b"\x02,")
    s.expect("Rename: 中文e".encode())
    s.send(b"\x15discard\x1b")
    end = time.monotonic() + 3
    while s.last_rows[0].startswith(b"Rename:"):
        s.read()
        assert time.monotonic() < end, s.last_rows
    s.send(b"\x02,")
    s.expect("Rename: 中文e".encode())
    s.send("\x15\x1b[200~粘贴\x02c\n\x1b[201~\r".encode())
    s.send(b"\x02,")
    s.expect("Rename: 粘贴c".encode())
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 18, 60, 0, 0))
    s.read(0.1)
    s.send(b"\x07")
    s.send(b"printf '\\n%s%s\\n' RENAME_ RESTORED; exit 0\n")
    s.finish(0)
    assert any(b"RENAME_RESTORED" in row for row in s.last_rows)
    assert not any(row.startswith(b"Rename:") for row in s.last_rows)
finally:
    s.close()

# A child exit cancels its pending rename and delivers the child's final screen.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"sleep 0.2; printf '\\033[2J\\033[H%s%s\\n' EDITOR_ EXIT; exit 7\n")
    s.send(b"\x02,")
    s.expect(b"Rename: shell")
    s.finish(7)
    assert any(b"EDITOR_EXIT" in row for row in s.last_rows)
    assert not any(row.startswith(b"Rename:") for row in s.last_rows)
finally:
    s.close()

# Persistent bar reflects creation, focus, rename and removal without hiding content.
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

s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    expect_bar(s, b"1 shell")
    expect_bar(s, b"LOCKED")
    s.send(b"\x02")
    expect_bar(s, b"NORMAL")
    s.send(b"n")
    expect_bar(s, b"LOCKED")
    s.send(b"printf '\\033[23;1H%s%s' LAST_ CONTENT\n")
    s.expect(b"LAST_CONTENT")
    assert b"LAST_CONTENT" in s.last_rows[21]
    assert b"1 shell" in s.last_rows[0]
    s.send(b"\x02c")
    s.expect(b"RUSTMUX_READY> ")
    expect_bar(s, b"2 shell")
    s.send("\x02,\x15中文\r".encode())
    expect_bar(s, "2 中文".encode())
    s.send(b"\x02p")
    expect_bar(s, b"1 shell")
    assert "2 中文".encode() in s.last_rows[0]
    s.send(b"\x02n")
    expect_bar(s, "2 中文".encode())
    s.send(b"exit 0\n")
    expect_bar_without(s, "2 中文".encode())
    expect_bar(s, b"1 shell")
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 1, 80, 0, 0))
    s.read(0.1)
    s.send(b"printf '\\033[2J\\033[H%s%s' ONE_ ROW\n")
    s.expect(b"ONE_ROW")
    assert len(s.last_rows) == 1 and b"1 shell" not in s.last_rows[0]
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 4, 80, 0, 0))
    expect_bar(s, b"1 shell")
    s.send(b"exit 0\n")
    s.finish(0)
finally:
    s.close()

bar_mouse = r"""
import os, select, time, tty
tty.setraw(0)
os.write(1, b"\x1b[?1000;1006h\x1b[2J\x1b[HBAR_MOUSE_READY")
expected = b"\x1b[<0;2;21M\x1b[<0;2;1mx"
data = bytearray()
end = time.monotonic() + 4
while len(data) < len(expected):
    assert time.monotonic() < end, repr(data)
    if select.select([0], [], [], 0.1)[0]:
        data.extend(os.read(0, len(expected) - len(data)))
assert data == expected, repr(data)
os.write(1, b"\x1b[2J\x1b[HBAR_MOUSE_OK")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(bar_mouse)
        source.flush()
        s.send(("exec python3 " + shlex.quote(source.name) + "\n").encode())
        s.expect(b"BAR_MOUSE_READY")
        s.send(b"\x1b[<0;3;23M\x1b[<0;3;1mx")
        s.finish(0)
        assert any(b"BAR_MOUSE_OK" in row for row in s.last_rows)
finally:
    s.close()

# Window labels remain clickable and the bar remains scrollable when the child
# itself has mouse reporting off.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"WIN=1\n\x02c")
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"WIN=2\n")
    s.expect(b"RUSTMUX_READY> ")
    # Legacy button reports match the encoding selected for a shell with mouse off.
    s.send(b"\x1b[M -!\x1b[M#-!printf '\nCLICKED:%s\n' $WIN\n")
    s.expect(b"CLICKED:2")
    s.send(b"\x1b[M \"!\x1b[M#\"!printf '\nCLICKED:%s\n' $WIN\n")
    s.expect(b"CLICKED:1")
    # Legacy wheel reports use button codes 64 (up) and 65 (down). The column
    # is deliberately outside both labels: the complete bar is scrollable.
    s.send(b"\x1b[Ma>!printf '\nSCROLLED:%s\n' $WIN\n")
    s.expect(b"SCROLLED:2")
    s.send(b"\x1b[M`>!printf '\nSCROLLED:%s\n' $WIN\n")
    s.expect(b"SCROLLED:1")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

# Numeric selection follows visible positions, including after earlier removal.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    for number in range(1, 11):
        if number > 1:
            s.send(b"\x02c")
            s.expect(b"RUSTMUX_READY> ")
        s.send(("WIN=%d; printf '\\033[2J\\033[HREADY_%%s\\n' $WIN\n" % number).encode())
        s.expect(("READY_%d" % number).encode())
    s.send(b"\x02")
    s.send(b"1printf '\\nSELECTED:%s\\n' $WIN\n")
    s.expect(b"SELECTED:1")
    expect_bar(s, b"1 shell")
    s.send(b"\x020printf '\\nSELECTED:%s\\n' $WIN\n")
    s.expect(b"SELECTED:10")
    expect_bar(s, b"10 shell")
    s.send(b"\x02\tprintf '\\nLAST:%s\\n' $WIN\n")
    s.expect(b"LAST:1")
    expect_bar(s, b"1 shell")
    s.send(b"\x02\tprintf '\\nBACK:%s\\n' $WIN\n")
    s.expect(b"BACK:10")
    expect_bar(s, b"10 shell")
    s.send(b"\x021exit 0\n")
    s.expect(b"\r\nREADY_2\r\n")
    expect_bar(s, b"1 shell")
    s.send(b"\x029printf '\\nSHIFTED:%s\\n' $WIN\n")
    s.expect(b"SHIFTED:10")
    expect_bar(s, b"9 shell")
    # Position ten is now absent. Both missing and current selections preserve input routing.
    s.send(b"\x020\x029printf '\\nSTILL:%s\\n' $WIN\n")
    s.expect(b"STILL:10")
    s.send(b"\x021printf '\\nFIRST:%s\\n' $WIN\n")
    s.expect(b"FIRST:2")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

# Explicit close: cancel, bracketed-paste confirmation, survivor input and child cleanup.
with tempfile.TemporaryDirectory() as directory:
    record = os.path.join(directory, "closing.pid")
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"KEEP=survivor\n")
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02c")
        s.expect(b"RUSTMUX_READY> ")
        s.send(("echo $$ > " + shlex.quote(record) + "\n").encode())
        s.expect(b"RUSTMUX_READY> ")
        end = time.monotonic() + 3
        while not os.path.exists(record):
            s.read()
            assert time.monotonic() < end
        with open(record) as source:
            closing_pid = int(source.read())
        for answer in (b"\r", b"no\r", b"yes\x07", b"yes\x03", b"yes\x1b"):
            s.send(b"\x02&")
            s.expect(b"Close window? Type yes:")
            s.send(answer)
            end = time.monotonic() + 3
            while s.last_rows[0].startswith(b"Close window?"):
                s.read()
                assert time.monotonic() < end, s.last_rows
            os.kill(closing_pid, 0)
            expect_bar(s, b"2 shell")
        s.send(b"sleep 0.2; printf '\\033[2J\\033[H%s%s\\n' CLOSE_ BACKGROUND\n")
        s.send(b"\x02&")
        s.expect(b"Close window? Type yes:")
        s.expect(b"CLOSE_BACKGROUND")
        fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 18, 60, 0, 0))
        s.send(b"\x1b[200~yes\r\n\x1b[201~")
        s.expect(b"Close window? Type yes: yes")
        os.kill(closing_pid, 0) # Pasted newline must not confirm.
        s.send(b"\rLEAK=1\n")
        expect_bar(s, b"1 shell")
        try:
            os.kill(closing_pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError("closed direct child still alive")
        s.send(b"printf '\\nSURVIVOR:%s:%s\\n' $KEEP ${LEAK-unset}\n")
        s.expect(b"SURVIVOR:survivor:unset")
        s.send(b"\x02&")
        s.expect(b"Close window? Type yes:")
        s.send(b"yes\r")
        s.finish(0)
    finally:
        s.close()

# Natural child exit while confirming retains its normal exit status.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"sleep 0.2; exit 7\n\x02&")
    s.expect(b"Close window? Type yes:")
    s.finish(7)
finally:
    s.close()

# Moving windows changes displayed positions, not shells or last-window identity.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    for name in ("A", "B", "C"):
        if name != "A":
            s.send(b"\x02c")
            s.expect(b"RUSTMUX_READY> ")
        s.send(("WIN=%s\n" % name).encode())
        s.expect(b"RUSTMUX_READY> ")
        s.send(("\x02,\x15%s\r" % name).encode())
        expect_bar(s, ("%d %s" % (ord(name) - ord("A") + 1, name)).encode())
    s.send(b"\x02<printf '\\nMOVED:%s\\n' $WIN\n")
    s.expect(b"MOVED:C")
    expect_bar(s, b"2 C")
    s.send(b"\x02\tprintf '\\nLAST:%s\\n' $WIN\n")
    s.expect(b"LAST:B")
    expect_bar(s, b"3 B")
    s.send(b"\x022\x02>\x02>printf '\\nRIGHT:%s\\n' $WIN\n")
    s.expect(b"RIGHT:C")
    expect_bar(s, b"1 C")
    s.send(b"\x02<printf '\\nWRAPPED:%s\\n' $WIN\n")
    s.expect(b"WRAPPED:C")
    expect_bar(s, b"3 C")
    s.send(b"\x02<\x02<printf '\\nLEFT:%s\\n' $WIN\n")
    s.expect(b"LEFT:C")
    expect_bar(s, b"1 C")
    s.send(b"exit 0\n")
    expect_bar_without(s, b"1 C")
    expect_bar(s, b"1 A")
    s.send(b"\x02\tprintf '\\nSURVIVING:%s\\n' $WIN\n")
    s.expect(b"SURVIVING:B")
    expect_bar(s, b"2 B")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

# Interactive splits retain all screens, route keys, resize and close only one pane.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"stty -echo; VAR=A; printf '\\033[2J\\033[H%s%s\\n' MARK _A\n")
    s.expect(b"MARK_A")
    s.send(b"\x02%")
    s.expect(b"RUSTMUX_READY>")
    s.send(b"stty -echo; VAR=B; printf '\\033[2J\\033[H%s%s:%s\\n' RIGHT _B \"$(stty size)\"\n")
    s.expect(b"RIGHT_B:21 38")
    assert any(b"MARK_A" in row for row in s.last_rows)
    s.send(b"\x02hprintf '\\033[2J\\033[HLEFT:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"LEFT:A:21 38")
    s.send(b'\x02"')
    s.expect(b"RUSTMUX_READY>")
    s.send(b"stty -echo; VAR=C; printf '\\033[2J\\033[HLOWER:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"LOWER:C:10 38")
    assert any(b"RIGHT_B:21 38" in row for row in s.last_rows)
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
    s.expect(b"AFTER:B:27 48")
    s.send(b"\x02hprintf '\\033[2J\\033[HRESTORED:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"RESTORED:A:27 48")
    s.send(b"exit 0\n")
    end = time.monotonic() + 3
    while any(b"RESTORED:A" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < end, s.last_rows
    s.send(b"printf '\\033[2J\\033[HLAST_PANE:%s:%s\\n' $VAR \"$(stty size)\"; exit 9\n")
    s.finish(9)
    assert any(b"LAST_PANE:B:27 98" in row for row in s.last_rows)
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
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"VAR=A\n\x02%")
    s.expect(b"RUSTMUX_READY>")
    s.send(b"VAR=B; count=0; trap 'count=$((count+1))' WINCH\n")
    s.expect(b"RUSTMUX_READY>")
    # Intermediate mouse positions are coalesced before the final PTY resize.
    # This prevents prompt-redrawing shells from processing stale dimensions.
    s.send(b"\x1b[M H%\x1b[M@M%\x1b[M@W%\x1b[M@R%\x1b[M#R%printf 'DRAG:%s:%s:%s\n' $VAR $count \"$(stty size)\"\n")
    s.expect(b"DRAG:B:1:21 28")
    s.send(b"\x02hprintf 'DRAG:%s:%s\n' $VAR \"$(stty size)\"\n")
    s.expect(b"DRAG:A:21 48")
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
    s.expect(b"ZOOM:B:21 78")
    assert not any(b"ZOOM_BASE" in row for row in s.last_rows)
    s.send(b"\x02hprintf '\\033[2J\\033[HTARGET:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"TARGET:A:21 78")
    assert not any(b"ZOOM:B" in row for row in s.last_rows)
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    s.read(0.1)
    s.send(b"printf '\\033[2J\\033[HZOOM_RESIZE:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"ZOOM_RESIZE:A:27 98")
    s.send(b"\x02oprintf '\\033[2J\\033[HCYCLE_ZOOM:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"CYCLE_ZOOM:B:27 98")
    s.send(b"\x02Zprintf '\\033[2J\\033[HTILED:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"TILED:B:27 48")
    assert any(b"ZOOM_RESIZE:A" in row for row in s.last_rows)
    s.send(b"\x02hprintf '\\033[2J\\033[HRESTORED_ZOOM:%s:%s\\n' $VAR \"$(stty size)\"\n")
    s.expect(b"RESTORED_ZOOM:A:27 48")
    s.send(b"\x02Zexit 0\n")
    end = time.monotonic() + 3
    while any(b"RESTORED_ZOOM:A" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < end, s.last_rows
    s.send(b"printf '\\033[2J\\033[HZOOM_SURVIVOR:%s:%s\\n' $VAR \"$(stty size)\"; exit 8\n")
    s.finish(8)
    assert any(b"ZOOM_SURVIVOR:B:27 98" in row for row in s.last_rows)
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

# A partially filled primary screen can enter history before it has scrollback.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"\x02[")
    s.expect(b"History 0/0")
    assert b"RUSTMUX_READY>" in b"".join(s.last_rows)
    assert b"\x1b[?1002h" in s.last_frame
    s.output.clear()
    s.send(b"/RUSTMUX_READY\r")
    s.expect(b"1/1 /RUSTMUX_READY")
    s.output.clear()
    s.send(b"\x1b")
    deadline = time.monotonic() + 3
    while b"/RUSTMUX_READY" in s.last_rows[0]:
        s.read()
        assert time.monotonic() < deadline, bytes(s.output[-1000:])
    assert b"History 0/0" in s.last_rows[0]
    s.output.clear()
    s.send(b"/draft")
    s.expect(b"/draft")
    s.output.clear()
    s.send(b"\x1b")
    deadline = time.monotonic() + 3
    while b"/draft" in s.last_rows[0]:
        s.read()
        assert time.monotonic() < deadline, bytes(s.output[-1000:])
    assert b"History 0/0" in s.last_rows[0]
    s.send(b"/still-in-history")
    s.expect(b"Search /still-in-history")
    s.send(b"\x03")
    s.expect(b"History 0/0")
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
        assert b"History " in s.last_rows[0]
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
        s.expect(b"1/1 /SELECT_TARGET")
        s.output.clear()
        s.send(b"y")
        deadline = time.monotonic() + 3
        while not (matched := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        assert base64.b64decode(matched.group(1), validate=True) == b"SELECT_TARGET"
        s.output.clear()
        s.send(b"v")
        s.expect(b"Select ")
        s.output.clear()
        s.send(b"\x1b")
        deadline = time.monotonic() + 3
        while b"/SELECT_TARGET" not in s.last_rows[0] or b"Select " in s.last_rows[0]:
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
        s.expect(b"Copy sent to terminal")
        s.output.clear()
        s.send(b"\x1b")
        deadline = time.monotonic() + 3
        while not (
            s.last_rows[0].startswith(b"History ")
            and b"/SELECT_TARGET" not in s.last_rows[0]
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
        s.send(b"g")
        s.expect(b"HIST_00")
        s.output.clear()
        # HIST_00 begins at outer column 42, row 13 in this frozen split.
        # Dragging over its first four cells copies HIST and leaves shell input untouched.
        s.send(b"\x1b[<0;42;13M\x1b[<32;45;13M\x1b[<0;45;13m")
        deadline = time.monotonic() + 3
        while not (selected := re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", s.output)):
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        selected_text = base64.b64decode(selected.group(1), validate=True)
        assert selected_text == b"HIST", (selected_text, s.last_rows, bytes(s.output[-500:]))
        s.expect(b"Copy sent to terminal")
        s.send(b"G")
        s.expect(b"HIST_44")
        s.output.clear()
        # One motion report above the pane keeps scrolling while the button is held.
        s.send(b"\x1b[<0;42;13M\x1b[<32;42;1M")
        deadline = time.monotonic() + 3
        while not re.search(rb"History [1-9][0-9]*/", s.last_rows[0]):
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
        s.expect(b"Copy sent to terminal")
        deadline = time.monotonic() + 3
        while b"Copy sent to terminal" in s.last_rows[0]:
            s.read()
            assert time.monotonic() < deadline, (s.last_rows, bytes(s.output[-1000:]))
        assert s.last_rows[0].startswith(b"History "), s.last_rows
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
        s.expect(b"1/10 /HIST_0")
        s.send(b"N")
        s.expect(b"10/10 /HIST_0")
        s.send(b"n")
        s.expect(b"1/10 /HIST_0")
        assert any(b"HIST_00" in row for row in s.last_rows[1:])
        s.send(b"G?HIST_0\r")
        s.expect(b"10/10 ?HIST_0")
        s.send(b"n")
        s.expect(b"9/10 ?HIST_0")
        s.send(b"N")
        s.expect(b"10/10 ?HIST_0")
        s.send(b"/cancelled\x03n")
        s.expect(b"9/10 ?HIST_0")
        s.send(b"?HIST_0\rn")
        s.expect(b"8/10 ?HIST_0")
        s.send(b"/DOES_NOT_EXIST\r")
        s.expect(b"no match")
        s.send(b"/qjk\x03")
        s.expect(b"no match")
        s.send(b"?DRAFT\x1b[A")
        s.expect(b"Search ?DOES_NOT_EXIST")
        s.send(b"\x1bOA")
        s.expect(b"Search ?HIST_0")
        s.send(b"\x1bOB\x1b[B")
        s.expect(b"Search ?DRAFT")
        s.send(b"\x1b[A\x1b[A\r")
        s.expect(b"8/10 ?HIST_0")
        assert b"Search " not in s.last_rows[0]
        s.send(b"/HIST_X\x1b[D\x1b[3~0")
        s.expect(b"Search /HIST_0")
        assert b"\x1b[?25h" in s.last_frame
        s.send(b"\x1b[H\x1b[C\x1b[F\r")
        s.expect(b"8/10 /HIST_0")
        assert b"\x1b[?25l" in s.last_frame
        s.send(b"/HIST_\x1b[200~0\r\n\x03\x02\x15\x7f\x1b[201~")
        s.expect(b"Search /HIST_0")
        assert b"\x1b[?25h" in s.last_frame
        s.send(b"\r")
        s.expect(b"8/10 /HIST_0")
        s.send(b"G")
        s.expect(b"History 0/")
        assert not any(b"LATE_HISTORY_OUTPUT" in row for row in s.last_rows)
        # Mouse coordinates are outer-terminal coordinates: bar row 1 and the
        # left pane / column-40 separator must not scroll the right snapshot.
        frozen = list(s.last_rows)
        s.send(b"\x1b[<64;10;5M\x1b[<64;40;5M\x1b[<64;50;1M")
        s.read(0.1)
        assert s.last_rows == frozen
        s.send(b"\x1b[<64;50;5M")
        s.expect(b"History 3/")
        assert any(b"HISTORY_LEFT" in row for row in s.last_rows)
        s.send(b"\x1b[<65;50;5M")
        s.expect(b"History 0/")
        s.send(b"q")
        s.expect(b"LATE_HISTORY_OUTPUT")
        assert b"\x1b[?1002h" in s.last_frame
        assert b"\x1b[?1006l" in s.last_frame
        s.send(b"printf '\\n%s%s\\n' INPUT_ INTACT\n")
        s.expect(b"INPUT_INTACT")
        s.send(b"\x02[\x1b[5~")
        s.expect(b"History ")
        fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        expect_bar(s, b"1 shell")
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
    assert b"History " in s.last_rows[0]
    s.send(b"qprintf '\\n%s%s\\n' RESIZE_PROMPT_ OK\n")
    s.expect(b"RESIZE_PROMPT_OK")
    s.send(b"\x02Z")
    s.expect(b"RESIZE_HIST_02")
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
    s.expect(b"RIGHT_right_21 37")
    s.send(b"\x02h")
    s.send(b"printf 'LEFT_%s_' \"$resize_token\"; stty size\n")
    s.expect(b"LEFT_left_21 39")
    s.send(b'\x02\x08\x02"')
    s.expect(b"RUSTMUX_READY>")
    s.send(b"stty -echo; printf 'BOTTOM_%s\\n' READY\n")
    s.expect(b"BOTTOM_READY")
    s.send(b"\x02\x0b")
    s.send(b"printf 'BOTTOM_%s_' SIZE; stty size\n")
    s.expect(b"BOTTOM_SIZE_11 38")
    s.send(b"\x02Z\x02\x0a\x02Z")
    s.send(b"printf 'RESTORED_%s_' SIZE; stty size\n")
    s.expect(b"RESTORED_SIZE_11 38")
    s.send(b"\x02\x0a")
    s.send(b"printf 'DOWN_%s_' SIZE; stty size\n")
    s.expect(b"DOWN_SIZE_10 38")
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
    s.expect(b"RIGHT_21 38")
    assert any(b"RIGHT_21 38" in row[:38] for row in s.last_rows[1:])
    s.send(b"\x02{test \"$swap_pid\" = \"$$\" && printf '%s_BACK_' \"$swap_token\"; stty size\n")
    s.expect(b"RIGHT_BACK_21 38")
    # Focus LEFT by geometry: its shell and variables survived both exchanges.
    s.send(b"\x02htest \"$swap_pid\" = \"$$\" && printf '%s_STILL_' \"$swap_token\"; stty size\n")
    s.expect(b"LEFT_STILL_21 38")
    s.send(b"exit 0\n")
    # Wait until removal is rendered before sending input to the surviving shell.
    end = time.monotonic() + 3
    while any("│".encode() in row for row in s.last_rows[1:]):
        s.read()
        assert time.monotonic() < end, s.last_rows
    s.send(b"printf '%s_SURVIVES_' \"$swap_token\"; stty size\n")
    s.expect(b"RIGHT_SURVIVES_21 78")
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
        s.expect(b"SURVIVOR:survivor:unset_21 78")
        s.send(b"\x02z")
        command = ("test \"$PWD\" = " + shlex.quote(directory) +
                   " && printf '\\nUNDO:%s:%s\\n' $UNDO_KEEP $$\n")
        s.send(command.encode())
        s.expect(("UNDO:restored:" + str(closing_pid)).encode())
        s.send(b"\x02x")
        s.expect(b"Close pane? Type yes:")
        s.send(b"yes\r")
        s.expect(b"SURVIVOR:survivor:unset_21 78")
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
        s.expect(b"KEEP:source_21 78")
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
        s.expect(b"KEEP:target_21 38")
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
        s.frames.clear()
        s.send(b"exit 0\n")
        s.expect(b"RUSTMUX_READY>")
        s.frames.clear()
        s.send(b"exit 0\n")
        expect_bar_without(s, b"2 shell")
        expect_bar(s, b"1 shell")
        s.send(b"printf 'SOURCE_%s\\n' \"$KEEP\"\n")
        s.expect(b"SOURCE_source")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()


# Attaching to an unknown name reports the missing session instead of its socket path.
missing_name = f"missing-{os.getpid()}"
missing = subprocess.run(
    [BINARY, "attach", missing_name], capture_output=True, text=True,
)
assert missing.returncode == 1, missing
assert missing.stderr.endswith(
    f"rustmux: session '{missing_name}' does not exist\n"
), missing.stderr


# A named server survives client detach and preserves its shell for reattachment.
session_name = f"integration-{os.getpid()}"
session_socket = f"/tmp/rustmux-{os.geteuid()}/{session_name}.sock"
s = Session(arguments=("new", session_name))
try:
    s.expect(b"RUSTMUX_READY>")
    expect_bar(s, f"Rustmux ({session_name})".encode())
    expect_bar(s, b"LOCKED")
    s.send(b"\x02")
    expect_bar(s, b"NORMAL")
    s.send(b"n")
    expect_bar(s, b"LOCKED")
    s.send(b"stty -echo; KEEP=persistent\n")
    s.expect(b"RUSTMUX_READY>")
    assert session_name in subprocess.run(
        [BINARY, "list"], check=True, capture_output=True, text=True,
    ).stdout.splitlines()
    occupied = subprocess.run(
        [BINARY, "attach", session_name], capture_output=True, text=True,
    )
    assert occupied.returncode == 1, occupied
    assert occupied.stderr.endswith(
        f"rustmux: session '{session_name}' already has an attached client\n"
    ), occupied.stderr
    s.send(b"\x02d")
    s.finish(0)
finally:
    s.close()
assert os.path.exists(session_socket), session_socket
listed = subprocess.run(
    [BINARY, "list"], check=True, capture_output=True, text=True,
).stdout.splitlines()
assert session_name in listed, listed

s = Session(arguments=("attach", session_name))
try:
    s.expect(b"RUSTMUX_READY>")
    expect_bar(s, f"Rustmux ({session_name})".encode())
    s.send(b"printf 'SESSION_%s\\n' \"$KEEP\"\n")
    s.expect(b"SESSION_persistent")
    s.send(b"exit 0\n")
    s.finish(0)
finally:
    s.close()
end = time.monotonic() + 3
while os.path.exists(session_socket):
    time.sleep(0.01)
    assert time.monotonic() < end, session_socket


# Killing an attached named session stops its server, restores the client terminal,
# and removes every endpoint sidecar.
kill_name = f"kill-{os.getpid()}"
session_directory = f"/tmp/rustmux-{os.geteuid()}"
kill_paths = [
    f"{session_directory}/{kill_name}.sock",
    f"{session_directory}/{kill_name}.lock",
    f"{session_directory}/{kill_name}.pid",
]
s = Session(arguments=("new", kill_name))
try:
    s.expect(b"RUSTMUX_READY>")
    killed = subprocess.run(
        [BINARY, "kill", kill_name], capture_output=True, text=True,
    )
    assert killed.returncode == 0, killed
    s.finish(143)
finally:
    s.close()
assert not [path for path in kill_paths if os.path.exists(path)], kill_paths


# The same command terminates a server after its only client has detached.
detached_name = f"kill-detached-{os.getpid()}"
detached_paths = [
    f"{session_directory}/{detached_name}.sock",
    f"{session_directory}/{detached_name}.lock",
    f"{session_directory}/{detached_name}.pid",
]
s = Session(arguments=("new", detached_name))
try:
    s.expect(b"RUSTMUX_READY>")
    s.send(b"\x02d")
    s.finish(0)
finally:
    s.close()
killed = subprocess.run(
    [BINARY, "kill", detached_name], capture_output=True, text=True,
)
assert killed.returncode == 0, killed
assert not [path for path in detached_paths if os.path.exists(path)], detached_paths


# Detached creation works without a controlling terminal, becomes listable before
# returning, and adopts the dimensions of its first real attachment.
background_name = f"background-{os.getpid()}"
background_env = dict(
    os.environ, RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="",
)
created = subprocess.run(
    [BINARY, "new", "--detached", background_name], capture_output=True, text=True,
    env=background_env,
)
assert created.returncode == 0, created
assert created.stdout == "", created.stdout
assert background_name in subprocess.run(
    [BINARY, "list"], check=True, capture_output=True, text=True,
).stdout.splitlines()
s = Session(arguments=("attach", background_name))
try:
    s.expect(b"RUSTMUX_READY>")
    expect_bar(s, f"Rustmux ({background_name})".encode())
    s.send(b"stty size\n")
    s.expect(b"21 78")
    s.send(b"exit 0\n")
    s.finish(0)
finally:
    s.close()


# Omitting the attach name opens a picker when several sessions are live. Its
# selection is based on the same sorted list as the CLI and restores the outer
# terminal before the selected session client takes over.
picker_helper = f"picker-zhelper-{os.getpid()}"
picker_target = f"picker-atarget-{os.getpid()}"
picker_second = f"picker-second-{os.getpid()}"
picker_created = f"picker-created-{os.getpid()}"
for name in (picker_helper, picker_target):
    created = subprocess.run(
        [BINARY, "new", "--detached", name], capture_output=True, text=True,
        env=background_env,
    )
    assert created.returncode == 0, created
long_listing = subprocess.run(
    [BINARY, "list", "--long"], check=True, capture_output=True, text=True,
).stdout
assert "SESSION" in long_listing, long_listing
assert "STATUS" in long_listing, long_listing
assert "PID" in long_listing, long_listing
assert "LAST CONNECTED" in long_listing, long_listing
assert picker_helper in long_listing, long_listing
assert "DETACHED" in long_listing, long_listing
assert "\x1b" not in long_listing, repr(long_listing)
picker = None
try:
    sessions = subprocess.run(
        [BINARY, "list"], check=True, capture_output=True, text=True,
    ).stdout.splitlines()
    assert len(sessions) > 1, sessions

    picker = Session(arguments=("attach",))
    end = time.monotonic() + 8
    while b"Session Manager" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.send(b"/ATA\t")
    visible_target = b"picker-atarget"
    while visible_target not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.send(b"\r")
    picker.expect(b"RUSTMUX_READY>")
    expect_bar(picker, f"Rustmux ({picker_target})".encode())

    # Ctrl-B Ctrl-W detaches only this client, opens the manager with the
    # current session selected, and attaches the chosen session. Cancelling a
    # manager opened this way reconnects the session that opened it.
    picker.output.clear()
    picker.send(b"\x02\x17")
    end = time.monotonic() + 8
    while b"Session Manager" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    while b"[CURRENT]" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    manager_order = [picker_target] + [name for name in sessions if name != picker_target]
    target_to_helper = manager_order.index(picker_helper)
    picker.send(b"j" * target_to_helper + b"\r")
    picker.expect(b"RUSTMUX_READY>")
    expect_bar(picker, f"Rustmux ({picker_helper})".encode())

    picker.output.clear()
    picker.send(b"\x02\x17")
    end = time.monotonic() + 8
    while b"Session Manager" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.send(b"q")
    picker.expect(b"RUSTMUX_READY>")
    expect_bar(picker, f"Rustmux ({picker_helper})".encode())
    picker.send(b"\x02d")
    picker.finish(0)

    # The most recently attached detached session is selected first. The first
    # d only arms deletion; another key cancels that confirmation, and only a
    # fresh consecutive dd terminates the selected session.
    picker.close()
    picker = Session(arguments=("attach",))
    fcntl.ioctl(picker.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 160, 0, 0))
    end = time.monotonic() + 8
    while b"Session Manager" not in picker.output or b"LAST CONNECTED" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.send(b"d")
    while b"Press d again to kill" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    assert picker_helper.encode() in picker.output
    assert picker_helper in subprocess.run(
        [BINARY, "list"], check=True, capture_output=True, text=True,
    ).stdout.splitlines()
    picker.output.clear()
    picker.send(b"xdd")
    while b"Session Manager" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    assert picker_helper not in subprocess.run(
        [BINARY, "list"], check=True, capture_output=True, text=True,
    ).stdout.splitlines()
    picker.send(b"q")
    picker.finish(0)

    # `a` uses the same New shortcut as main. The entered name is validated,
    # created as a persistent session and attached after the picker restores.
    created = subprocess.run(
        [BINARY, "new", "--detached", picker_second], capture_output=True,
        text=True, env=background_env,
    )
    assert created.returncode == 0, created
    picker.close()
    picker = Session(arguments=("attach",))
    end = time.monotonic() + 8
    while b"Session Manager" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.send(b"a" + picker_created.encode() + b"\r")
    picker.expect(b"RUSTMUX_READY>")
    expect_bar(picker, f"Rustmux ({picker_created})".encode())
    picker.send(b"exit 0\n")
    picker.finish(0)
finally:
    if picker is not None:
        picker.close()
    for name in (picker_helper, picker_target, picker_second, picker_created):
        subprocess.run(
            [BINARY, "kill", name], capture_output=True, text=True, timeout=5,
        )

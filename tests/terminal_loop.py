"""Run the real binary inside an outer PTY and inspect the outer termios."""
import errno
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
    app = subprocess.Popen([sys.argv[2]])
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
    def __init__(self, shell="/bin/sh"):
        self.master, self.slave = os.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        env = dict(os.environ, RUSTMUX_SHELL=shell, PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
        def child_setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        read_report, write_report = os.pipe()
        self.child = subprocess.Popen(
            [sys.executable, __file__, "--supervisor", BINARY, str(write_report)],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, env=env,
            preexec_fn=child_setup, pass_fds=(write_report,))
        os.close(write_report)
        self.report = os.fdopen(read_report)
        self.app_pid = json.loads(self.report.readline())["pid"]
        os.set_blocking(self.master, False)
        self.output = bytearray()
        self.frame_pending = bytearray()
        self.frames = []
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
                    rows = (self.last_rows + [b""] * height)[:height]
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
                    self.last_rows = rows
                    self.frames.append(self.last_rows)
                    self.frames = self.frames[-64:]
            except OSError as error:
                if error.errno not in (errno.EIO, errno.EAGAIN):
                    raise

    def expect(self, text):
        def matches():
            target = text.strip(b"\r\n")
            for rows in self.frames:
                if text.startswith(b"\r\n"):
                    if target in rows:
                        return True
                elif target == b"RUSTMUX_READY> ":
                    nonempty = [row for row in (rows[:-1] if len(rows) > 1 else rows) if row]
                    if nonempty and nonempty[-1].endswith(target.rstrip()):
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
        s.expect(f"SIZE:{max(1, rows - 1)}:{columns}\r\n".encode())
    # Invalid transient dimensions must not terminate Rustmux or reach the child.
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 0, 0, 0, 0))
    s.read(0.15)
    assert b"SIZE:0:0" not in s.output
    assert s.child.poll() is None
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    s.expect(b"SIZE:23:80\r\n")
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
    assert s.last_rows[:5] == [b"HEADER", b"TWO", b"THREE", b"SCROLLED", b"FOOTER"], s.last_rows
    s.send(b"\n")
    s.expect(b"\r\nREVERSED\r\n")
    assert s.last_rows[:5] == [b"HEADER", b"REVERSED", b"TWO", b"THREE", b"FOOTER"], s.last_rows
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
        assert s.last_rows[:5] == expected, (command, s.last_rows)
        s.send(b"\n")
        s.expect(b"RUSTMUX_READY> ")

    for command, expected in [(b"@", b"ab cdefgh"), (b"P", b"abdefgh"), (b"X", b"ab defgh")]:
        s.send(b"stty -echo; printf '\\033[r\\033[2J\\033[1;1Habcdefgh"
               b"\\033[1;3H\\033[" + command +
               b"\\033[2;1HCHAR_EDIT_DONE'; read answer; "
               b"stty echo; printf '\\033[2;1H'\n")
        s.expect(b"\r\nCHAR_EDIT_DONE\r\n")
        assert s.last_rows[0] == expected, (command, s.last_rows)
        s.send(b"\n")
        s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[2;4r\\033[?6h"
           b"\\033[HORIGIN\\033[2d\\033[4GCOLUMN\\033[?6lABS"
           b"\\033[6;1HORIGIN_DONE'; read answer; "
           b"stty echo; printf '\\033[r\\033[6;1H'\n")
    s.expect(b"\r\nORIGIN_DONE\r\n")
    assert s.last_rows[:3] == [b"ABS", b"ORIGIN", b"   COLUMN"], s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[1;1Habcdefgh"
           b"\\033[1;3H\\033[4hXY\\033[4lZ"
           b"\\033[2;1HINSERT_DONE'; read answer; "
           b"stty echo; printf '\\033[2;1H'\n")
    s.expect(b"\r\nINSERT_DONE\r\n")
    assert s.last_rows[0] == b"abXYZdefgh", s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[1;1HA\\033[1;80H"
           b"\\033[?7lBC\\033[?7hD\\033[3;1HWRAP_DONE'; read answer; "
           b"stty echo; printf '\\033[3;1H'\n")
    s.expect(b"\r\nWRAP_DONE\r\n")
    assert s.last_rows[:2] == [b"A" + b" " * 78 + b"C", b"D"], s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[3g\\033[4G\\033H"
           b"\\033[12G\\033H\\033[H\\tA\\tB\\033[ZC"
           b"\\033[2;1HTABS_DONE'; read answer; "
           b"stty echo; printf '\\033[2;1H'\n")
    s.expect(b"\r\nTABS_DONE\r\n")
    assert s.last_rows[0] == b"   A       C", s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[H\\033(0lqqk"
           b"\\033(B\\033[2;1H\\033)0\\016x\\017q"
           b"\\033[3;1HCHARSET_DONE'; read answer; "
           b"stty echo; printf '\\033[3;1H'\n")
    s.expect(b"\r\nCHARSET_DONE\r\n")
    assert s.last_rows[:2] == ["┌──┐".encode(), "│q".encode()], s.last_rows
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[?1049h\\033[31;44m"
           b"\\033[?25l\\033[4h\\033[3g\\033(0lqqk"
           b"\\033cRESET_DONE'; read answer; stty echo; printf '\\033[2;1H'\n")
    s.expect(b"\r\nRESET_DONE\r\n")
    assert s.last_rows[0] == b"RESET_DONE" and not any(s.last_rows[1:-1]), s.last_rows
    assert s.last_frame.endswith(b"\x1b[?25h"), s.last_frame
    s.send(b"\n")
    s.expect(b"RUSTMUX_READY> ")

    s.send(b"stty -echo; printf '\\033[2J\\033[H\\033[31;44mKEPT"
           b"\\033[?25l\\033[4h\\033(0\\033[!pq"
           b"\\033[2;1HSOFT_RESET_DONE'; read answer; "
           b"stty echo; printf '\\033[3;1H'\n")
    s.expect(b"\r\nSOFT_RESET_DONE\r\n")
    assert s.last_rows[0] == b"KEPTq", s.last_rows
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

# Single-pane coordinates and event bytes are forwarded without translation.
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
    (1000, 1006, b"\x1b[<0;10;5M\x1b[<0;10;5m"),
    (1002, 1006, b"\x1b[<32;11;6M\x1b[<0;11;6m"),
    (1003, 1006, b"\x1b[<35;12;7M\x1b[<64;12;7M\x1b[<65;12;7M"),
    (1000, 0, b"\x1b[M *%\x1b[M#*%"),
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
            (1000, 1006, b"\x1b[<0;10;5M\x1b[<0;10;5m"),
            (1002, 1006, b"\x1b[<32;11;6M\x1b[<0;11;6m"),
            (1003, 1006, b"\x1b[<35;12;7M\x1b[<64;12;7M\x1b[<65;12;7M"),
            (1000, 0, b"\x1b[M *%\x1b[M#*%"),
        ]:
            s.expect(("\r\nMOUSE_%d_%d\r\n" % (mode, encoding)).encode())
            assert ("\x1b[?%dh" % mode).encode() in s.last_frame
            assert (b"\x1b[?1006h" if encoding else b"\x1b[?1006l") in s.last_frame
            s.send(payload[:3])
            s.send(payload[3:])
        s.expect(b"\r\nMOUSE_OFF\r\n")
        assert b"\x1b[?1000l" in s.last_frame
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
    assert s.last_rows[0] == b"UNCHANGED_ROW"
    assert b"\x1b[1;1H" not in s.last_frame
    assert b"\x1b[2;1H" in s.last_frame
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
    s.expect(b"X" * 80)
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
    s.expect(b"RETAINED:A:39 100")
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
expected = b"\x02n\x02z\x1b[200~paste\x02c\x02n\x02p\x1b[201~"
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
        s.send(b"\x02\x02n\x02z\x1b[20")
        s.send(b"0~paste\x02c\x02n\x02p\x1b[201~")
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
        s.send(b"\x02c")
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
    assert s.last_rows[-1].startswith("Rename: 中文e".encode())
    s.send(b"\r")
    s.send(b"\x02,")
    s.expect("Rename: 中文e".encode())
    s.send(b"\x15discard\x1b")
    end = time.monotonic() + 3
    while s.last_rows[-1].startswith(b"Rename:"):
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
    while not session.last_rows or marker not in session.last_rows[-1]:
        session.read()
        assert time.monotonic() < end, session.last_rows

s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    expect_bar(s, b"*1:shell")
    s.send(b"printf '\\033[23;1H%s%s' LAST_ CONTENT\n")
    s.expect(b"LAST_CONTENT")
    assert b"LAST_CONTENT" in s.last_rows[22]
    assert b"*1:shell" in s.last_rows[23]
    s.send(b"\x02c")
    s.expect(b"RUSTMUX_READY> ")
    expect_bar(s, b"*2:shell")
    s.send("\x02,\x15中文\r".encode())
    expect_bar(s, "*2:中文".encode())
    s.send(b"\x02p")
    expect_bar(s, b"*1:shell")
    assert "2:中文".encode() in s.last_rows[-1]
    s.send(b"\x02n")
    expect_bar(s, "*2:中文".encode())
    s.send(b"exit 0\n")
    expect_bar(s, b"*1:shell")
    assert "中文".encode() not in s.last_rows[-1]
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 1, 80, 0, 0))
    s.read(0.1)
    s.send(b"printf '\\033[2J\\033[H%s%s' ONE_ ROW\n")
    s.expect(b"ONE_ROW")
    assert len(s.last_rows) == 1 and b"*1:shell" not in s.last_rows[0]
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 4, 80, 0, 0))
    expect_bar(s, b"*1:shell")
    s.send(b"exit 0\n")
    s.finish(0)
finally:
    s.close()

bar_mouse = r"""
import os, select, time, tty
tty.setraw(0)
os.write(1, b"\x1b[?1000;1006h\x1b[2J\x1b[HBAR_MOUSE_READY")
expected = b"\x1b[<0;2;23mx"
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
        s.send(b"\x1b[<0;2;24M\x1b[<0;2;24mx")
        s.finish(0)
        assert any(b"BAR_MOUSE_OK" in row for row in s.last_rows)
finally:
    s.close()

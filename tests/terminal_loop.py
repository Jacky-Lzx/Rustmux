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
import time

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

    def read(self, seconds=0.05):
        if select.select([self.master], [], [], seconds)[0]:
            try:
                chunk = os.read(self.master, 65536)
                self.output.extend(chunk)
                self.frame_pending.extend(chunk)
                # A full frame ends in cursor positioning plus its visibility mode. Decode rows
                # payloads independently of the Rust parser; SGR does not occupy cells.
                while (match := re.search(rb"\x1b\[[0-9]+;[0-9]+H\x1b\[\?25[hl]", self.frame_pending)):
                    end = match.end()
                    frame = bytes(self.frame_pending[:end])
                    del self.frame_pending[:end]
                    if b"\x1b[?25l" not in frame:
                        continue
                    self.last_frame = frame
                    payloads = re.split(rb"\x1b\[[0-9]+;[0-9]+H", frame)[1:-1]
                    self.last_rows = [re.sub(rb"\x1b\[[0-9;]*m", b"", row).rstrip(b" ") for row in payloads]
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
                    nonempty = [row for row in rows if row]
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
        os.close(self.master)
        os.close(self.slave)

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
        s.expect(f"SIZE:{rows}:{columns}\r\n".encode())
    # Invalid transient dimensions must not terminate Rustmux or reach the child.
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 0, 0, 0, 0))
    s.read(0.15)
    assert b"SIZE:0:0" not in s.output
    assert s.child.poll() is None
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    s.expect(b"SIZE:24:80\r\n")
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
    s.send(b"exit\n")
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

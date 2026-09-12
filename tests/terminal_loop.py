"""Run the real binary inside an outer PTY and inspect the outer termios."""
import errno
import fcntl
import json
import os
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

    def read(self, seconds=0.05):
        if select.select([self.master], [], [], seconds)[0]:
            try:
                self.output.extend(os.read(self.master, 65536))
            except OSError as error:
                if error.errno not in (errno.EIO, errno.EAGAIN):
                    raise

    def expect(self, text):
        end = time.monotonic() + 8
        while text not in self.output:
            self.read()
            if time.monotonic() > end:
                raise AssertionError((text, bytes(self.output[-2000:]), self.child.poll()))
        self.output.clear()

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
    # Exit immediately after output exceeding the queue cap: no tail may be lost.
    s.send(b"python3 -c 'import os; os.write(1, b\"Z\" * 200000); print(\"BURST_DONE\")'; printf '\\nLAST_OUTPUT\\n'; exit 7\n")
    s.finish(7)
    assert b"Z" * 200000 + b"BURST_DONE\r\n" in s.output
    assert b"\r\nLAST_OUTPUT\r\n" in s.output
    assert b"\x1b[?1049l" in s.output
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
    s.expect(b"X" * 8192)
    time.sleep(0.2)
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

result = subprocess.run([BINARY], stdin=subprocess.DEVNULL, capture_output=True, timeout=5)
assert result.returncode == 1 and b"must be terminals" in result.stderr
print("Nested PTY: Unicode, backspace, Ctrl-C, 200KB output, exit tail, termios, startup failure and SIGTERM passed.")

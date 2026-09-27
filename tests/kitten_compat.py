"""Opt-in end-to-end smoke for an installed kitten icat in a Rustmux pane.

The fake outer PTY advertises Kitty graphics. The test asks real kitten icat
to send a PNG with Unicode placeholders and an explicit window size, then
checks Rustmux's composed outer RGBA upload. It does not require a GUI terminal
or a particular home config. Automatic kitten window-size discovery is not
covered: the child PTY currently has zero pixel-size ioctl fields.
"""

import fcntl
import os
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
from pathlib import Path

from yazi_compat import (
    FIRST_COLORS,
    assert_fixture_pixels,
    decode_outer_rgba_overlay,
    fixture_png,
    wait_for,
)


def complete_overlay(output):
    try:
        decode_outer_rgba_overlay(output)
        return True
    except AssertionError:
        return False


def main(binary):
    kitten = shutil.which("kitten")
    if kitten is None:
        print("SKIP: kitten is not installed")
        return
    with tempfile.TemporaryDirectory(prefix="rustmux-kitten-compat-") as temporary:
        root = Path(temporary)
        image = root / "preview.png"
        fixture_png(image, FIRST_COLORS)
        config = root / "config"
        config.mkdir()
        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 960, 480))
        os.set_blocking(master, False)
        environment = dict(
            os.environ,
            TERM="xterm-kitty",
            RUSTMUX_SHELL="/bin/sh",
            PS1="KITTEN_COMPAT_READY> ",
            ENV="",
            BASH_ENV="",
            XDG_CONFIG_HOME=str(config),
        )

        def child_setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        process = subprocess.Popen(
            [binary], stdin=slave, stdout=slave, stderr=slave,
            env=environment, preexec_fn=child_setup,
        )
        output = bytearray()
        try:
            query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c"
            wait_for(master, output, lambda data: query in data, process, 8, "outer Kitty probe")
            os.write(master, b"\x1b_Gi=31;OK\x1b\\\x1b[?1;2c")
            wait_for(
                master, output, lambda data: b"KITTEN_COMPAT_READY>" in data,
                process, 8, "child shell prompt",
            )
            output.clear()
            command = (
                f"{shlex.quote(kitten)} icat --transfer-mode=stream "
                "--unicode-placeholder --place=2x2@0x0 "
                "--use-window-size=80,24,960,480 "
                f"--stdin=no --image-id=47 {shlex.quote(str(image))}\n"
            )
            os.write(master, command.encode())
            wait_for(master, output, complete_overlay, process, 20,
                     "complete composed kitten image overlay")
            assert "\U0010eeee".encode() not in output, (
                "child placeholder leaked to outer terminal"
            )
            _, width, height, pixels = decode_outer_rgba_overlay(output)
            assert_fixture_pixels(width, height, pixels, FIRST_COLORS)
            print("PASS: installed kitten icat explicit-size Unicode-placeholder PNG composited")
        finally:
            os.close(master)
            os.close(slave)
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        pass


if __name__ == "__main__":
    main(sys.argv[1])

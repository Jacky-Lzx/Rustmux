"""Opt-in end-to-end smoke for an installed kitten icat in a Rustmux pane.

The fake outer PTY advertises Kitty graphics. Real kitten icat is exercised
both with Unicode placeholders and with its default command, including a
multi-chunk image. It does not require a GUI terminal or a home config.
"""

import fcntl
import hashlib
import os
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import zlib
from pathlib import Path

from yazi_compat import (
    FIRST_COLORS,
    assert_fixture_pixels,
    decode_outer_rgba_overlay,
    fixture_png,
    png_chunk,
    wait_for,
)


def complete_overlay(output):
    try:
        decode_outer_rgba_overlay(output)
        return True
    except AssertionError:
        return False


def multichunk_png(path):
    width = height = 320
    rows = b"".join(
        b"\0" + hashlib.shake_256(str(row).encode()).digest(width * 3)
        for row in range(height)
    )
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + png_chunk(b"IDAT", zlib.compress(rows))
        + png_chunk(b"IEND", b"")
    )


def run_case(binary, kitten, image, config, options, label, expected_colors=None):
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
        [binary],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=environment,
        preexec_fn=child_setup,
    )
    output = bytearray()
    try:
        query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c"
        wait_for(
            master, output, lambda data: query in data, process, 8, "outer Kitty probe"
        )
        os.write(master, b"\x1b_Gi=31;OK\x1b\\\x1b[?1;2c")
        wait_for(
            master,
            output,
            lambda data: b"KITTEN_COMPAT_READY>" in data,
            process,
            8,
            "child shell prompt",
        )
        output.clear()
        command = f"{shlex.quote(kitten)} icat {options}{shlex.quote(str(image))}\n"
        os.write(master, command.encode())
        try:
            wait_for(
                master,
                output,
                complete_overlay,
                process,
                20,
                "complete composed kitten image overlay",
            )
        except AssertionError as error:
            plain = bytes(output).decode("utf-8", errors="replace")
            graphics_count = output.count(b"\x1b_G")
            raise AssertionError(
                f"{error}; captured {len(output)} bytes, "
                f"{graphics_count} graphics commands; "
                f"first output: {plain[:1000]!r}"
            ) from error
        assert "\U0010eeee".encode() not in output, (
            "child placeholder leaked to outer terminal"
        )
        _, width, height, pixels = decode_outer_rgba_overlay(output)
        if expected_colors is None:
            assert any(pixels[index] for index in range(3, len(pixels), 4)), (
                "multi-chunk image had no visible pixels"
            )
        else:
            assert_fixture_pixels(width, height, pixels, expected_colors)
        print(f"PASS: installed kitten icat {label} composited")
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


def main(binary):
    kitten = shutil.which("kitten")
    if kitten is None:
        print("SKIP: kitten is not installed")
        return
    with tempfile.TemporaryDirectory(prefix="rustmux-kitten-compat-") as temporary:
        root = Path(temporary)
        small = root / "small.png"
        fixture_png(small, FIRST_COLORS)
        run_case(
            binary,
            kitten,
            small,
            root / "small-config",
            "--unicode-placeholder --place=2x2@0x0 --stdin=no --image-id=47 ",
            "automatic-size Unicode-placeholder PNG",
            FIRST_COLORS,
        )
        large = root / "multi-chunk.png"
        multichunk_png(large)
        run_case(
            binary,
            kitten,
            large,
            root / "large-config",
            "",
            "default auto-detect multi-chunk PNG",
        )


if __name__ == "__main__":
    main(sys.argv[1])

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

from kitty_large_overlay_compat import decode_tiles
from yazi_compat import (
    FIRST_COLORS,
    assert_fixture_pixels,
    decode_outer_rgba_overlay,
    fixture_png,
    png_chunk,
    wait_for,
)


def complete_overlay(output):
    # Do not repeatedly Base64-decode every preceding chunk while a large
    # overlay is still streaming; decode it once after the final APC arrives.
    final = output.rfind(b"m=0;")
    return final >= 0 and output.find(b"\x1b\\", final) >= 0


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


def run_case(
    binary,
    kitten,
    image,
    config,
    options,
    label,
    expected_colors=None,
    session_name=None,
    terminal_size=(24, 80, 960, 480),
    timeout=20,
    verify_complete_tiles=False,
):
    config.mkdir()
    master, slave = os.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", *terminal_size))
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
        [binary, "new", session_name] if session_name else [binary],
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
                lambda data: complete_overlay(data)
                or (verify_complete_tiles and b"KITTEN_COMPAT_READY>" in data),
                process,
                timeout,
                "complete composed kitten image overlay",
            )
            if verify_complete_tiles and not complete_overlay(output):
                # The shell can finish before an outer render flushes. Give
                # it a short final chance, then report the missing upload.
                wait_for(
                    master,
                    output,
                    complete_overlay,
                    process,
                    min(timeout, 10),
                    "outer Kitty image after kitten returned",
                )
        except AssertionError as error:
            graphics_count = output.count(b"\x1b_G")
            reason = str(error).splitlines()[0]
            if verify_complete_tiles and graphics_count == 0 and b"KITTEN_COMPAT_READY>" in output:
                reason = "kitten returned to the shell without an outer Kitty image upload"
            details = "" if verify_complete_tiles else (
                f"; first output: {bytes(output[:250]).decode('utf-8', errors='replace')!r}"
            )
            failure = AssertionError(
                f"{reason}; captured {len(output)} bytes, "
                f"{graphics_count} graphics commands{details}"
            )
            if verify_complete_tiles:
                raise failure from None
            raise failure from error
        assert "\U0010eeee".encode() not in output, (
            "child placeholder leaked to outer terminal"
        )
        if verify_complete_tiles:
            wait_for(
                master,
                output,
                lambda data: b"KITTEN_COMPAT_READY>" in data,
                process,
                timeout,
                "shell prompt after complete image upload",
            )
            tiles = decode_tiles(output)
            assert tiles, "no complete Rustmux-owned Kitty image was uploaded"
            assert all(tile["width"] > 0 and tile["height"] > 0 for tile in tiles)
            formats = ",".join(sorted({tile["format"].decode("ascii") for tile in tiles}))
            print(f"PASS: {len(tiles)} complete outer Kitty image tiles parsed (f={formats})")
        else:
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
        if session_name:
            # A failed startup may leave no named session to kill. Cleanup
            # must not hide the probe or image error that caused the failure.
            subprocess.run([binary, "kill", session_name], check=False, capture_output=True)


def main(binary, custom_image=None):
    kitten = shutil.which("kitten")
    if kitten is None:
        if custom_image is not None:
            raise AssertionError("RUSTMUX_COMPAT_IMAGE requires an installed kitten")
        print("SKIP: kitten is not installed")
        return
    with tempfile.TemporaryDirectory(prefix="rustmux-kitten-compat-") as temporary:
        root = Path(temporary)
        if custom_image is not None:
            image = Path(custom_image)
            assert image.is_file(), "RUSTMUX_COMPAT_IMAGE must name an existing file"
            run_case(
                binary,
                kitten,
                image,
                root / "custom-config",
                "",
                "user-provided image",
                session_name=f"compatimage{os.getpid()}",
                terminal_size=(61, 215, 3655, 2013),
                timeout=60,
                verify_complete_tiles=True,
            )
            return
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
        run_case(
            binary,
            kitten,
            small,
            root / "file-config",
            "--transfer-mode=file --unicode-placeholder --place=2x2@0x0 --stdin=no --image-id=48 ",
            "file-transfer Unicode-placeholder PNG",
            FIRST_COLORS,
        )
        assert small.exists(), "regular-file transfer must retain its source"
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
        run_case(
            binary,
            kitten,
            large,
            root / "large-viewport-config",
            "",
            "large-viewport named-session PNG",
            session_name=f"compatkitten{os.getpid()}",
            terminal_size=(61, 215, 3655, 2013),
            timeout=60,
        )


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else None)

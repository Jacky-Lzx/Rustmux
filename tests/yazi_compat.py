"""Opt-in end-to-end smoke against the installed Yazi image-preview protocol.

Starts Rustmux on a fake Kitty-capable outer PTY, then runs real Yazi on an
isolated 2x2 PNG. Success means Rustmux emitted a composed Kitty RGBA image
with the fixture's four pixels in the right order for Yazi's Unicode
placeholders; it does not require a GUI terminal.
"""

import base64
import fcntl
import os
import re
import select
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time
import zlib
from pathlib import Path


def png_chunk(tag, data):
    payload = tag + data
    return struct.pack(">I", len(data)) + payload + struct.pack(">I", zlib.crc32(payload))


def fixture_png(path):
    # Two rows of RGBA pixels, filter method 0, no external image tool needed.
    rows = bytes(
        [0, 255, 0, 0, 255, 0, 255, 0, 255]
        + [0, 0, 0, 255, 255, 255, 255, 255, 255]
    )
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 6, 0, 0, 0))
        + png_chunk(b"IDAT", zlib.compress(rows))
        + png_chunk(b"IEND", b"")
    )


def wait_for(master, output, predicate, process, timeout, label):
    end = time.monotonic() + timeout
    while not predicate(output):
        if process.poll() is not None:
            raise AssertionError(f"Rustmux exited while waiting for {label}: {process.returncode}")
        if time.monotonic() >= end:
            tail = bytes(output[-2000:]).decode("utf-8", errors="replace")
            raise AssertionError(f"timed out waiting for {label}; output tail:\n{tail}")
        if select.select([master], [], [], 0.05)[0]:
            try:
                output.extend(os.read(master, 65536))
            except BlockingIOError:
                pass
            if len(output) > 20 * 1024 * 1024:
                raise AssertionError("outer-terminal capture exceeded 20 MiB")


def decode_outer_rgba_overlay(output):
    """Decode the first complete Rustmux-owned Kitty RGBA upload."""
    payload = bytearray()
    dimensions = None
    for command in re.finditer(rb"\x1b_G([^;]*);([A-Za-z0-9+/=]*)\x1b\\", output):
        controls = dict(
            part.split(b"=", 1) for part in command.group(1).split(b",") if b"=" in part
        )
        if controls.get(b"a") == b"T" and controls.get(b"f") == b"32":
            image_id = int(controls.get(b"i", b"0"))
            if image_id < 0x80000000:
                continue
            dimensions = (int(controls[b"s"]), int(controls[b"v"]))
            payload.clear()
        elif dimensions is None or set(controls) != {b"m"}:
            continue

        encoded = command.group(2)
        assert len(encoded) <= 4096, "outer Kitty chunk exceeds the protocol limit"
        payload.extend(base64.b64decode(encoded, validate=True))
        if controls.get(b"m") == b"0":
            width, height = dimensions
            assert len(payload) == width * height * 4, (
                "outer RGBA dimensions do not match payload"
            )
            return width, height, bytes(payload)
        assert controls.get(b"m") == b"1", "invalid outer Kitty chunk continuation"
    raise AssertionError("no complete Rustmux-owned RGBA overlay found")


def assert_fixture_pixels(width, height, pixels):
    assert width > 0 and height > 0
    assert len(pixels) == width * height * 4
    colors = {
        "red": (255, 0, 0, 255),
        "green": (0, 255, 0, 255),
        "blue": (0, 0, 255, 255),
        "white": (255, 255, 255, 255),
    }
    positions = {name: [] for name in colors}
    for offset in range(0, len(pixels), 4):
        color = tuple(pixels[offset:offset + 4])
        for name, expected in colors.items():
            if color == expected:
                cell = offset // 4
                positions[name].append((cell % width, cell // width))
    for name, points in positions.items():
        assert points, f"Yazi fixture's {name} pixel is missing from the outer image"
    assert max(x for x, _ in positions["red"]) < min(
        x for x, _ in positions["green"]
    ), "red must be left of green"
    assert max(x for x, _ in positions["blue"]) < min(
        x for x, _ in positions["white"]
    ), "blue must be left of white"
    assert max(y for _, y in positions["red"]) < min(
        y for _, y in positions["blue"]
    ), "red must be above blue"
    assert max(y for _, y in positions["green"]) < min(
        y for _, y in positions["white"]
    ), "green must be above white"


def main(binary):
    yazi = shutil.which("yazi")
    if yazi is None:
        print("SKIP: Yazi is not installed")
        return
    version = subprocess.run([yazi, "--version"], capture_output=True, text=True, check=True)
    with tempfile.TemporaryDirectory(prefix="rustmux-yazi-compat-") as temporary:
        root = Path(temporary)
        images = root / "images"
        images.mkdir()
        fixture_png(images / "preview.png")
        config = root / "config"
        config.mkdir()
        master, slave = os.openpty()
        # Give Yazi realistic 12x20-pixel cells and Rustmux an exact outer
        # cell size, independent of the terminal running this test.
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 960, 480))
        os.set_blocking(master, False)
        environment = dict(
            os.environ,
            TERM="xterm-kitty",
            RUSTMUX_SHELL="/bin/sh",
            PS1="YAZI_COMPAT_READY> ",
            ENV="",
            BASH_ENV="",
            XDG_CONFIG_HOME=str(config),
            YAZI_CONFIG_HOME=str(config),
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
                master, output, lambda data: b"YAZI_COMPAT_READY>" in data,
                process, 8, "child shell prompt",
            )
            output.clear()
            command = f"exec env TERM=xterm-kitty YAZI_CONFIG_HOME={shlex.quote(str(config))} {shlex.quote(yazi)} {shlex.quote(str(images))}\n"
            os.write(master, command.encode())
            # Rustmux's overlay IDs start at 0x80000000. A successful a=T
            # from that range proves that Yazi's upload and placeholders were
            # both accepted and composed, rather than merely appearing in raw
            # child output or in the initial capability query.
            overlay = re.compile(rb"\x1b_Ga=T,f=32,[^;]*i=214748364[0-9][^;]*;")
            def completed_overlay(data):
                match = overlay.search(data)
                if match is None:
                    return False
                final = data.find(b"\x1b_Gm=0;", match.end())
                if final < 0 and b",m=0;" in match.group():
                    final = match.start()
                return final >= 0 and b"\x1b\\" in data[final:]

            wait_for(master, output, completed_overlay, process, 20,
                     "complete composed Yazi image overlay")
            assert "\U0010eeee".encode() not in output, "child placeholder leaked to outer terminal"
            assert_fixture_pixels(*decode_outer_rgba_overlay(output))
            version_line = next(
                (line.strip() for line in version.stdout.splitlines() if "Version:" in line),
                version.stdout.strip().splitlines()[0] if version.stdout.strip() else "Yazi",
            )
            print(f"PASS: {version_line}: Yazi preview RGBA pixels match fixture")
        finally:
            # Close the PTY first: on macOS a foreground terminal process can
            # otherwise remain blocked in terminal drain during termination.
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

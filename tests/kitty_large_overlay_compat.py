"""Opt-in PTY smoke for a large tiled Kitty overlay and its deletion.

This uses a synthetic PNG and a fake Kitty-capable outer PTY, so no installed
image viewer or GUI terminal is required. It exercises the named-session
client/server bridge as well as the normal child graphics pipeline.
"""

import base64
import fcntl
import os
import re
import select
import shlex
import struct
import subprocess
import sys
import tempfile
import termios
import time
import zlib
from pathlib import Path

CELL_PIXELS = 10
IMAGE_WIDTH = 2400
IMAGE_HEIGHT = 1800
MAX_CAPTURE = 48 * 1024 * 1024
TOKEN = re.compile(rb"\x1b\[([0-9]+);([0-9]+)H|\x1b_G([^;]*);([A-Za-z0-9+/=]*)\x1b\\")


def read_once(master, output, process, deadline, label):
    if process.poll() is not None:
        raise AssertionError(
            f"Rustmux exited while waiting for {label}: {process.returncode}"
        )
    if time.monotonic() >= deadline:
        raise AssertionError(
            f"timed out waiting for {label}; captured {len(output)} bytes; "
            f"tail={bytes(output[-1000:])!r}"
        )
    if select.select([master], [], [], 0.05)[0]:
        try:
            data = os.read(master, 65536)
        except BlockingIOError:
            return b""
        output.extend(data)
        if len(output) > MAX_CAPTURE:
            raise AssertionError("outer-terminal capture exceeded 48 MiB")
        return data
    return b""


def wait_for_bytes(master, output, process, expected, timeout, label):
    deadline = time.monotonic() + timeout
    while expected not in output:
        read_once(master, output, process, deadline, label)


def wait_for_tiles(master, output, process, count):
    deadline = time.monotonic() + 60
    while len(decode_tiles(output, require_complete=False)) < count:
        read_once(master, output, process, deadline, "complete tiled overlays")


def decode_png_boundary_pixels(data):
    assert data.startswith(b"\x89PNG\r\n\x1a\n")
    offset = 8
    compressed = bytearray()
    dimensions = None
    bytes_per_pixel = None
    while offset < len(data):
        size = struct.unpack_from(">I", data, offset)[0]
        kind = data[offset + 4 : offset + 8]
        content = data[offset + 8 : offset + 8 + size]
        assert len(content) == size
        if kind == b"IHDR":
            width, height, depth, color, _, _, interlace = struct.unpack(
                ">IIBBBBB", content
            )
            assert depth == 8 and color in (2, 6) and interlace == 0
            dimensions = width, height
            bytes_per_pixel = 3 if color == 2 else 4
        elif kind == b"IDAT":
            compressed.extend(content)
        elif kind == b"IEND":
            break
        offset += size + 12
    assert dimensions is not None
    width, height = dimensions
    scanlines = zlib.decompress(compressed)
    stride = width * bytes_per_pixel
    assert len(scanlines) == height * (stride + 1)
    first_pixel = None
    previous_pixel = b"\0" * bytes_per_pixel
    for row in range(height):
        start = row * (stride + 1)
        filter_type = scanlines[start]
        assert filter_type in range(5)
        filtered = scanlines[start + 1 : start + 1 + bytes_per_pixel]
        if filter_type in (0, 1):
            predictor = b"\0" * bytes_per_pixel
        elif filter_type in (2, 4):
            predictor = previous_pixel
        else:
            predictor = bytes(value // 2 for value in previous_pixel)
        pixel = bytes((value + delta) & 255 for value, delta in zip(filtered, predictor))
        if row == 0:
            first_pixel = pixel
        previous_pixel = pixel
    if bytes_per_pixel == 3:
        first_pixel += b"\xff"
        previous_pixel += b"\xff"
    return width, height, first_pixel, previous_pixel


def decode_tiles(output, require_complete=True):
    position = None
    active = None
    tiles = []
    for token in TOKEN.finditer(output):
        if token.group(1) is not None:
            position = (int(token.group(1)), int(token.group(2)))
            continue
        controls = dict(
            part.split(b"=", 1) for part in token.group(3).split(b",") if b"=" in part
        )
        if controls.get(b"a") == b"T":
            image_id = int(controls.get(b"i", b"0"))
            assert image_id >= 0x80000000, "child graphics leaked to the outer terminal"
            image_format = controls.get(b"f")
            assert image_format in (b"24", b"32", b"100")
            assert controls.get(b"z") == b"0"
            assert active is None and position is not None
            active = {
                "id": image_id,
                "position": position,
                "format": image_format,
                "width": int(controls[b"s"]) if image_format != b"100" else None,
                "height": int(controls[b"v"]) if image_format != b"100" else None,
                "pixels": bytearray(),
            }
        elif controls.keys() != {b"m"} or active is None:
            continue
        assert len(token.group(4)) <= 4096
        active["pixels"].extend(base64.b64decode(token.group(4), validate=True))
        if controls.get(b"m") == b"0":
            if active["format"] == b"100":
                (
                    active["width"], active["height"],
                    active["first_pixel"], active["last_pixel"],
                ) = decode_png_boundary_pixels(active["pixels"])
            else:
                channels = 3 if active["format"] == b"24" else 4
                assert len(active["pixels"]) == active["width"] * active["height"] * channels
                active["first_pixel"] = active["pixels"][:channels]
                active["last_pixel"] = active["pixels"][
                    -active["width"] * channels :
                ][:channels]
                if channels == 3:
                    active["first_pixel"] += b"\xff"
                    active["last_pixel"] += b"\xff"
            tiles.append(active)
            active = None
        else:
            assert controls.get(b"m") == b"1"
    if require_complete:
        assert active is None, (
            f"incomplete final upload: complete={len(tiles)}, "
            f"active={active['id'] if active else None}, captured={len(output)}"
        )
    return tiles


def check_tiles(tiles):
    assert len(tiles) >= 3, "large image should require multiple output tiles"
    assert any(tile["format"] == b"100" for tile in tiles), (
        "compressible overlay did not use PNG"
    )
    ids = [tile["id"] for tile in tiles]
    assert len(ids) == len(set(ids))
    top = min(tile["position"][0] for tile in tiles)
    left = min(tile["position"][1] for tile in tiles)
    rectangles = sorted(
        (
            (tile["position"][0] - top) * CELL_PIXELS,
            (tile["position"][1] - left) * CELL_PIXELS,
            tile,
        )
        for tile in tiles
    )
    next_row = 0
    for row, column, tile in rectangles:
        assert (row, column) == (next_row, 0), "tile positions have a gap or overlap"
        assert tile["width"] == IMAGE_WIDTH
        for local_row, pixel in (
            (0, tile["first_pixel"]),
            (tile["height"] - 1, tile["last_pixel"]),
        ):
            expected = (
                b"\xff\0\0\xff"
                if row + local_row < IMAGE_HEIGHT // 2
                else b"\0\0\xff\xff"
            )
            assert pixel == expected
        next_row += tile["height"]
    assert next_row == IMAGE_HEIGHT
    return ids


def main(binary):
    child = Path(__file__).with_name("kitty_child_large_png.py")
    with tempfile.TemporaryDirectory(
        prefix="rustmux-large-overlay-compat-"
    ) as temporary:
        config = Path(temporary) / "config"
        config.mkdir()
        name = f"compatlarge{os.getpid()}"
        master, slave = os.openpty()
        fcntl.ioctl(
            slave, termios.TIOCSWINSZ, struct.pack("HHHH", 184, 244, 2440, 1840)
        )
        os.set_blocking(master, False)
        environment = dict(
            os.environ,
            TERM="xterm-kitty",
            RUSTMUX_SHELL="/bin/sh",
            PS1="LARGE_COMPAT_READY> ",
            ENV="",
            BASH_ENV="",
            XDG_CONFIG_HOME=str(config),
        )

        def child_setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        process = subprocess.Popen(
            [binary, "new", name],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=environment,
            preexec_fn=child_setup,  # noqa: PLW1509 - PTY session setup
        )
        output = bytearray()
        try:
            query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c"
            wait_for_bytes(master, output, process, query, 10, "outer Kitty probe")
            os.write(master, b"\x1b_Gi=31;OK\x1b\\\x1b[?1;2c")
            wait_for_bytes(
                master,
                output,
                process,
                b"LARGE_COMPAT_READY>",
                10,
                "child shell prompt",
            )
            output.clear()
            os.write(master, f"python3 {shlex.quote(str(child))}\n".encode())
            wait_for_tiles(master, output, process, 3)
            ids = check_tiles(decode_tiles(output))
            for image_id in ids:
                delete = f"\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\".encode()
                assert delete not in output, "tile disappeared before the child deleted it"

            output.clear()
            os.write(master, f"python3 {shlex.quote(str(child))} delete\n".encode())
            for image_id in ids:
                delete = f"\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\".encode()
                wait_for_bytes(
                    master, output, process, delete, 20, "outer tile deletion"
                )
            print(
                f"PASS: {len(ids)} large Kitty tiles reconstructed and deleted through named PTY"
            )

            output.clear()
            os.write(master, f"python3 {shlex.quote(str(child))} small\n".encode())
            wait_for_tiles(master, output, process, 1)
            small = decode_tiles(output)[0]
            assert small["format"] == b"24", "opaque small overlay did not use raw RGB"
            assert (small["width"], small["height"]) == (20, 10)
            assert small["first_pixel"] == b"\xff\0\0\xff"
            assert small["last_pixel"] == b"\0\0\xff\xff"
            print("PASS: opaque small Kitty overlay uses f=24 with correct pixels")
        finally:
            os.close(master)
            os.close(slave)
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
            subprocess.run(
                [binary, "kill", name], capture_output=True, timeout=5, check=False
            )


if __name__ == "__main__":
    main(sys.argv[1])

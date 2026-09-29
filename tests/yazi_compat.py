"""Opt-in end-to-end smoke against the installed Yazi image-preview protocol.

Starts Rustmux on a fake Kitty-capable outer PTY, then runs real Yazi on two
isolated 2x2 PNGs. The default run checks direct-data composition and a
pixel-only cell resize. --shared-memory accepts the separate transport probe
and checks RGBA shared-memory uploads, A-B-A reuse, and eviction recovery.
Neither run requires a GUI terminal.
"""

import base64
import ctypes
import fcntl
import mmap
import os
import re
import select
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import zlib
from pathlib import Path


FIRST_COLORS = {
    "red": (255, 0, 0, 255),
    "green": (0, 255, 0, 255),
    "blue": (0, 0, 255, 255),
    "white": (255, 255, 255, 255),
}
SECOND_COLORS = {
    "yellow": (255, 255, 0, 255),
    "cyan": (0, 255, 255, 255),
    "magenta": (255, 0, 255, 255),
    "black": (0, 0, 0, 255),
}


def png_chunk(tag, data):
    payload = tag + data
    return struct.pack(">I", len(data)) + payload + struct.pack(">I", zlib.crc32(payload))


def fixture_png(path, colors):
    # Colors are row-major; PNG rows use filter method 0, no image tool needed.
    pixels = list(colors.values())
    assert len(pixels) == 4
    rows = b"".join(
        b"\0" + b"".join(bytes(color) for color in pixels[index:index + 2])
        for index in (0, 2)
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


def decode_outer_rgba_overlay(output, expected_width=None):
    """Decode the first complete Rustmux-owned Kitty raw upload to RGBA."""
    payload = bytearray()
    dimensions = None
    channels = None
    for command in re.finditer(rb"\x1b_G([^;]*);([A-Za-z0-9+/=]*)\x1b\\", output):
        controls = dict(
            part.split(b"=", 1) for part in command.group(1).split(b",") if b"=" in part
        )
        if controls.get(b"a") == b"T" and controls.get(b"f") in (b"24", b"32"):
            image_id = int(controls.get(b"i", b"0"))
            if image_id < 0x80000000:
                continue
            width = int(controls[b"s"])
            dimensions = (width, int(controls[b"v"])) if expected_width in (None, width) else None
            channels = 3 if controls[b"f"] == b"24" else 4
            payload.clear()
        elif dimensions is None or set(controls) != {b"m"}:
            continue

        encoded = command.group(2)
        assert len(encoded) <= 4096, "outer Kitty chunk exceeds the protocol limit"
        payload.extend(base64.b64decode(encoded, validate=True))
        if controls.get(b"m") == b"0":
            width, height = dimensions
            assert len(payload) == width * height * channels, (
                "outer raw dimensions do not match payload"
            )
            pixels = bytes(payload)
            if channels == 3:
                pixels = b"".join(
                    pixels[index:index + 3] + b"\xff"
                    for index in range(0, len(pixels), 3)
                )
            return image_id, width, height, pixels
        assert controls.get(b"m") == b"1", "invalid outer Kitty chunk continuation"
    raise AssertionError("no complete Rustmux-owned raw overlay found")


def read_shared_pixels(name, size):
    """Act as a Kitty terminal: map the POSIX object, then unlink it."""
    libc = ctypes.CDLL(None, use_errno=True)
    libc.shm_open.argtypes = [ctypes.c_char_p, ctypes.c_int, ctypes.c_int]
    libc.shm_open.restype = ctypes.c_int
    libc.shm_unlink.argtypes = [ctypes.c_char_p]
    libc.shm_unlink.restype = ctypes.c_int
    fd = libc.shm_open(name, os.O_RDONLY, 0)
    if fd < 0:
        raise OSError(ctypes.get_errno(), f"cannot open shared image {name!r}")
    try:
        with mmap.mmap(fd, size, access=mmap.ACCESS_READ) as mapping:
            pixels = mapping[:]
    finally:
        os.close(fd)
    if libc.shm_unlink(name) != 0:
        raise OSError(ctypes.get_errno(), f"cannot unlink shared image {name!r}")
    return pixels


def decode_outer_shared_overlay(output):
    """Read one complete Rustmux-owned Kitty shared-memory image upload."""
    for command in re.finditer(rb"\x1b_G([^;]*);([A-Za-z0-9+/=]+)\x1b\\", output):
        controls = dict(
            part.split(b"=", 1) for part in command.group(1).split(b",") if b"=" in part
        )
        if controls.get(b"a") != b"T" or controls.get(b"t") != b"s":
            continue
        image_id = int(controls.get(b"i", b"0"))
        if image_id < 0x80000000:
            continue
        assert controls.get(b"f") == b"32", "Yazi RGBA fast path was not preserved"
        width, height = int(controls[b"s"]), int(controls[b"v"])
        channels = 4
        size = width * height * channels
        assert int(controls[b"S"]) == size, "shared image size disagrees with dimensions"
        name = base64.b64decode(command.group(2), validate=True)
        assert name.startswith(b"/rmx-"), "unexpected shared image name"
        pixels = read_shared_pixels(name, size)
        return image_id, width, height, pixels
    raise AssertionError("no Rustmux-owned shared-memory overlay found")


def assert_no_direct_owned_overlay(output):
    for command in re.finditer(rb"\x1b_G([^;]*);", output):
        controls = dict(
            part.split(b"=", 1) for part in command.group(1).split(b",") if b"=" in part
        )
        if controls.get(b"a") == b"T" and int(controls.get(b"i", b"0")) >= 0x80000000:
            assert controls.get(b"t") == b"s", "Yazi preview fell back to direct-data upload"


def owned_shared_upload_bytes(output):
    total = 0
    for command in re.finditer(rb"\x1b_G([^;]*);", output):
        controls = dict(
            part.split(b"=", 1) for part in command.group(1).split(b",") if b"=" in part
        )
        if (controls.get(b"a") == b"T" and controls.get(b"t") == b"s"
                and int(controls.get(b"i", b"0")) >= 0x80000000):
            total += int(controls[b"S"])
    return total


def assert_fixture_pixels(width, height, pixels, colors):
    assert width > 0 and height > 0
    assert len(pixels) == width * height * 4
    positions = {name: [] for name in colors}
    for offset in range(0, len(pixels), 4):
        color = tuple(pixels[offset:offset + 4])
        for name, expected in colors.items():
            if color == expected:
                cell = offset // 4
                positions[name].append((cell % width, cell // width))
    for name, points in positions.items():
        assert points, f"fixture's {name} pixel is missing from the outer image"
    upper_left, upper_right, lower_left, lower_right = positions.values()
    assert max(x for x, _ in upper_left) < min(x for x, _ in upper_right), (
        "top-row preview colors are out of order"
    )
    assert max(x for x, _ in lower_left) < min(x for x, _ in lower_right), (
        "bottom-row preview colors are out of order"
    )
    assert max(y for _, y in upper_left) < min(y for _, y in lower_left), (
        "left-column preview colors are out of order"
    )
    assert max(y for _, y in upper_right) < min(y for _, y in lower_right), (
        "right-column preview colors are out of order"
    )


def main(binary, use_shared_memory=False):
    yazi = shutil.which("yazi")
    if yazi is None:
        print("SKIP: Yazi is not installed")
        return
    version = subprocess.run([yazi, "--version"], capture_output=True, text=True, check=True)
    with tempfile.TemporaryDirectory(prefix="rustmux-yazi-compat-") as temporary:
        root = Path(temporary)
        images = root / "images"
        images.mkdir()
        fixture_png(images / "a-preview.png", FIRST_COLORS)
        fixture_png(images / "b-preview.png", SECOND_COLORS)
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
            if use_shared_memory:
                shared_query = re.compile(rb"\x1b_Ga=q,t=s,[^;]*i=32;([A-Za-z0-9+/=]+)\x1b\\")
                wait_for(master, output, lambda data: shared_query.search(data), process, 8,
                         "outer shared-memory probe")
                probe_name = base64.b64decode(shared_query.search(output).group(1), validate=True)
                assert read_shared_pixels(probe_name, 3) == b"\0\0\0"
                os.write(master, b"\x1b_Gi=32;OK\x1b\\\x1b[?1;2c")
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
            overlay = re.compile(
                rb"\x1b_Ga=T,t=s,f=(?:24|32),[^;]*i=([0-9]+)[^;]*;[A-Za-z0-9+/=]+\x1b\\"
                if use_shared_memory else rb"\x1b_Ga=T,f=32,[^;]*i=([0-9]+)[^;]*;"
            )
            def completed_overlay(data, expected_width=None):
                for match in overlay.finditer(data):
                    if int(match.group(1)) < 0x80000000:
                        continue
                    if use_shared_memory:
                        return True
                    if expected_width is not None:
                        controls = dict(
                            part.split(b"=", 1)
                            for part in match.group().split(b";")[0].split(b",")
                            if b"=" in part
                        )
                        if int(controls.get(b"s", b"0")) != expected_width:
                            continue
                    final = data.find(b"\x1b_Gm=0;", match.end())
                    if final < 0 and b",m=0;" in match.group():
                        final = match.start()
                    if final >= 0 and b"\x1b\\" in data[final:]:
                        return True
                return False

            wait_for(master, output, completed_overlay, process, 20,
                     "complete composed Yazi image overlay")
            assert "\U0010eeee".encode() not in output, "child placeholder leaked to outer terminal"
            decode_overlay = (
                decode_outer_shared_overlay if use_shared_memory else decode_outer_rgba_overlay
            )
            first_id, width, height, pixels = decode_overlay(output)
            assert_fixture_pixels(width, height, pixels, FIRST_COLORS)
            if use_shared_memory:
                assert_no_direct_owned_overlay(output)
                first_placement = int(re.search(
                    rb"\x1b_Ga=T,t=s,[^;]*i=" + str(first_id).encode() + rb",p=([0-9]+),",
                    output,
                ).group(1))

            output.clear()
            second_started = time.monotonic()
            os.write(master, b"j")
            wait_for(master, output, completed_overlay, process, 20,
                     "updated Yazi image overlay")
            second_latency = time.monotonic() - second_started
            assert "\U0010eeee".encode() not in output, "child placeholder leaked after navigation"
            second_id, width, height, pixels = decode_overlay(output)
            assert second_id != first_id, "preview update reused a live outer image ID"
            assert_fixture_pixels(width, height, pixels, SECOND_COLORS)
            if use_shared_memory:
                second_upload_bytes = owned_shared_upload_bytes(output)
                assert_no_direct_owned_overlay(output)
                second_placement = int(re.search(
                    rb"\x1b_Ga=T,t=s,[^;]*i=" + str(second_id).encode() + rb",p=([0-9]+),",
                    output,
                ).group(1))
            deleted = (
                f"\x1b_Ga=d,d=i,i={first_id},p={first_placement},q=2\x1b\\"
                if use_shared_memory else f"\x1b_Ga=d,d=I,i={first_id},q=2\x1b\\"
            ).encode()
            wait_for(master, output, lambda data: deleted in data, process, 5,
                     "previous preview's outer image deletion")

            if use_shared_memory:
                output.clear()
                revisit_started = time.monotonic()
                os.write(master, b"k")
                reused = f"\x1b_Ga=p,i={first_id},p=".encode()
                wait_for(master, output, lambda data: reused in data, process, 20,
                         "cached first Yazi image placement")
                revisit_latency = time.monotonic() - revisit_started
                assert owned_shared_upload_bytes(output) == 0
                assert_no_direct_owned_overlay(output)
                deleted = (
                    f"\x1b_Ga=d,d=i,i={second_id},p={second_placement},q=2\x1b\\"
                ).encode()
                assert deleted in output, "second preview placement was not removed"
                # A terminal may evict an image with no active placements.
                # The error reply must make Rustmux upload it again.
                output.clear()
                os.write(master, f"\x1b_Gi={first_id};ENOENT:missing image\x1b\\".encode())
                wait_for(master, output, completed_overlay, process, 20,
                         "re-upload after cached image eviction")
                restored_id, _, _, pixels = decode_outer_shared_overlay(output)
                assert restored_id != first_id
                assert_fixture_pixels(width, height, pixels, FIRST_COLORS)
                print(
                    "PASS: shared-memory Yazi A-B-A reuses its first image and recovers eviction; "
                    f"B upload={second_upload_bytes} bytes, A revisit upload=0 bytes, "
                    f"observed latency={second_latency * 1000:.1f}/{revisit_latency * 1000:.1f} ms"
                )
                return

            # Keep the text grid unchanged while the physical cell width grows.
            # Rustmux must replace its outer overlay even without a new upload
            # or placeholder change from Yazi.
            assert width % 12 == 0, "unexpected initial outer cell width"
            resized_width = width // 12 * 13
            output.clear()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 1040, 480))
            os.kill(process.pid, signal.SIGWINCH)
            wait_for(master, output,
                     lambda data: completed_overlay(data, resized_width), process, 20,
                     "resized Yazi image overlay")
            third_id, new_width, new_height, pixels = decode_outer_rgba_overlay(
                output, resized_width
            )
            assert third_id != second_id, "pixel resize reused a live outer image ID"
            assert (new_width, new_height) == (resized_width, height)
            assert_fixture_pixels(new_width, new_height, pixels, SECOND_COLORS)
            deleted = f"\x1b_Ga=d,d=I,i={second_id},q=2\x1b\\".encode()
            wait_for(master, output, lambda data: deleted in data, process, 5,
                     "pre-resize outer image deletion")
            version_line = next(
                (line.strip() for line in version.stdout.splitlines() if "Version:" in line),
                version.stdout.strip().splitlines()[0] if version.stdout.strip() else "Yazi",
            )
            print(f"PASS: {version_line}: Yazi previews switch and survive cell-pixel resize")
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
    main(sys.argv[1], "--shared-memory" in sys.argv[2:])

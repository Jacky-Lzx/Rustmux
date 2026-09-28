"""Emit synthetic Kitty PNGs of different sizes, or delete the large one."""

import base64
import random
import struct
import sys
import zlib

WIDTH = 2400
HEIGHT = 1800
IMAGE_ID = 7
LARGE_INPUT_WIDTH = 2400
LARGE_INPUT_HEIGHT = 2400
LARGE_INPUT_DISPLAY_CELLS = 10


def png_chunk(kind, data):
    payload = kind + data
    return (
        struct.pack(">I", len(data)) + payload + struct.pack(">I", zlib.crc32(payload))
    )


def send_large_input_png():
    """Send an incompressible >16 MiB PNG as bounded direct-data chunks."""
    width = LARGE_INPUT_WIDTH
    height = LARGE_INPUT_HEIGHT
    output_pixels = LARGE_INPUT_DISPLAY_CELLS * 10
    first = width // (2 * output_pixels)
    last = (2 * (output_pixels - 1) + 1) * width // (2 * output_pixels)
    generator = random.Random(0x12345678)
    compressor = zlib.compressobj(level=1)
    compressed = bytearray()
    for row in range(height):
        pixels = bytearray(generator.randbytes(width * 3))
        if row == first:
            pixels[first * 3 : first * 3 + 3] = b"\xff\0\0"
        if row == last:
            pixels[last * 3 : last * 3 + 3] = b"\0\0\xff"
        compressed.extend(compressor.compress(b"\0" + pixels))
    compressed.extend(compressor.flush())
    png = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + png_chunk(b"IDAT", compressed)
        + png_chunk(b"IEND", b"")
    )
    assert 16 * 1024 * 1024 < len(png) < 32 * 1024 * 1024

    output = sys.stdout.buffer
    output.write(b"\x1b[1;1H")
    chunk_size = 96 * 1024
    for index, start in enumerate(range(0, len(png), chunk_size)):
        more = start + chunk_size < len(png)
        controls = (
            (
                f"a=T,t=d,f=100,s={width},v={height},i=9,p=1,"
                f"c={LARGE_INPUT_DISPLAY_CELLS},r={LARGE_INPUT_DISPLAY_CELLS},"
                f"z=0,C=1,q=2,m={int(more)}"
            )
            if index == 0 else f"m={int(more)}"
        )
        output.write(b"\x1b_G" + controls.encode() + b";"
                     + base64.b64encode(png[start:start + chunk_size]) + b"\x1b\\")
    output.flush()


def send_alpha_image():
    """Replace the small opaque image with a two-color translucent raw image."""
    width, height = 20, 10
    red = bytes((255, 0, 0, 128)) * width
    blue = bytes((0, 0, 255, 64)) * width
    pixels = red * (height // 2) + blue * (height // 2)
    command = (
        f"\x1b[1;1H\x1b_Ga=T,t=d,f=32,s={width},v={height},i=8,"
        f"p=1,c=2,r=1,z=0,C=1,q=2;"
    ).encode() + base64.b64encode(pixels) + b"\x1b\\"
    sys.stdout.buffer.write(command)
    sys.stdout.buffer.flush()


def main():
    if len(sys.argv) == 2 and sys.argv[1] == "delete":
        sys.stdout.buffer.write(b"\x1b_Ga=d,d=I,i=7,q=2;\x1b\\")
        sys.stdout.buffer.flush()
        return
    if len(sys.argv) == 2 and sys.argv[1] == "small":
        width, height, image_id = 20, 10, 8
    elif len(sys.argv) == 2 and sys.argv[1] == "alpha":
        send_alpha_image()
        return
    elif len(sys.argv) == 2 and sys.argv[1] == "large-input":
        send_large_input_png()
        return
    elif len(sys.argv) == 1:
        width, height, image_id = WIDTH, HEIGHT, IMAGE_ID
    else:
        raise SystemExit("usage: kitty_child_large_png.py [small|alpha|large-input|delete]")

    red = bytes((255, 0, 0, 255)) * width
    blue = bytes((0, 0, 255, 255)) * width
    compressor = zlib.compressobj(level=1)
    compressed = bytearray()
    for row in range(height):
        compressed.extend(
            compressor.compress(b"\0" + (red if row < height // 2 else blue))
        )
    compressed.extend(compressor.flush())
    png = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + png_chunk(b"IDAT", compressed)
        + png_chunk(b"IEND", b"")
    )
    encoded = base64.b64encode(png)
    # One small child APC keeps this test focused on outer tiling, not input
    # chunking. Kitty graphics transfers still enter the normal pane pipeline.
    assert len(encoded) < 128 * 1024
    command = (
        (
            f"\x1b[1;1H\x1b_Ga=T,t=d,f=100,s={width},v={height},i={image_id},"
            f"p=1,c={width // 10},r={height // 10},z=0,C=1,q=2;"
        ).encode()
        + encoded
        + b"\x1b\\"
    )
    sys.stdout.buffer.write(command + b"\nLARGE_CHILD_DONE\n")
    sys.stdout.buffer.flush()


if __name__ == "__main__":
    main()

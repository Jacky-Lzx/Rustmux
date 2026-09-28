"""Emit a large or small two-color Kitty PNG, or delete the large one."""

import base64
import struct
import sys
import zlib

WIDTH = 2400
HEIGHT = 1800
IMAGE_ID = 7


def png_chunk(kind, data):
    payload = kind + data
    return (
        struct.pack(">I", len(data)) + payload + struct.pack(">I", zlib.crc32(payload))
    )


def main():
    if len(sys.argv) == 2 and sys.argv[1] == "delete":
        sys.stdout.buffer.write(b"\x1b_Ga=d,d=I,i=7,q=2;\x1b\\")
        sys.stdout.buffer.flush()
        return
    if len(sys.argv) == 2 and sys.argv[1] == "small":
        width, height, image_id = 20, 10, 8
    elif len(sys.argv) == 1:
        width, height, image_id = WIDTH, HEIGHT, IMAGE_ID
    else:
        raise SystemExit("usage: kitty_child_large_png.py [small|delete]")

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

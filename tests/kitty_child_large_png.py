"""Emit one large, two-color Kitty PNG or delete it in a child PTY."""

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
    if len(sys.argv) != 1:
        raise SystemExit("usage: kitty_child_large_png.py [delete]")

    red = bytes((255, 0, 0, 255)) * WIDTH
    blue = bytes((0, 0, 255, 255)) * WIDTH
    compressor = zlib.compressobj(level=1)
    compressed = bytearray()
    for row in range(HEIGHT):
        compressed.extend(
            compressor.compress(b"\0" + (red if row < HEIGHT // 2 else blue))
        )
    compressed.extend(compressor.flush())
    png = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 6, 0, 0, 0))
        + png_chunk(b"IDAT", compressed)
        + png_chunk(b"IEND", b"")
    )
    encoded = base64.b64encode(png)
    # One small child APC keeps this test focused on outer tiling, not input
    # chunking. Kitty graphics transfers still enter the normal pane pipeline.
    assert len(encoded) < 128 * 1024
    command = (
        (
            f"\x1b[1;1H\x1b_Ga=T,t=d,f=100,s={WIDTH},v={HEIGHT},i={IMAGE_ID},"
            f"p=1,c={WIDTH // 10},r={HEIGHT // 10},z=0,C=1,q=2;"
        ).encode()
        + encoded
        + b"\x1b\\"
    )
    sys.stdout.buffer.write(command + b"\nLARGE_CHILD_DONE\n")
    sys.stdout.buffer.flush()


if __name__ == "__main__":
    main()

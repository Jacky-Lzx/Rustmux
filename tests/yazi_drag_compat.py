"""Real installed Yazi, including Kitty's pre-offer gesture ID semantics."""
import base64
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import time
from urllib.parse import unquote

from terminal_loop_support import Session


PREFIX = b"\x1b]72;"
ST = b"\x1b\\"


def packet(metadata, payload=None):
    return PREFIX + metadata.encode() + (b";" + payload if payload is not None else b"") + ST


def packets(data):
    result = []
    for body in re.findall(rb"\x1b\]72;(.*?)\x1b\\", data):
        meta, _, payload = body.partition(b";")
        result.append((dict(part.split(b"=", 1) for part in meta.split(b":")), payload))
    return result


def main():
    yazi = shutil.which("yazi")
    assert yazi, "an installed Yazi is required"
    version = subprocess.run([yazi, "--version"], capture_output=True, text=True, check=True).stdout.strip()
    with tempfile.TemporaryDirectory(prefix="rustmux-yazi-drag-") as temporary:
        root = Path(temporary)
        config = root / "rustmux/config.toml"
        config.parent.mkdir()
        config.write_text("drag_source=true\n")
        files = root / "files"
        files.mkdir()
        source = files / "drag-test.txt"
        source.write_bytes(b"real Yazi drag fixture\n")
        client = Session(extra_env=dict(XDG_CONFIG_HOME=str(root), YAZI_CONFIG_HOME=str(root / "yazi"),
                                        TERM="xterm-kitty"),
                         pixels=(800, 480), lifetime=35)

        def wait(predicate, detail, timeout=8):
            end = time.monotonic() + timeout
            while not predicate():
                client.read(0.05)
                assert client.child.poll() is None, (detail, client.child.returncode)
                assert time.monotonic() < end, (detail, packets(client.output), client.last_rows)

        def registration():
            return next((fields[b"i"] for fields, _ in packets(client.output)
                         if fields.get(b"t") == b"o" and fields.get(b"x") == b"1"), None)

        try:
            wait(lambda: any(fields.get(b"t") == b"q" for fields, _ in packets(client.output)),
                 "outer support probe")
            identifier = next(fields[b"i"] for fields, _ in packets(client.output)
                              if fields.get(b"t") == b"q").decode()
            client.send(packet("t=q:i=" + identifier))
            client.expect(b"RUSTMUX_READY>")
            client.output.clear()
            client.send(("exec " + shlex.quote(yazi) + " " + shlex.quote(str(source)) + "\n").encode())
            wait(registration, "Yazi source registration")
            wait(lambda: any(b"drag-test.txt" in row for row in client.last_rows), "Yazi file list")
            previous = None
            seen = set()
            for gesture_id in (None, b"0", "previous"):
                current = registration()
                assert current not in seen, "outer gesture ID was reused"
                seen.add(current)
                client.output.clear()
                # The source must first receive the mouse press: Yazi pins
                # its clicked component before processing the OSC 72 offer.
                # Outer header + pane title occupy the first two rows.
                client.send(b"\x1b[<0;31;4M")
                metadata = "t=o:x=30:y=3:X=305:Y=67"
                if gesture_id is not None:
                    metadata += ":i=" + (previous if gesture_id == "previous" else gesture_id).decode()
                client.send(packet(metadata))
                wait(lambda: any(fields.get(b"t") == b"P" for fields, _ in packets(client.output)),
                     "real Yazi drag offer/start")
                commands = packets(client.output)
                assert all(fields[b"i"] == current for fields, _ in commands)
                assert any(fields.get(b"t") == b"o" and payload == b"text/uri-list"
                           for fields, payload in commands)
                uri_data = b"".join(payload for fields, payload in commands
                                    if fields.get(b"t") == b"p" and fields.get(b"x") == b"0")
                uri = base64.b64decode(uri_data + b"=" * (-len(uri_data) % 4)).decode()
                assert uri.startswith("file://")
                assert Path(unquote(uri[7:])).resolve() == source.resolve(), uri
                assert Path(unquote(uri[7:])).read_bytes() == source.read_bytes()
                assert any(fields.get(b"t") == b"p" and fields.get(b"x") == b"-1"
                           for fields, _ in commands), "Yazi thumbnail missing"
                assert any(fields.get(b"t") == b"p" and fields.get(b"x") == b"0" and not payload
                           for fields, payload in commands), "URI end-of-data missing"
                client.output.clear()
                client.send(packet("t=E:i=" + current.decode(), b"OK"))
                client.send(packet("t=e:x=4:y=0:i=" + current.decode()))
                client.send(b"\x1b[<0;31;4m")
                wait(registration, "source registration after completion")
                previous = current
            client.send(b"q")
            client.finish(0)
            print(f"PASS: {version}: URI data, thumbnail and start relayed for missing, zero and previous offer IDs")
        finally:
            client.close()


if __name__ == "__main__":
    main()

"""Deterministic PTY backpressure without waiting for a terminal repaint."""
from unittest.mock import patch

from terminal_loop_support import Session


for output_ready in (False, True):
    session = Session.__new__(Session)
    session.master = 7
    received = bytearray()
    blocked = False
    now = 0.0
    reads = []

    def write(fd, data):
        global blocked
        assert fd == session.master
        if blocked:
            raise BlockingIOError()
        # A macOS PTY can accept only about 1 KiB of this reply at a time.
        count = min(len(data), 1022)
        received.extend(data[:count])
        blocked = True
        return count

    def ready(readers, writers, errors, seconds):
        global blocked, now
        assert readers == [session.master] and errors == []
        if writers:
            assert writers == [session.master]
            now += 0.001
            blocked = False
            return (readers if output_ready else [], writers, [])
        # No repaint is due during discovery. A read-only wait spends its
        # timeout despite the PTY becoming writable almost immediately.
        now += seconds
        blocked = False
        return (readers if output_ready else [], [], [])

    def read(seconds=0.05):
        reads.append(seconds)
        ready([session.master], [], [], seconds)

    session.read = read
    payload = b"0123456789abcdef" * 520
    with patch("terminal_loop_support.os.write", side_effect=write), \
         patch("terminal_loop_support.select.select", side_effect=ready), \
         patch("terminal_loop_support.time.monotonic", side_effect=lambda: now):
        session.send(payload)
    assert received == payload
    # Leave room for the startup test's deliberate 150 ms response delay in
    # the production 500 ms window. The old eight read waits cost 400 ms.
    assert now < 0.35, (output_ready, now, reads)
    assert reads == ([0] * 8 if output_ready else []), reads

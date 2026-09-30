"""Configured History mode through the real binary and outer terminal."""
import os
import tempfile
import time
import subprocess
from terminal_loop_support import BINARY, Session, expect_footer

CONFIG = """
clear_defaults = true
[keybinds.locked]
"Ctrl b" = { actions = [{ action = "switch-mode", mode = "normal" }] }
"Ctrl s" = { actions = [{ action = "switch-mode", mode = "history" }] }
[keybinds.normal]
enter = { actions = [{ action = "switch-mode", mode = "history" }] }
s = { actions = [{ action = "switch-mode", mode = "history" }] }
p = { actions = [{ action = "switch-mode", mode = "pane" }] }
[keybinds.pane]
s = { actions = [{ action = "switch-mode", mode = "history" }] }
[keybinds.history]
t = { actions = ["scroll-top"] }
b = { actions = ["scroll-bottom"] }
up = { actions = ["scroll-page-up"] }
f = { actions = ["history-search-forward"] }
c = { actions = ["copy-history"] }
x = { actions = [{ action = "switch-mode", mode = "normal" }] }
p = { actions = [{ action = "switch-mode", mode = "pane" }] }
q = { actions = [{ action = "switch-mode", mode = "locked" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"""

with tempfile.TemporaryDirectory(prefix="rustmux-history-mode-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w") as file:
        file.write(CONFIG)
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; i=0; while [ $i -lt 45 ]; do printf 'ROW_%02d\\n' $i; i=$((i+1)); done\n")
        s.expect(b"ROW_44")
        # Both aliases work with every default cleared.
        s.send(b"\x02\rt")
        expect_footer(s, b"HISTORY ")
        deadline = time.monotonic() + 3
        while not any(b"ROW_00" in row for row in s.physical_rows):
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        expect_footer(s, b"f ")
        s.send(b"b")
        expect_footer(s, b"HISTORY  0/")
        s.send(b"\x1b[A")
        deadline = time.monotonic() + 3
        while b"HISTORY  0/" in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"x")
        expect_footer(s, b"NORMAL")
        s.send(b"s")
        expect_footer(s, b"HISTORY")
        s.send(b"p")
        expect_footer(s, b"PANE")
        s.send(b"s")
        expect_footer(s, b"HISTORY")
        # Remapped query and copy controls still work, and 'x' remains text in search.
        s.send(b"fROW_44\r")
        expect_footer(s, b"1/1 /ROW_44")
        s.output.clear()
        s.send(b"c")
        deadline = time.monotonic() + 3
        while b"\x1b]52;c;Uk9XXzQ0\x07" not in s.output:
            s.read()
            assert time.monotonic() < deadline, bytes(s.output[-1000:])
        s.send(b"fxxx\x03")
        expect_footer(s, b"HISTORY")
        s.send(b"q\x13")
        expect_footer(s, b"HISTORY")
        # Paste payload must not execute the configured exit or normal transition.
        s.send(b"\x1b[200~qx\x1b[201~")
        s.read(0.1)
        assert b"HISTORY" in s.physical_rows[-1]
        # Escape cancels the active submitted query, then exits on its next use.
        s.send(b"fROW_44\r")
        expect_footer(s, b"1/1 /ROW_44")
        s.send(b"\x1b")
        deadline = time.monotonic() + 3
        while b"/ROW_44" in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        assert b"HISTORY" in s.physical_rows[-1]
        s.send(b"\x1b")
        expect_footer(s, b"LOCKED")
        s.send(b"printf 'CONFIGURED_HISTORY_%s\\n' OK\n")
        s.expect(b"CONFIGURED_HISTORY_OK")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()

# Entering history on the alternate screen remains a safe ignored operation.
with tempfile.TemporaryDirectory(prefix="rustmux-history-alt-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w") as file:
        file.write(CONFIG)
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(b"stty -echo; printf '\\033[?1049hALT_HISTORY_%s\\n' READY\n")
        s.expect(b"ALT_HISTORY_READY")
        s.send(b"\x13")
        expect_footer(s, b"LOCKED")
        s.send(b"printf '\\033[?1049l'; printf 'ALT_EXIT_%s\\n' OK\n")
        s.expect(b"ALT_EXIT_OK")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()

# Named-session clients forward configured entry/exit keys to the owning server.
with tempfile.TemporaryDirectory(prefix="rustmux-history-session-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w") as file:
        file.write(CONFIG + '\n[keybinds.session]\ns = { actions = [{ action = "switch-mode", mode = "history" }] }\n')
        file.write('\n[keybinds.history.o]\nactions = [{ action = "switch-mode", mode = "session" }]\n')
    name = f"history-mode-{os.getpid()}"
    s = Session(arguments=("new", name), extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY>")
        s.send(b"\x13")
        expect_footer(s, b"HISTORY")
        s.send(b"o")
        expect_footer(s, b"SESSION")
        s.send(b"s")
        expect_footer(s, b"HISTORY")
        s.send(b"q")
        expect_footer(s, b"LOCKED")
        s.send(b"exit 0\n")
        s.finish(0)
    finally:
        s.close()
        # Clean up only this test's server after a failed assertion.
        subprocess.run([BINARY, "kill", name], capture_output=True)

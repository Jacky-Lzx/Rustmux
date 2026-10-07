"""Outer-PTY notifications integration scenarios."""
import os
import re
import shlex
import signal
import tempfile
import time

from terminal_loop_support import (
    Session,
)

# OSC 133 command lifetimes are tracked independently per pane. Short commands
# stay silent; a background command over the configured threshold rings once and
# reuses the pane/window bell marker. Disabling the notification suppresses both.
short_command_probe = r"""
import os, time
os.write(1, b"\x1b]133;C\x1b\\SHORT_START")
time.sleep(0.1)
os.write(1, b"\x1b]133;D;0\x1b\\SHORT_DONE")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.output.clear()
    s.frames.clear()
    s.send(("python3 -c " + shlex.quote(short_command_probe) + "\n").encode())
    deadline = time.monotonic() + 3
    while not any(b"SHORT_DONE" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < deadline, s.last_rows
    s.read(0.2)
    assert b"\x07" not in s.output, bytes(s.output)
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

with tempfile.TemporaryDirectory(prefix="rustmux-shortcuts-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
        config.write("[shortcuts]\nnew_window = 'N'\nsplit_right = 'R'\nsplit_down = 'D'\n")
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02")
        deadline = time.monotonic() + 3
        while b"New" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"N")
        deadline = time.monotonic() + 3
        while b"2 shell" not in s.physical_rows[0]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[0]
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
    finally:
        s.close()

with tempfile.TemporaryDirectory(prefix="rustmux-mode-keybinds-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
        config.write(
            "[keybinds.locked]\n"
            "'Ctrl b' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }\n"
            "[keybinds.normal]\n"
            "N = { actions = ['new-window', { action = 'switch-mode', mode = 'locked' }] }\n"
            "'Ctrl g' = { actions = [{ action = 'switch-mode', mode = 'locked' }] }\n"
        )
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02")
        deadline = time.monotonic() + 3
        while b"NORMAL" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"\x07")
        deadline = time.monotonic() + 3
        while b"LOCKED" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"\x02")
        deadline = time.monotonic() + 3
        while b"NORMAL" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"\x1b")
        deadline = time.monotonic() + 3
        while b"LOCKED" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"\x02N")
        deadline = time.monotonic() + 3
        while b"2 shell" not in s.physical_rows[0]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[0]
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
    finally:
        s.close()

with tempfile.TemporaryDirectory(prefix="rustmux-pane-mode-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
        config.write(
            "[keybinds.normal]\n"
            "'Ctrl p' = { actions = [{ action = 'switch-mode', mode = 'pane' }] }\n"
            "[keybinds.pane]\n"
            "h = { actions = ['focus-left'] }\n"
            "j = { actions = ['focus-down'] }\n"
            "k = { actions = ['focus-up'] }\n"
            "l = { actions = ['focus-right'] }\n"
            "left = { actions = ['focus-left'] }\n"
            "down = { actions = ['focus-down'] }\n"
            "up = { actions = ['focus-up'] }\n"
            "right = { actions = ['focus-right'] }\n"
            "r = { actions = ['new-pane-right', { action = 'switch-mode', mode = 'locked' }], display = 'always' }\n"
            "esc = { actions = [{ action = 'switch-mode', mode = 'locked' }] }\n"
        )
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02\x10")
        deadline = time.monotonic() + 3
        while b"PANE" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        assert b"Focus" in s.physical_rows[-1], s.physical_rows[-1]
        s.send(b"h")
        s.read(0.1)
        assert b"PANE" in s.physical_rows[-1], s.physical_rows[-1]
        s.send(b"\x1b")
        deadline = time.monotonic() + 3
        while b"LOCKED" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"\x02\x10r")
        deadline = time.monotonic() + 3
        while b"LOCKED" not in s.physical_rows[-1] or s.physical_rows[2].count(b"RUSTMUX_READY>") != 2:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"\x02\x10")
        deadline = time.monotonic() + 3
        while b"PANE" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"\x1b[D\x1b")
        deadline = time.monotonic() + 3
        while b"LOCKED" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"printf 'LEFT_ARROW_MARK\\n'\n")
        s.expect(b"LEFT_ARROW_MARK")
        assert any(b"LEFT_ARROW_MARK" in row[:40] for row in s.physical_rows), s.physical_rows
        s.send(b"\x02\x10")
        deadline = time.monotonic() + 3
        while b"PANE" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"\x1bOC\x1b")
        deadline = time.monotonic() + 3
        while b"LOCKED" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"printf 'RIGHT_ARROW_MARK\\n'\n")
        s.expect(b"RIGHT_ARROW_MARK")
        assert any(b"RIGHT_ARROW_MARK" in row[40:] for row in s.physical_rows), s.physical_rows
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
    finally:
        s.close()

with tempfile.TemporaryDirectory(prefix="rustmux-pane-window-move-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
        config.write(
            "[keybinds.normal]\n"
            "'Ctrl p' = { actions = [{ action = 'switch-mode', mode = 'pane' }] }\n"
            "[keybinds.pane]\n"
            "'[' = { actions = ['move-pane-previous-window', { action = 'switch-mode', mode = 'locked' }] }\n"
            "']' = { actions = ['move-pane-next-window', { action = 'switch-mode', mode = 'locked' }] }\n"
        )
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02%")
        deadline = time.monotonic() + 3
        while s.physical_rows[1].count("┌".encode()) != 2:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"RUSTMUX_RELOCATE=alive\n")
        s.send(b"printf 'BEFORE_%s\\n' \"$RUSTMUX_RELOCATE\"\n")
        s.expect(b"BEFORE_alive")
        s.send(b"\x02c")
        deadline = time.monotonic() + 3
        while b"2 shell" not in s.physical_rows[0]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[0]
        s.send(b"\x021\x02\x10")
        deadline = time.monotonic() + 3
        while b"PANE" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        assert b"Move" in s.physical_rows[-1], s.physical_rows[-1]
        s.send(b"]")
        s.read(0.2)
        s.send(b"\x021")
        deadline = time.monotonic() + 3
        while s.physical_rows[1].count("┌".encode()) != 1:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"\x022")
        deadline = time.monotonic() + 3
        while s.physical_rows[1].count("┌".encode()) != 2:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"printf 'AFTER_%s\\n' \"$RUSTMUX_RELOCATE\"\n")
        s.expect(b"AFTER_alive")
        s.send(b"\x02\x10[")
        s.read(0.2)
        s.send(b"\x022")
        deadline = time.monotonic() + 3
        while s.physical_rows[1].count("┌".encode()) != 1:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"\x021")
        deadline = time.monotonic() + 3
        while s.physical_rows[1].count("┌".encode()) != 2:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"printf 'RETURNED_%s\\n' \"$RUSTMUX_RELOCATE\"\n")
        s.expect(b"RETURNED_alive")
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
    finally:
        s.close()

with tempfile.TemporaryDirectory(prefix="rustmux-resize-mode-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
        config.write(
            "[keybinds.normal]\n"
            "r = { actions = [{ action = 'switch-mode', mode = 'resize' }] }\n"
            "[keybinds.resize]\n"
            "h = { actions = ['resize-pane-left'], display = 'always' }\n"
            "j = { actions = ['resize-pane-down'], display = 'always' }\n"
            "k = { actions = ['resize-pane-up'], display = 'always' }\n"
            "l = { actions = ['resize-pane-right'], display = 'always' }\n"
            "left = { actions = ['resize-pane-left'] }\n"
            "r = { actions = [{ action = 'switch-mode', mode = 'normal' }] }\n"
            "esc = { actions = [{ action = 'switch-mode', mode = 'locked' }] }\n"
        )
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02%")
        divider = "│".encode()
        def separator_column():
            matches = [match.start() for match in re.finditer(re.escape(divider), s.physical_rows[2])]
            return matches[1] if len(matches) >= 4 else None
        deadline = time.monotonic() + 3
        while separator_column() is None:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        original = separator_column()
        s.send(b"\x02r")
        deadline = time.monotonic() + 3
        while b"RESIZE" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        assert b"Resize" in s.physical_rows[-1], s.physical_rows[-1]
        s.send(b"h")
        deadline = time.monotonic() + 3
        while separator_column() == original:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        after_h = separator_column()
        assert b"RESIZE" in s.physical_rows[-1], s.physical_rows[-1]
        s.send(b"\x1b[D")
        deadline = time.monotonic() + 3
        while separator_column() == after_h:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"\x1b")
        deadline = time.monotonic() + 3
        while b"LOCKED" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"printf 'RESIZE_MODE_DONE\\n'\n")
        s.expect(b"RESIZE_MODE_DONE")
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
    finally:
        s.close()

with tempfile.TemporaryDirectory(prefix="rustmux-mode-transitions-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
        config.write(
            "[keybinds.normal]\n"
            "'Ctrl p' = { actions = [{ action = 'switch-mode', mode = 'pane' }] }\n"
            "[keybinds.pane]\n"
            "'Ctrl t' = { actions = [{ action = 'switch-mode', mode = 'tab' }] }\n"
            "'Ctrl m' = { actions = [{ action = 'switch-mode', mode = 'move' }] }\n"
            "esc = { actions = [{ action = 'switch-mode', mode = 'locked' }] }\n"
            "[keybinds.tab]\n"
            "'Ctrl r' = { actions = [{ action = 'switch-mode', mode = 'resize' }] }\n"
            "'Ctrl p' = { actions = [{ action = 'switch-mode', mode = 'pane' }] }\n"
            "'Ctrl m' = { actions = [{ action = 'switch-mode', mode = 'move' }] }\n"
            "[keybinds.move]\n"
            "'Ctrl r' = { actions = [{ action = 'switch-mode', mode = 'resize' }] }\n"
            "'Ctrl t' = { actions = [{ action = 'switch-mode', mode = 'tab' }] }\n"
            "[keybinds.resize]\n"
            "'Ctrl p' = { actions = [{ action = 'switch-mode', mode = 'pane' }] }\n"
            "'Ctrl t' = { actions = [{ action = 'switch-mode', mode = 'tab' }] }\n"
        )
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        for keys, badge in [
            (b"\x02\x10", b"PANE"),
            (b"\x14", b"TAB"),
            (b"\x0d", b"MOVE"),
            (b"\x12", b"RESIZE"),
            (b"\x10", b"PANE"),
            (b"\x0d", b"MOVE"),
            (b"\x14", b"TAB"),
            (b"\x12", b"RESIZE"),
        ]:
            s.send(keys)
            deadline = time.monotonic() + 3
            while badge not in s.physical_rows[-1]:
                s.read()
                assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"\x1b")
        deadline = time.monotonic() + 3
        while b"LOCKED" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"printf 'MODE_TRANSITIONS_DONE\\n'\n")
        s.expect(b"MODE_TRANSITIONS_DONE")
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
    finally:
        s.close()

with tempfile.TemporaryDirectory(prefix="rustmux-tab-mode-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
        config.write(
            "[keybinds.normal]\n"
            "'Ctrl t' = { actions = [{ action = 'switch-mode', mode = 'tab' }] }\n"
            "[keybinds.tab]\n"
            "h = { actions = ['previous-window'], display = 'always' }\n"
            "l = { actions = ['next-window'], display = 'always' }\n"
            "n = { actions = ['new-window', { action = 'switch-mode', mode = 'locked' }] }\n"
            "r = { actions = ['rename-window'], display = 'always' }\n"
            "esc = { actions = [{ action = 'switch-mode', mode = 'locked' }] }\n"
        )
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"printf 'FIRST_TAB_MARK\\n'\n")
        s.expect(b"FIRST_TAB_MARK")
        s.send(b"\x02")
        deadline = time.monotonic() + 3
        while b"NORMAL" not in s.physical_rows[-1] or b"Ctrl-T" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"\x14")
        deadline = time.monotonic() + 3
        while b"TAB" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        assert b"Previous window" in s.physical_rows[-1], s.physical_rows[-1]
        assert b"Next window" in s.physical_rows[-1], s.physical_rows[-1]
        s.send(b"n")
        deadline = time.monotonic() + 3
        while b"LOCKED" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"printf 'SECOND_TAB_MARK\\n'\n")
        s.expect(b"SECOND_TAB_MARK")
        s.send(b"\x02\x14h")
        deadline = time.monotonic() + 3
        while not any(b"FIRST_TAB_MARK" in row for row in s.physical_rows):
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        assert b"TAB" in s.physical_rows[-1], s.physical_rows[-1]
        s.send(b"l")
        deadline = time.monotonic() + 3
        while not any(b"SECOND_TAB_MARK" in row for row in s.physical_rows):
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"rRENAMED_TAB\n")
        deadline = time.monotonic() + 3
        while b"RENAMED_TAB" not in s.physical_rows[0] or b"TAB" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"\x1b")
        deadline = time.monotonic() + 3
        while b"LOCKED" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"printf 'TAB_MODE_DONE\\n'\n")
        s.expect(b"TAB_MODE_DONE")
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
    finally:
        s.close()

with tempfile.TemporaryDirectory(prefix="rustmux-move-mode-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
        config.write(
            "[keybinds.normal]\n"
            "'Ctrl m' = { actions = [{ action = 'switch-mode', mode = 'move' }] }\n"
            "[keybinds.move]\n"
            "h = { actions = ['move-pane-left'], display = 'always' }\n"
            "j = { actions = ['move-pane-down'], display = 'always' }\n"
            "k = { actions = ['move-pane-up'], display = 'always' }\n"
            "l = { actions = ['move-pane-right'], display = 'always' }\n"
            "left = { actions = ['move-pane-left'] }\n"
            "m = { actions = [{ action = 'switch-mode', mode = 'normal' }] }\n"
            "esc = { actions = [{ action = 'switch-mode', mode = 'locked' }] }\n"
        )
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"printf 'LEFT_PANE_MARK\\n'\n")
        s.expect(b"LEFT_PANE_MARK")
        s.send(b"\x02%")
        deadline = time.monotonic() + 3
        while s.physical_rows[1].count("┌".encode()) != 2:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        s.send(b"printf 'RIGHT_PANE_MARK\\n'\n")
        s.expect(b"RIGHT_PANE_MARK")
        s.send(b"\x02\x0d")
        deadline = time.monotonic() + 3
        while b"MOVE" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        assert b"Move" in s.physical_rows[-1], s.physical_rows[-1]
        s.send(b"h")
        deadline = time.monotonic() + 3
        while not any(b"RIGHT_PANE_MARK" in row[:40] for row in s.physical_rows):
            s.read()
            assert time.monotonic() < deadline, s.physical_rows
        assert any(b"LEFT_PANE_MARK" in row[40:] for row in s.physical_rows), s.physical_rows
        assert b"MOVE" in s.physical_rows[-1], s.physical_rows[-1]
        s.send(b"\x1b")
        deadline = time.monotonic() + 3
        while b"LOCKED" not in s.physical_rows[-1]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[-1]
        s.send(b"printf 'MOVED_ACTIVE_PANE\\n'\n")
        s.expect(b"MOVED_ACTIVE_PANE")
        assert any(b"MOVED_ACTIVE_PANE" in row[:40] for row in s.physical_rows), s.physical_rows
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
    finally:
        s.close()

with tempfile.TemporaryDirectory(prefix="rustmux-normal-window-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
        config.write(
            "[keybinds.normal]\n"
            "x = { actions = ['close-window', { action = 'switch-mode', mode = 'locked' }] }\n"
        )
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02c")
        deadline = time.monotonic() + 3
        while b"2 shell" not in s.physical_rows[0]:
            s.read()
            assert time.monotonic() < deadline, s.physical_rows[0]
        s.send(b"\x02x")
        s.expect(b"Close window? Type yes:")
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
    finally:
        s.close()

long_command_probe = r"""
import os, time
os.write(1, b"\x1b]133;C\x1b\\LONG_START")
time.sleep(1.1)
os.write(1, b"\x1b]133;D;0\x1b\\LONG_DONE")
"""
with tempfile.TemporaryDirectory(prefix="rustmux-notifications-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    config_path = os.path.join(directory, "rustmux", "config.toml")
    with open(config_path, "w", encoding="utf-8") as config:
        config.write("[notifications]\ncommand_duration_seconds = 1\n")
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(("python3 -c " + shlex.quote(long_command_probe) + "\n").encode())
        s.expect(b"LONG_START")
        s.send(b"\x02c")
        s.expect(b"RUSTMUX_READY> ")
        deadline = time.monotonic() + 4
        while b"1 shell [!]" not in s.physical_rows[0] or b"\x07" not in s.output:
            s.read()
            assert time.monotonic() < deadline, (s.physical_rows[0], bytes(s.output))
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
    finally:
        s.close()

    with open(config_path, "w", encoding="utf-8") as config:
        config.write(
            "[notifications]\n"
            "long_command_bell = false\n"
            "command_duration_seconds = 1\n"
        )
    s = Session(extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.output.clear()
        s.frames.clear()
        s.send(("python3 -c " + shlex.quote(long_command_probe) + "\n").encode())
        deadline = time.monotonic() + 4
        while not any(b"LONG_DONE" in row for row in s.last_rows):
            s.read()
            assert time.monotonic() < deadline, s.last_rows
        s.read(0.2)
        assert b"\x07" not in s.output, bytes(s.output)
        assert b"[!]" not in s.physical_rows[0], s.physical_rows[0]
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
    finally:
        s.close()

# Physical mouse rows are translated past the top bar; other event fields are preserved.

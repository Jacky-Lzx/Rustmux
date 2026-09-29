"""Outer-PTY windows integration scenarios."""
import fcntl
import os
import shlex
import signal
import struct
import subprocess
import termios
import tempfile
import time

from terminal_loop_support import (
    BINARY,
    Session,
    background_query,
    expect_bar,
    expect_bar_without,
    expect_emitted_footer,
    expect_footer,
)

# Real interactive windows: retain shell variables, background output and size.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"WIN=A; printf '\\033[2J\\033[H%s%s\\n' READY _A\n")
    s.expect(b"READY_A")
    # A bell from an unfocused pane marks that pane even while its window is
    # active. Selecting the window is not enough; focusing the pane clears it.
    s.send(b"sleep 0.2; printf '\\a\\033[2J\\033[H%s%s\\n' PANE _BELL\n")
    s.send(b"\x02%")
    s.expect(b"RUSTMUX_READY> ")
    deadline = time.monotonic() + 3
    while b"1 shell [!]" not in s.physical_rows[0]:
        s.read()
        assert time.monotonic() < deadline, s.physical_rows[0]
    assert b"shell [!]" in b"".join(s.physical_rows[1:]), s.last_rows
    s.send(b"\x02h")
    s.expect(b"PANE_BELL")
    deadline = time.monotonic() + 3
    while b"1 shell [!]" in s.physical_rows[0]:
        s.read()
        assert time.monotonic() < deadline, s.physical_rows[0]
    assert b"shell [!]" not in b"".join(s.physical_rows[1:]), s.last_rows
    s.send(b"\x02l")
    s.send(b"exit\n")
    deadline = time.monotonic() + 3
    while b"\xe2\x94\x82\xe2\x94\x82" in b"".join(s.last_rows):
        s.read()
        assert time.monotonic() < deadline, s.last_rows
    assert any(b"PANE_BELL" in row for row in s.last_rows), s.last_rows
    s.send(b"sleep 0.2; printf '\\a\\033[?2004h\\033[2J\\033[H%s%s\\n' BACK _A\n")
    s.send(b"\x02")
    s.send(b"c")
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"printf '\\033[2J\\033[H%s:%s\\n' WINDOW_B ${WIN-unset}\n")
    s.expect(b"WINDOW_B:unset")
    s.read(0.3)
    assert not any(b"BACK_A" in row for row in s.last_rows)
    deadline = time.monotonic() + 3
    while b"1 shell [!]" not in s.physical_rows[0]:
        s.read()
        assert time.monotonic() < deadline, s.physical_rows[0]
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 100, 0, 0))
    s.read(0.1)
    s.send(b"\x02p")
    s.expect(b"BACK_A")
    deadline = time.monotonic() + 3
    while b"1 shell [!]" in s.physical_rows[0]:
        s.read()
        assert time.monotonic() < deadline, s.physical_rows[0]
    assert s.private_modes[2004]
    s.send(b"printf '\\n%s:%s:%s\\n' RETAINED $WIN \"$(stty size)\"\n")
    s.expect(b"RETAINED:A:36 98")
    s.send(b"\x02n")
    s.expect(b"WINDOW_B:unset")
    assert not s.private_modes[2004]
    s.send(b"exit 4\n")
    s.expect(b"RETAINED:A")
    s.send(b"printf '\\n%s%s\\n' LAST _WINDOW; exit 7\n")
    s.finish(7)
    assert any(b"LAST_WINDOW" in row for row in s.last_rows)
finally:
    s.close()

# Prefix escaping and bracketed paste containing window commands reach the child.
prefix_probe = r"""
import os, select, time, tty
tty.setraw(0)
os.write(1, b"\x1b[?2004h\x1b[2J\x1b[HPREFIX_READY")
expected = b"\x02n\x02q\x1b[200~paste\x02c\x02n\x02p\x021\x020\x02\t\x02&\x02<\x02>\x1b[201~"
data = bytearray()
end = time.monotonic() + 5
while len(data) < len(expected):
    assert time.monotonic() < end, repr(data)
    if select.select([0], [], [], 0.1)[0]:
        data.extend(os.read(0, len(expected) - len(data)))
assert data == expected, repr(data)
os.write(1, b"\x1b[2J\x1b[HPREFIX_PASSED")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(prefix_probe)
        source.flush()
        s.send(("exec python3 " + shlex.quote(source.name) + "\n").encode())
        s.expect(b"PREFIX_READY")
        s.send(b"\x02\x02n\x02q\x1b[20")
        s.send(b"0~paste\x02c\x02n\x02p\x021\x020\x02\t\x02&\x02<\x02>\x1b[201~")
        s.finish(0)
        assert any(b"PREFIX_PASSED" in row for row in s.last_rows)
finally:
    s.close()

# A later spawn failure must not close or replace the existing window.
with tempfile.TemporaryDirectory(prefix="rustmux-window-spawn-") as directory:
    shell = os.path.join(directory, "shell")
    with open(shell, "w") as source:
        source.write('#!/bin/sh\nrm -- "$0"\nexport PS1="RUSTMUX_READY> "\nexec /bin/sh -i\n')
    os.chmod(shell, 0o700)
    s = Session(shell=shell)
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b'\x02c\x02%\x02"')
        s.send(b"printf '\\n%s%s\\n' SPAWN_ SURVIVED; exit 0\n")
        s.finish(0)
        assert any(b"SPAWN_SURVIVED" in row for row in s.last_rows)
    finally:
        s.close()

# An inactive child's terminal query must be answered without stealing focus.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(background_query)
        source.flush()
        s.send(("exec python3 " + shlex.quote(source.name) + "\n").encode())
        s.expect(b"QUERY_WAIT")
        s.send(b"\x02c")
        s.expect(b"RUSTMUX_READY> ")
        s.read(0.5)
        assert not any(b"QUERY_BG_OK" in row for row in s.last_rows)
        s.send(b"\x02p")
        s.expect(b"QUERY_BG_OK")
        s.send(b"x")
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"exit 0\n")
        s.finish(0)
finally:
    s.close()

# The resident-window cap and global termination cover every owned direct child.
with tempfile.TemporaryDirectory(prefix="rustmux-window-limit-") as directory:
    shell = os.path.join(directory, "shell")
    record = os.path.join(directory, "pids")
    with open(shell, "w") as source:
        source.write('#!/bin/sh\nprintf "%s\\n" "$$" >> ' + shlex.quote(record) + '\n'
                     'export PS1="RUSTMUX_READY> "\nexec /bin/sh -i\n')
    os.chmod(shell, 0o700)
    s = Session(shell=shell)
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02c" * 15)
        end = time.monotonic() + 5
        while True:
            s.read()
            with open(record) as source:
                pids = [int(line) for line in source if line.strip()]
            if len(pids) == 16:
                break
            assert time.monotonic() < end, pids
        s.send(b"\x02c")
        for _ in range(4):
            s.read(0.05)
        with open(record) as source:
            assert len(source.readlines()) == 16
        os.kill(s.app_pid, signal.SIGTERM)
        s.finish(128 + signal.SIGTERM)
        for pid in pids:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                pass
            else:
                raise AssertionError(("child still alive after global shutdown", pid))
    finally:
        s.close()


# Rename edits the active window name in place while the child keeps running.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"sleep 0.2; printf '\\033[2J\\033[H%s%s\\n' WORK _DONE\n")
    s.send(b"\x02,")
    expect_emitted_footer(s, b"RENAME")
    expect_bar_without(s, b"shell")
    s.send("\x15中文e\u0301\x7f".encode())
    expect_bar(s, "1 中文e".encode())
    end = time.monotonic() + 3
    while not any(b"WORK_DONE" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < end, s.last_rows
    assert "1 中文e".encode() in s.last_rows[0]
    s.send(b"\r")
    expect_emitted_footer(s, b"LOCKED")
    s.send(b"\x02,")
    expect_emitted_footer(s, b"RENAME")
    expect_bar_without(s, "中文e".encode())
    s.send(b"discard\x1b")
    expect_emitted_footer(s, b"LOCKED")
    expect_bar(s, "1 中文e".encode())
    s.send(b"\x02,")
    expect_emitted_footer(s, b"RENAME")
    s.send("\x1b[200~粘贴\x02c\n\x1b[201~\r".encode())
    expect_bar(s, "1 粘贴c".encode())
    expect_emitted_footer(s, b"LOCKED")
    s.send(b"\x02,")
    expect_emitted_footer(s, b"RENAME")
    expect_bar_without(s, "粘贴c".encode())
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 18, 60, 0, 0))
    s.read(0.1)
    s.send(b"\x07")
    expect_emitted_footer(s, b"LOCKED")
    expect_bar(s, "1 粘贴c".encode())
    s.send(b"printf '\\n%s%s\\n' RENAME_ RESTORED; exit 0\n")
    s.finish(0)
    assert any(b"RENAME_RESTORED" in row for row in s.last_rows)
    assert not any(b"RENAME" in row for row in s.last_rows[-1:])
finally:
    s.close()

# A child exit cancels its pending rename and delivers the child's final screen.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"sleep 0.2; printf '\\033[2J\\033[H%s%s\\n' EDITOR_ EXIT; exit 7\n")
    s.send(b"\x02,")
    expect_emitted_footer(s, b"RENAME")
    s.finish(7)
    assert any(b"EDITOR_EXIT" in row for row in s.last_rows)
    assert not any(b"RENAME" in row for row in s.last_rows[-1:])
finally:
    s.close()

# Persistent bar reflects creation, focus, rename and removal without hiding content.

# The named server owns its LOCKED prefix. Reattaching from a different local
# config must use the key announced in the handshake, including the client's
# detach shortcut, rather than the new client's own config.
with tempfile.TemporaryDirectory(prefix="rustmux-server-prefix-") as server_config, \
     tempfile.TemporaryDirectory(prefix="rustmux-client-prefix-") as client_config:
    for directory, key in ((server_config, "a"), (client_config, "b")):
        os.mkdir(os.path.join(directory, "rustmux"))
        with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
            config.write(
                "[keybinds.locked]\n"
                f"'Ctrl {key}' = {{ actions = [{{ action = 'switch-mode', mode = 'normal' }}] }}\n"
            )
    prefix_name = f"prefix-{os.getpid()}"
    s = Session(arguments=("new", prefix_name), extra_env={"XDG_CONFIG_HOME": server_config})
    try:
        s.expect(b"RUSTMUX_READY> ")
        expect_footer(s, b"Ctrl-A")
        s.send(b"\x02")
        s.read(0.1)
        assert b"LOCKED" in s.physical_rows[-1], s.physical_rows[-1]
        s.send(b"\x01")
        expect_footer(s, b"NORMAL")
        s.send(b"\x01")
        expect_footer(s, b"LOCKED")
        s.send(b"\x01d")
        s.finish(0)
    except BaseException:
        subprocess.run([BINARY, "kill", prefix_name], capture_output=True, text=True, timeout=5)
        raise
    finally:
        s.close()
    s = Session(arguments=("attach", prefix_name), extra_env={"XDG_CONFIG_HOME": client_config})
    try:
        expect_bar(s, f"Rustmux ({prefix_name})".encode())
        expect_footer(s, b"Ctrl-A")
        # Reattachment preserves the shell's unfinished input, including the
        # literal control bytes sent before detach. Cancel it before expecting
        # a fresh, empty prompt for the following commands.
        s.output.clear()
        s.frames.clear()
        s.send(b"\x03")
        s.expect(b"RUSTMUX_READY> ")
        s.output.clear()
        s.frames.clear()
        s.send(b"printf '\\033[=1u'\n")
        end = time.monotonic() + 8
        while b"\x1b[=1u" not in s.output:
            s.read()
            assert time.monotonic() < end, bytes(s.output[-2000:])
        s.send(b"\x1b[97;5u\x1b[99;1u")
        expect_bar(s, b"2 shell")
        s.output.clear()
        s.frames.clear()
        s.send(b"printf '\\033[=1u'\n")
        end = time.monotonic() + 8
        while b"\x1b[=1u" not in s.output:
            s.read()
            assert time.monotonic() < end, bytes(s.output[-2000:])
        s.send(b"\x1b[97;5u\x1b[100;1u")
        s.finish(0)
    finally:
        s.close()
        subprocess.run([BINARY, "kill", prefix_name], capture_output=True, text=True, timeout=5)

# With defaults cleared, unbound NORMAL keys and client-side legacy detach
# must not run, while explicitly configured mode and session actions still work.
with tempfile.TemporaryDirectory(prefix="rustmux-clear-defaults-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
        config.write(
            "clear_defaults = true\n"
            "[keybinds.locked]\n"
            "'Ctrl a' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }\n"
            "[keybinds.normal]\n"
            "c = { actions = ['new-window', { action = 'switch-mode', mode = 'locked' }] }\n"
            "'?' = { actions = ['show-help', { action = 'switch-mode', mode = 'locked' }] }\n"
            "'Ctrl w' = { actions = ['switch-session', { action = 'switch-mode', mode = 'locked' }] }\n"
            "'Ctrl o' = { actions = [{ action = 'switch-mode', mode = 'session' }] }\n"
            "[keybinds.session]\n"
            "d = { actions = ['detach'] }\n"
        )
    clear_name = f"clear-{os.getpid()}"
    s = Session(arguments=("new", clear_name), extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        expect_footer(s, b"Ctrl-A")
        s.send(b"\x01")
        expect_footer(s, b"NORMAL")
        expect_footer(s, b"Sessions")
        assert b"Split" not in s.physical_rows[-1], s.physical_rows[-1]
        s.send(b"c")
        expect_bar(s, b"2 shell")
        s.output.clear()
        s.send(b"\x01?")
        end = time.monotonic() + 8
        while b"Shortcut Help" not in s.output or b"New window" not in s.output:
            s.read()
            assert time.monotonic() < end, bytes(s.output[-2000:])
        assert b"Close window" not in s.output, bytes(s.output[-2000:])
        s.output.clear()
        s.frames.clear()
        s.send(b"q")
        s.expect(b"RUSTMUX_READY> ")
        # The prompt can be drawn before the frame that clears Help arrives.
        end = time.monotonic() + 8
        while any(b"Shortcut Help" in row for row in s.physical_rows):
            s.read()
            assert time.monotonic() < end, s.physical_rows
        assert not any(b"Shortcut Help" in row for row in s.physical_rows)
        expect_footer(s, b"LOCKED")
        s.output.clear()
        s.send(b"\x01\x17")
        end = time.monotonic() + 8
        while b"Session Manager" not in s.output:
            s.read()
            assert time.monotonic() < end, bytes(s.output[-2000:])
        s.output.clear()
        s.frames.clear()
        s.send(b"q")
        s.expect(b"RUSTMUX_READY> ")
        expect_footer(s, b"LOCKED")
        s.send(b"\x01d")
        s.read(0.1)
        assert s.child.poll() is None, "unbound detach left the session"
        s.send(b"\x01\x0f")
        expect_footer(s, b"SESSION")
        s.send(b"d")
        s.finish(0)
    finally:
        s.close()
        subprocess.run([BINARY, "kill", clear_name], capture_output=True, text=True, timeout=5)

s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    expect_bar(s, b"1 shell")
    expect_footer(s, b"LOCKED")
    expect_footer(s, b"Ctrl-B")
    expect_footer(s, b"Commands")
    s.send(b"\x02")
    expect_footer(s, b"NORMAL")
    expect_footer(s, b"New")
    expect_footer(s, b"h/j/k/l")
    expect_footer(s, b"Focus")
    s.send(b"n")
    expect_footer(s, b"LOCKED")
    expect_footer(s, b"Ctrl-B")
    expect_footer(s, b"Commands")

    # The reserved Help footer opens the panel. Clicking its first command uses
    # the same New action as the keyboard shortcut and consumes the release.
    s.send(b"\x02")
    expect_footer(s, b"Help")
    s.output.clear()
    s.send(b"\x1b[<0;67;24M\x1b[<0;67;24m")
    end = time.monotonic() + 8
    while b"Shortcut Help" not in s.output or b"Browse history" not in s.output:
        s.read()
        assert time.monotonic() < end, bytes(s.output[-2000:])
    s.output.clear()
    s.send(b"\x1b[<0;7;6M\x1b[<0;7;6m")
    expect_bar(s, b"2 shell")
    s.send(b"exit 0\n")
    expect_bar_without(s, b"2 shell")
    expect_bar(s, b"1 shell")

    # A listed keyboard command also closes Help and dispatches through the
    # normal shortcut table.
    s.output.clear()
    s.send(b"\x02?")
    end = time.monotonic() + 8
    while b"Shortcut Help" not in s.output:
        s.read()
        assert time.monotonic() < end, bytes(s.output[-2000:])
    s.send(b"c")
    expect_bar(s, b"2 shell")
    s.send(b"exit 0\n")
    expect_bar_without(s, b"2 shell")
    expect_bar(s, b"1 shell")

    # Unknown bytes stay modal; q closes the panel and the next shell command
    # proves that neither byte leaked into its input queue.
    s.output.clear()
    s.send(b"\x02?")
    end = time.monotonic() + 8
    while b"Shortcut Help" not in s.output:
        s.read()
        assert time.monotonic() < end, bytes(s.output[-2000:])
    s.frames.clear()
    s.send(b"vq")
    end = time.monotonic() + 8
    while not s.frames or any(b"Shortcut Help" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < end, s.last_rows
    s.send(b"printf 'HELP_OK\\n'\n")
    s.expect(b"HELP_OK")

    # The visible ? close action shares the same modal state.
    s.output.clear()
    s.send(b"\x02?")
    end = time.monotonic() + 8
    while b"Shortcut Help" not in s.output:
        s.read()
        assert time.monotonic() < end, bytes(s.output[-2000:])
    s.frames.clear()
    s.send(b"?")
    end = time.monotonic() + 8
    while not s.frames or any(b"Shortcut Help" in row for row in s.last_rows):
        s.read()
        assert time.monotonic() < end, s.last_rows

    s.send(b"printf '\\033[23;1H%s%s' LAST_ CONTENT\n")
    s.expect(b"LAST_CONTENT")
    assert b"LAST_CONTENT" in s.last_rows[20]
    assert b"1 shell" in s.last_rows[0]
    s.send(b"\x02c")
    s.expect(b"RUSTMUX_READY> ")
    expect_bar(s, b"2 shell")
    s.send("\x02,\x15中文\r".encode())
    expect_bar(s, "2 中文".encode())
    s.send(b"\x02p")
    expect_bar(s, b"1 shell")
    assert "2 中文".encode() in s.last_rows[0]
    s.send(b"\x02n")
    expect_bar(s, "2 中文".encode())
    s.send(b"exit 0\n")
    expect_bar_without(s, "2 中文".encode())
    expect_bar(s, b"1 shell")
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 1, 80, 0, 0))
    s.read(0.1)
    s.send(b"printf '\\033[2J\\033[H%s%s' ONE_ ROW\n")
    s.expect(b"ONE_ROW")
    assert len(s.last_rows) == 1 and b"1 shell" not in s.last_rows[0]
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 4, 80, 0, 0))
    expect_bar(s, b"1 shell")
    s.send(b"exit 0\n")
    s.finish(0)
finally:
    s.close()

# Visible footer hints execute the same server-side actions as their keyboard
# shortcuts. Press/release is consumed once and grouped keys use the clicked key.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 120, 0, 0))
    s.read(0.1)
    s.send(b"CLICK_WIN=1\n")
    s.send(b"\x1b[<0;11;24M\x1b[<0;11;24m")
    expect_footer(s, b"NORMAL")
    expect_footer(s, b"New")
    s.send(b"\x1b[<0;11;24M\x1b[<0;11;24m")
    s.expect(b"RUSTMUX_READY> ")
    expect_bar(s, b"2 shell")
    s.send(b"CLICK_WIN=2\n")

    s.send(b"\x1b[<0;11;24M\x1b[<0;11;24m")
    expect_footer(s, b"NORMAL")
    s.send(b"\x1b[<0;67;24M\x1b[<0;67;24m")
    s.send(b"printf '\nFOOTER_NEXT:%s\n' $CLICK_WIN\n")
    s.expect(b"FOOTER_NEXT:1")

    s.send(b"\x1b[<0;11;24M\x1b[<0;11;24m")
    s.send(b"\x1b[<0;69;24M\x1b[<0;69;24m")
    s.send(b"printf '\nFOOTER_PREVIOUS:%s\n' $CLICK_WIN\n")
    s.expect(b"FOOTER_PREVIOUS:2")
    s.send(b"exit 0\n")
    expect_bar_without(s, b"2 shell")
    s.send(b"exit 0\n")
    s.finish(0)
finally:
    s.close()

bar_mouse = r"""
import os, select, time, tty
tty.setraw(0)
os.write(1, b"\x1b[?1000;1006h\x1b[2J\x1b[HBAR_MOUSE_READY")
expected = b"\x1b[<0;2;20M\x1b[<0;2;1mx"
data = bytearray()
end = time.monotonic() + 4
while len(data) < len(expected):
    assert time.monotonic() < end, repr(data)
    if select.select([0], [], [], 0.1)[0]:
        data.extend(os.read(0, len(expected) - len(data)))
assert data == expected, repr(data)
os.write(1, b"\x1b[2J\x1b[HBAR_MOUSE_OK")
"""
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    with tempfile.NamedTemporaryFile(mode="w", suffix=".py") as source:
        source.write(bar_mouse)
        source.flush()
        s.send(("exec python3 " + shlex.quote(source.name) + "\n").encode())
        s.expect(b"BAR_MOUSE_READY")
        s.send(b"\x1b[<0;3;24M\x1b[<0;3;24m\x1b[<64;3;24M")
        s.send(b"\x1b[<0;3;22M\x1b[<0;3;1mx")
        s.finish(0)
        assert any(b"BAR_MOUSE_OK" in row for row in s.last_rows)
finally:
    s.close()

# Window labels remain clickable and the bar remains scrollable when the child
# itself has mouse reporting off.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"WIN=1\n\x02c")
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"WIN=2\n")
    s.expect(b"RUSTMUX_READY> ")
    # Legacy button reports match the encoding selected for a shell with mouse off.
    s.send(b"\x1b[M -!\x1b[M#-!printf '\nCLICKED:%s\n' $WIN\n")
    s.expect(b"CLICKED:2")
    s.send(b"\x1b[M \"!\x1b[M#\"!printf '\nCLICKED:%s\n' $WIN\n")
    s.expect(b"CLICKED:1")
    # Legacy wheel reports use button codes 64 (up) and 65 (down). The column
    # is deliberately outside both labels: the complete bar is scrollable.
    s.send(b"\x1b[Ma>!printf '\nSCROLLED:%s\n' $WIN\n")
    s.expect(b"SCROLLED:2")
    s.send(b"\x1b[M`>!printf '\nSCROLLED:%s\n' $WIN\n")
    s.expect(b"SCROLLED:1")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

# Numeric selection follows visible positions, including after earlier removal.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    for number in range(1, 11):
        if number > 1:
            s.send(b"\x02c")
            s.expect(b"RUSTMUX_READY> ")
        s.send(("WIN=%d; printf '\\033[2J\\033[HREADY_%%s\\n' $WIN\n" % number).encode())
        s.expect(("READY_%d" % number).encode())
    s.send(b"\x02")
    s.send(b"1printf '\\nSELECTED:%s\\n' $WIN\n")
    s.expect(b"SELECTED:1")
    expect_bar(s, b"1 shell")
    s.send(b"\x020printf '\\nSELECTED:%s\\n' $WIN\n")
    s.expect(b"SELECTED:10")
    expect_bar(s, b"10 shell")
    s.send(b"\x02\tprintf '\\nLAST:%s\\n' $WIN\n")
    s.expect(b"LAST:1")
    expect_bar(s, b"1 shell")
    s.send(b"\x02\tprintf '\\nBACK:%s\\n' $WIN\n")
    s.expect(b"BACK:10")
    expect_bar(s, b"10 shell")
    s.send(b"\x021exit 0\n")
    s.expect(b"\r\nREADY_2\r\n")
    expect_bar(s, b"1 shell")
    s.send(b"\x029printf '\\nSHIFTED:%s\\n' $WIN\n")
    s.expect(b"SHIFTED:10")
    expect_bar(s, b"9 shell")
    # Position ten is now absent. Both missing and current selections preserve input routing.
    s.send(b"\x020\x029printf '\\nSTILL:%s\\n' $WIN\n")
    s.expect(b"STILL:10")
    s.send(b"\x021printf '\\nFIRST:%s\\n' $WIN\n")
    s.expect(b"FIRST:2")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

# Explicit close: cancel, bracketed-paste confirmation, survivor input and child cleanup.
with tempfile.TemporaryDirectory() as directory:
    record = os.path.join(directory, "closing.pid")
    s = Session()
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"KEEP=survivor\n")
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02c")
        s.expect(b"RUSTMUX_READY> ")
        s.send(("echo $$ > " + shlex.quote(record) + "\n").encode())
        s.expect(b"RUSTMUX_READY> ")
        end = time.monotonic() + 3
        while not os.path.exists(record):
            s.read()
            assert time.monotonic() < end
        with open(record) as source:
            closing_pid = int(source.read())
        for answer in (b"\r", b"no\r", b"yes\x07", b"yes\x03", b"yes\x1b"):
            s.send(b"\x02&")
            s.expect(b"Close window? Type yes:")
            s.send(answer)
            end = time.monotonic() + 3
            while s.last_rows[0].startswith(b"Close window?"):
                s.read()
                assert time.monotonic() < end, s.last_rows
            os.kill(closing_pid, 0)
            expect_bar(s, b"2 shell")
        s.send(b"sleep 0.2; printf '\\033[2J\\033[H%s%s\\n' CLOSE_ BACKGROUND\n")
        s.send(b"\x02&")
        s.expect(b"Close window? Type yes:")
        s.expect(b"CLOSE_BACKGROUND")
        fcntl.ioctl(s.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 18, 60, 0, 0))
        s.send(b"\x1b[200~yes\r\n\x1b[201~")
        s.expect(b"Close window? Type yes: yes")
        os.kill(closing_pid, 0) # Pasted newline must not confirm.
        s.send(b"\rLEAK=1\n")
        expect_bar(s, b"1 shell")
        try:
            os.kill(closing_pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError("closed direct child still alive")
        s.send(b"printf '\\nSURVIVOR:%s:%s\\n' $KEEP ${LEAK-unset}\n")
        s.expect(b"SURVIVOR:survivor:unset")
        s.send(b"\x02&")
        s.expect(b"Close window? Type yes:")
        s.send(b"yes\r")
        s.finish(0)
    finally:
        s.close()

# Natural child exit while confirming retains its normal exit status.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    s.send(b"sleep 0.2; exit 7\n\x02&")
    s.expect(b"Close window? Type yes:")
    s.finish(7)
finally:
    s.close()

# Moving windows changes displayed positions, not shells or last-window identity.
s = Session()
try:
    s.expect(b"RUSTMUX_READY> ")
    for name in ("A", "B", "C"):
        if name != "A":
            s.send(b"\x02c")
            s.expect(b"RUSTMUX_READY> ")
        s.send(("WIN=%s\n" % name).encode())
        s.expect(b"RUSTMUX_READY> ")
        s.send(("\x02,\x15%s\r" % name).encode())
        expect_bar(s, ("%d %s" % (ord(name) - ord("A") + 1, name)).encode())
    s.send(b"\x02<printf '\\nMOVED:%s\\n' $WIN\n")
    s.expect(b"MOVED:C")
    expect_bar(s, b"2 C")
    s.send(b"\x02\tprintf '\\nLAST:%s\\n' $WIN\n")
    s.expect(b"LAST:B")
    expect_bar(s, b"3 B")
    s.send(b"\x022\x02>\x02>printf '\\nRIGHT:%s\\n' $WIN\n")
    s.expect(b"RIGHT:C")
    expect_bar(s, b"1 C")
    s.send(b"\x02<printf '\\nWRAPPED:%s\\n' $WIN\n")
    s.expect(b"WRAPPED:C")
    expect_bar(s, b"3 C")
    s.send(b"\x02<\x02<printf '\\nLEFT:%s\\n' $WIN\n")
    s.expect(b"LEFT:C")
    expect_bar(s, b"1 C")
    s.send(b"exit 0\n")
    expect_bar_without(s, b"1 C")
    expect_bar(s, b"1 A")
    s.send(b"\x02\tprintf '\\nSURVIVING:%s\\n' $WIN\n")
    s.expect(b"SURVIVING:B")
    expect_bar(s, b"2 B")
    os.kill(s.app_pid, signal.SIGTERM)
    s.finish(128 + signal.SIGTERM)
finally:
    s.close()

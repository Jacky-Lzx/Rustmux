"""Outer-PTY sessions integration scenarios."""
import fcntl
import os
import struct
import subprocess
import termios
import tempfile
import time

from terminal_loop_support import (
    BINARY,
    DEFAULT_CONFIG_DIR,
    Session,
    expect_bar,
    expect_footer,
)



# Attaching to an unknown name reports the missing session instead of its socket path.
missing_name = f"missing-{os.getpid()}"
missing = subprocess.run(
    [BINARY, "attach", missing_name], capture_output=True, text=True,
)
assert missing.returncode == 1, missing
assert missing.stderr.endswith(
    f"rustmux: session '{missing_name}' does not exist\n"
), missing.stderr


# A named server survives client detach and preserves its shell for reattachment.
session_name = f"integration-{os.getpid()}"
session_socket = f"/tmp/rustmux-{os.geteuid()}/{session_name}.sock"
s = Session(arguments=("new", session_name))
try:
    s.expect(b"RUSTMUX_READY>")
    expect_bar(s, f"Rustmux ({session_name})".encode())
    expect_footer(s, b"LOCKED")
    s.send(b"\x02")
    expect_footer(s, b"NORMAL")
    s.send(b"n")
    expect_footer(s, b"LOCKED")
    s.send(b"stty -echo; KEEP=persistent\n")
    s.expect(b"RUSTMUX_READY>")
    assert session_name in subprocess.run(
        [BINARY, "list"], check=True, capture_output=True, text=True,
    ).stdout.splitlines()
    occupied = subprocess.run(
        [BINARY, "attach", session_name], capture_output=True, text=True,
    )
    assert occupied.returncode == 1, occupied
    assert occupied.stderr.endswith(
        f"rustmux: session '{session_name}' already has an attached client\n"
    ), occupied.stderr
    s.send(b"\x02d")
    s.finish(0)
finally:
    s.close()
assert os.path.exists(session_socket), session_socket
listed = subprocess.run(
    [BINARY, "list"], check=True, capture_output=True, text=True,
).stdout.splitlines()
assert session_name in listed, listed

s = Session(arguments=("attach", session_name))
try:
    s.expect(b"RUSTMUX_READY>")
    expect_bar(s, f"Rustmux ({session_name})".encode())
    s.send(b"printf 'SESSION_%s\\n' \"$KEEP\"\n")
    s.expect(b"SESSION_persistent")
    s.send(b"exit 0\n")
    s.finish(0)
finally:
    s.close()
end = time.monotonic() + 3
while os.path.exists(session_socket):
    time.sleep(0.01)
    assert time.monotonic() < end, session_socket


# Killing an attached named session stops its server, restores the client terminal,
# and removes every endpoint sidecar.
kill_name = f"kill-{os.getpid()}"
session_directory = f"/tmp/rustmux-{os.geteuid()}"
kill_paths = [
    f"{session_directory}/{kill_name}.sock",
    f"{session_directory}/{kill_name}.lock",
    f"{session_directory}/{kill_name}.pid",
]
s = Session(arguments=("new", kill_name))
try:
    s.expect(b"RUSTMUX_READY>")
    killed = subprocess.run(
        [BINARY, "kill", kill_name], capture_output=True, text=True,
    )
    assert killed.returncode == 0, killed
    s.finish(143)
finally:
    s.close()
assert not [path for path in kill_paths if os.path.exists(path)], kill_paths


# The same command terminates a server after its only client has detached.
detached_name = f"kill-detached-{os.getpid()}"
detached_paths = [
    f"{session_directory}/{detached_name}.sock",
    f"{session_directory}/{detached_name}.lock",
    f"{session_directory}/{detached_name}.pid",
]
s = Session(arguments=("new", detached_name))
try:
    s.expect(b"RUSTMUX_READY>")
    s.send(b"\x02d")
    s.finish(0)
finally:
    s.close()
killed = subprocess.run(
    [BINARY, "kill", detached_name], capture_output=True, text=True,
)
assert killed.returncode == 0, killed
assert not [path for path in detached_paths if os.path.exists(path)], detached_paths


# Detached creation works without a controlling terminal, becomes listable before
# returning, and adopts the dimensions of its first real attachment.
background_name = f"background-{os.getpid()}"
background_env = dict(
    os.environ, RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="",
    XDG_CONFIG_HOME=DEFAULT_CONFIG_DIR.name,
)
created = subprocess.run(
    [BINARY, "new", "--detached", background_name], capture_output=True, text=True,
    env=background_env,
)
assert created.returncode == 0, created
assert created.stdout == "", created.stdout
assert background_name in subprocess.run(
    [BINARY, "list"], check=True, capture_output=True, text=True,
).stdout.splitlines()
s = Session(arguments=("attach", background_name))
try:
    s.expect(b"RUSTMUX_READY>")
    expect_bar(s, f"Rustmux ({background_name})".encode())
    s.send(b"stty size\n")
    s.expect(b"20 78")
    s.send(b"exit 0\n")
    s.finish(0)
finally:
    s.close()


# SESSION mode uses configured keys and server-requested detach. Both ordinary
# and Kitty-encoded input must stay out of the child shell.
with tempfile.TemporaryDirectory(prefix="rustmux-session-mode-") as directory:
    os.mkdir(os.path.join(directory, "rustmux"))
    with open(os.path.join(directory, "rustmux", "config.toml"), "w", encoding="utf-8") as config:
        config.write(
            "[keybinds.normal]\n"
            "'Ctrl o' = { actions = [{ action = 'switch-mode', mode = 'session' }] }\n"
            "[keybinds.session]\n"
            "d = { actions = ['detach'], display = 'always' }\n"
            "w = { actions = ['switch-session', { action = 'switch-mode', mode = 'locked' }], display = 'always' }\n"
            "o = { actions = [{ action = 'switch-mode', mode = 'normal' }], display = 'always' }\n"
            "esc = { actions = [{ action = 'switch-mode', mode = 'locked' }] }\n"
        )
    session_mode_name = f"mode-{os.getpid()}"
    s = Session(arguments=("new", session_mode_name), extra_env={"XDG_CONFIG_HOME": directory})
    try:
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"\x02\x0f")
        expect_footer(s, b"SESSION")
        expect_footer(s, b"Detach")
        s.send(b"o")
        expect_footer(s, b"NORMAL")
        s.send(b"\x0f")
        expect_footer(s, b"SESSION")
        s.output.clear()
        s.send(b"w")
        end = time.monotonic() + 8
        while b"Session Manager" not in s.output:
            s.read()
            assert time.monotonic() < end, bytes(s.output[-2000:])
        # Require a prompt emitted by the reattached client, not one retained
        # from before the session manager took over the terminal.
        s.output.clear()
        s.frames.clear()
        s.send(b"q")
        s.expect(b"RUSTMUX_READY> ")
        s.send(b"printf '\\033[=1u'\n")
        end = time.monotonic() + 8
        while b"\x1b[=1u" not in s.output:
            s.read()
            assert time.monotonic() < end, bytes(s.output[-2000:])
        s.send(b"\x1b[98;5u\x1b[111;5u")
        expect_footer(s, b"SESSION")
        s.send(b"\x1b[100;1u")
        s.finish(0)
        assert session_mode_name in subprocess.run(
            [BINARY, "list"], check=True, capture_output=True, text=True,
        ).stdout.splitlines()
    finally:
        s.close()
        subprocess.run([BINARY, "kill", session_mode_name], capture_output=True, text=True, timeout=5)

# Omitting the attach name opens a picker when several sessions are live. Its
# selection is based on the same sorted list as the CLI and restores the outer
# terminal before the selected session client takes over.
picker_helper = f"picker-zhelper-{os.getpid()}"
picker_target = f"picker-atarget-{os.getpid()}"
picker_second = f"picker-second-{os.getpid()}"
picker_created = f"picker-created-{os.getpid()}"
for name in (picker_helper, picker_target):
    created = subprocess.run(
        [BINARY, "new", "--detached", name], capture_output=True, text=True,
        env=background_env,
    )
    assert created.returncode == 0, created
long_listing = subprocess.run(
    [BINARY, "list", "--long"], check=True, capture_output=True, text=True,
).stdout
assert "SESSION" in long_listing, long_listing
assert "STATUS" in long_listing, long_listing
assert "PID" in long_listing, long_listing
assert "LAST CONNECTED" in long_listing, long_listing
assert picker_helper in long_listing, long_listing
assert "DETACHED" in long_listing, long_listing
assert "\x1b" not in long_listing, repr(long_listing)
picker = None
try:
    sessions = subprocess.run(
        [BINARY, "list"], check=True, capture_output=True, text=True,
    ).stdout.splitlines()
    assert len(sessions) > 1, sessions

    picker = Session(arguments=("attach",))
    end = time.monotonic() + 8
    while b"Session Manager" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.send(b"/ATA\t")
    visible_target = b"picker-atarget"
    while visible_target not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.send(b"\r")
    picker.expect(b"RUSTMUX_READY>")
    expect_bar(picker, f"Rustmux ({picker_target})".encode())

    # Clicking the named-session footer asks the client to leave the alternate
    # screen, opens the manager with the current session selected, and attaches
    # the chosen session. Cancelling a manager opened this way reconnects the
    # session that opened it.
    expect_footer(picker, b"LOCKED")
    assert b"Ctrl-W" not in picker.physical_rows[-1]
    picker.send(b"\x1b[<0;11;24M\x1b[<0;11;24m")
    expect_footer(picker, b"NORMAL")
    expect_footer(picker, b"Ctrl-W")
    expect_footer(picker, b"Sessions")
    picker.output.clear()
    picker.send(b"\x1b[<0;49;24M\x1b[<0;49;24m")
    end = time.monotonic() + 8
    while b"Session Manager" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    while b"[CURRENT]" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.output.clear()
    picker.send(b"/zhelper\t")
    end = time.monotonic() + 8
    while picker_helper.encode() not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.output.clear()
    picker.send(b"\r")
    picker.expect(b"RUSTMUX_READY>")
    expect_bar(picker, f"Rustmux ({picker_helper})".encode())

    # The keyboard shortcut remains available from the newly attached session.
    picker.output.clear()
    picker.send(b"\x02\x17")
    end = time.monotonic() + 8
    while b"Session Manager" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.output.clear()
    picker.last_rows.clear()
    picker.send(b"q")
    picker.expect(b"RUSTMUX_READY>")
    expect_bar(picker, f"Rustmux ({picker_helper})".encode())

    # A pane can enable Kitty keyboard encoding before the next prefix. The
    # attached client forwards those sequences, so the named server must also
    # recognize the Session Manager shortcut after decoding them.
    picker.output.clear()
    picker.frames.clear()
    picker.send(b"printf '\\033[=1u'\n")
    end = time.monotonic() + 8
    while b"\x1b[=1u" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.output.clear()
    picker.send(b"\x1b[98;5u\x1b[119;5u")
    end = time.monotonic() + 8
    while b"Session Manager" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.send(b"q")
    picker.expect(b"RUSTMUX_READY>")
    expect_bar(picker, f"Rustmux ({picker_helper})".encode())
    picker.output.clear()
    picker.frames.clear()
    picker.send(b"printf '\\033[=0u'\n")
    end = time.monotonic() + 8
    while b"\x1b[=0u" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.send(b"\x02d")
    picker.finish(0)

    # Filter to the uniquely named test session before exercising deletion so
    # unrelated sessions in the user's runtime directory cannot be selected.
    # The first d only arms deletion; another key cancels that confirmation,
    # and only a fresh consecutive dd terminates the selected session.
    picker.close()
    picker = Session(arguments=("attach",))
    fcntl.ioctl(picker.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 160, 0, 0))
    end = time.monotonic() + 8
    while b"Session Manager" not in picker.output or b"LAST CONNECTED" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.output.clear()
    picker.send(b"/zhelper")
    end = time.monotonic() + 8
    while b"Search: zhelper_" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.output.clear()
    picker.send(b"\x1b")
    end = time.monotonic() + 8
    while picker_helper.encode() not in picker.output or b"<dd> Kill" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.output.clear()
    picker.send(b"d")
    end = time.monotonic() + 8
    while b"Press d again to kill" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    assert picker_helper.encode() in picker.output
    assert picker_helper in subprocess.run(
        [BINARY, "list"], check=True, capture_output=True, text=True,
    ).stdout.splitlines()
    picker.output.clear()
    picker.send(b"xdd")
    while b"Session Manager" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    assert picker_helper not in subprocess.run(
        [BINARY, "list"], check=True, capture_output=True, text=True,
    ).stdout.splitlines()
    picker.send(b"q")
    picker.finish(0)

    # `a` uses the same New shortcut as main. The entered name is validated,
    # created as a persistent session and attached after the picker restores.
    created = subprocess.run(
        [BINARY, "new", "--detached", picker_second], capture_output=True,
        text=True, env=background_env,
    )
    assert created.returncode == 0, created
    picker.close()
    picker = Session(arguments=("attach",))
    end = time.monotonic() + 8
    while b"Session Manager" not in picker.output:
        picker.read()
        assert time.monotonic() < end, bytes(picker.output[-2000:])
    picker.send(b"a" + picker_created.encode() + b"\r")
    picker.expect(b"RUSTMUX_READY>")
    expect_bar(picker, f"Rustmux ({picker_created})".encode())
    picker.send(b"exit 0\n")
    picker.finish(0)
finally:
    if picker is not None:
        picker.close()
    for name in (picker_helper, picker_target, picker_second, picker_created):
        subprocess.run(
            [BINARY, "kill", name], capture_output=True, text=True, timeout=5,
        )

"""Named attach-or-create: live identity, restored workspaces and safe failures."""
import fcntl
import os
from pathlib import Path
import shlex
import socket
import struct
import subprocess
import tempfile
import termios
import tomllib

from terminal_loop_support import BINARY, Session, expect_bar, expect_footer


with tempfile.TemporaryDirectory(prefix="rustmux-attach-create-") as temporary:
    root = Path(temporary)
    settings = root / "custom.toml"
    settings.write_text("save_scrollback=true\nscrollback_lines=100\n")
    bad_config = root / "bad.toml"
    bad_config.write_text("not valid TOML\n")
    working = root / "saved cwd"
    working.mkdir()
    env = dict(os.environ, XDG_CONFIG_HOME=str(root / "config"),
               XDG_STATE_HOME=str(root / "state"), RUSTMUX_SHELL="/bin/sh",
               PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"attach-create-{os.getpid()}"
    stale_name = f"attach-stale-{os.getpid()}"
    unsafe_name = f"attach-unsafe-{os.getpid()}"
    runtime = Path(f"/tmp/rustmux-{os.geteuid()}")
    endpoint = runtime / f"{name}.sock"
    snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
    client = None

    def run(*args, success=True):
        result = subprocess.run([BINARY, *map(str, args)], env=env, capture_output=True,
                                text=True, timeout=8)
        assert (result.returncode == 0) == success, (args, result)
        return result

    def panes():
        return tomllib.loads(run("list-panes", "-s", name, "--toml").stdout)["panes"]

    def reject(*args, text):
        failed = Session(extra_env=env, arguments=args)
        try:
            failed.finish(1)
            assert text in failed.output, bytes(failed.output)
        finally:
            failed.close()

    try:
        # Plain named attach stays strict. Interactive creation without a usable
        # terminal or configuration must not leave a waiting background server.
        assert "does not exist" in run("attach", name, success=False).stderr
        assert "must be terminals" in run("attach", name, "--create", success=False).stderr
        assert not endpoint.exists()
        reject("attach", name, "--create", "--config", str(bad_config), text=b"bad.toml")
        assert not endpoint.exists()
        nested = dict(env, RUSTMUX="1")
        blocked = subprocess.run([BINARY, "attach", name, "--create"], env=nested,
                                 capture_output=True, text=True, timeout=8)
        assert blocked.returncode == 1 and "nested Rustmux" in blocked.stderr, blocked
        assert not endpoint.exists()

        # First invocation creates; its explicit configuration enables history
        # saving. Record layout, cwd, shell state and process identities.
        client = Session(extra_env=env, arguments=("attach", name, "--create", "-c", str(settings)))
        client.expect(b"RUSTMUX_READY>")
        expect_bar(client, ("Rustmux (" + name + ")").encode())
        client.send(("stty -echo; KEEP=retained; cd " + shlex.quote(str(working)) +
                     "; printf 'CWD_%s\\n' READY\n").encode())
        client.expect(b"CWD_READY")
        client.send(b"\x02%")
        client.expect(b"RUSTMUX_READY>")
        client.send(b"\x02c")
        client.expect(b"RUSTMUX_READY>")
        client.send(b"\x02,logs\r")
        expect_bar(client, b"logs")
        fcntl.ioctl(client.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        client.send(b"stty -echo; KEEP=retained; printf 'HISTORY_%s\\nSIZE:%s\\n' MARKER \"$(stty size)\"\n")
        client.expect(b"SIZE:26 98")
        run("save", name)
        saved = snapshot.read_bytes()
        assert b"HISTORY_MARKER" in saved
        original_panes = panes()
        original_pid = (runtime / f"{name}.pid").read_bytes()
        duplicate = run("attach", name, "--create", success=False)
        assert "already has an attached client" in duplicate.stderr, duplicate
        assert (runtime / f"{name}.pid").read_bytes() == original_pid
        assert snapshot.read_bytes() == saved
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None

        # --create on a live server ignores startup configuration and snapshots.
        # A corrupt saved file cannot disrupt the existing shell or its PID.
        snapshot.write_text("version=999\n")
        client = Session(extra_env=env, arguments=("attach", name, "--create", "--config", str(bad_config)))
        client.expect(b"RUSTMUX_READY>")
        expect_bar(client, b"logs")
        assert (runtime / f"{name}.pid").read_bytes() == original_pid
        assert [(p["id"], p["pid"]) for p in panes()] == [(p["id"], p["pid"]) for p in original_panes]
        client.send(b"printf 'KEEP:%s\\n' ${KEEP-unset}\n")
        client.expect(b"KEEP:retained")
        assert snapshot.read_text() == "version=999\n"
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None
        snapshot.write_bytes(saved)
        run("kill", name)

        # A saved-only entry is restored explicitly only with --create. Changed
        # attachment dimensions keep old history off the fresh live screen.
        assert "does not exist" in run("attach", name, success=False).stderr
        assert name in run("ls").stdout.splitlines()
        client = Session(extra_env=env, arguments=("--config", str(settings), "attach", name, "--create"))
        client.expect(b"RUSTMUX_READY>")
        expect_bar(client, b"logs")
        assert b"HISTORY_MARKER" not in b"".join(client.last_rows)
        restored_panes = panes()
        assert len(restored_panes) == 3
        assert [p["window_name"] for p in restored_panes] == [p["window_name"] for p in original_panes]
        assert [p["active"] for p in restored_panes] == [p["active"] for p in original_panes]
        assert [p["selected"] for p in restored_panes] == [p["selected"] for p in original_panes]
        assert restored_panes[0]["directory"] == str(working.resolve())
        assert all(p["pid"] not in {old["pid"] for old in original_panes} for p in restored_panes)
        assert (runtime / f"{name}.pid").read_bytes() != original_pid
        assert snapshot.read_bytes() == saved
        client.send(b"stty -echo; printf 'FRESH:%s SIZE:%s\\n' ${KEEP-unset} \"$(stty size)\"\n")
        client.expect(b"FRESH:unset SIZE:20 78")
        client.send(b"\x02[g")
        expect_footer(client, b"HISTORY")
        client.expect(b"HISTORY_MARKER")
        client.send(b"q\x021\x02h")
        client.send(b"stty -echo; printf 'LEFT:%s\\n' ${KEEP-unset}\n")
        client.expect(b"LEFT:unset")
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None
        run("kill", name)

        # Invalid snapshots and a busy workspace fail without binding, rewriting
        # the evidence or stealing a competing operation's lock.
        snapshot.write_text("version=999\n")
        reject("attach", name, "--create", "-c", str(settings), text=b"missing field `rows`")
        assert not endpoint.exists() and snapshot.read_text() == "version=999\n"
        snapshot.write_bytes(saved)
        with (runtime / f"{name}.workspace").open("r+b") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            reject("attach", name, "--create", "-c", str(settings), text=b"rustmux:")
            assert not endpoint.exists() and snapshot.read_bytes() == saved

        # An unsafe pathname is never treated as a missing session. A genuinely
        # stale private socket is reclaimed by the existing creation path.
        unsafe = runtime / f"{unsafe_name}.sock"
        unsafe.write_bytes(b"preserve this file")
        unsafe.chmod(0o600)
        reject("attach", unsafe_name, "--create", text=b"not a session socket")
        assert unsafe.read_bytes() == b"preserve this file"
        unsafe.unlink()
        stale = runtime / f"{stale_name}.sock"
        with socket.socket(socket.AF_UNIX) as wire:
            wire.bind(str(stale))
            stale.chmod(0o600)
        client = Session(extra_env=env, arguments=("attach", stale_name, "--create"))
        client.expect(b"RUSTMUX_READY>")
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None
        run("kill", stale_name)
    finally:
        if client:
            client.close()
        for target in (name, stale_name):
            subprocess.run([BINARY, "kill", target], env=env, capture_output=True, timeout=8)
        for target in (name, stale_name, unsafe_name):
            (runtime / f"{target}.workspace").unlink(missing_ok=True)
        (runtime / f"{unsafe_name}.sock").unlink(missing_ok=True)

print("attach --create live identity, restoration, terminal cleanup and failure isolation passed")

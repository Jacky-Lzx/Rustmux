"""Script-created jobs keep startup metadata and reject failures before mutation."""
import json
import os
from pathlib import Path
import shlex
import socket
import struct
import subprocess
import tempfile
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_footer


with tempfile.TemporaryDirectory(prefix="rustmux-pane-startup-") as temporary:
    root = Path(temporary)
    source = root / "source"
    source.mkdir()
    work = root / "work;touch cwd-executed"
    work.mkdir()
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    config.write_text("remain_on_exit=true\nsave_scrollback=true\n")
    shell = root / "shell.sh"
    shell.write_text("#!/bin/sh\nexport PS1='RUSTMUX_READY> '\nexec /bin/sh \"$@\"\n")
    shell.chmod(0o700)
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL=str(shell), PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"pane-startup-{os.getpid()}"
    runtime = Path(f"/tmp/rustmux-{os.geteuid()}")
    client = None
    probe = root / "record.py"
    probe.write_text("""import json,os,sys
size=os.get_terminal_size(0)
with open(sys.argv[1],'a') as log:
    log.write(json.dumps({'cwd':os.getcwd(),'rows':size.lines,'columns':size.columns,'pid':os.getppid()})+'\\n')
print('STARTUP_'+sys.argv[2],flush=True)
""")

    def run(action, *arguments, success=True):
        result = subprocess.run([BINARY, *action.split(), "-s", name, *map(str, arguments)],
                                env=env, capture_output=True, text=True, timeout=8)
        assert (result.returncode == 0) == success, (action, arguments, result)
        return result

    def panes():
        return tomllib.loads(run("pane list", "--toml").stdout)["panes"]

    def active():
        return next(p["id"] for p in panes() if p["active"])

    def records(label):
        path = root / f"{label}.runs"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def wait_until(predicate, detail):
        deadline = time.monotonic() + 5
        while not predicate():
            if client:
                client.read(0.01)
            assert time.monotonic() < deadline, detail
            time.sleep(0.01)

    def command(label, finite=False):
        invocation = "python3 " + " ".join(shlex.quote(str(arg)) for arg in (probe, root / f"{label}.runs", label))
        return invocation + ("; exit 7" if finite else "; export PS1='RUSTMUX_READY> '; exec /bin/sh -i")

    def check_record(label, directory, size, count=1):
        wait_until(lambda: len(records(label)) == count, ("missing startup run", label, count))
        record = records(label)[-1]
        assert record["cwd"] == str(directory.resolve()), record
        assert (record["rows"], record["columns"]) == size, record
        return record

    def wire(body):
        with socket.socket(socket.AF_UNIX) as peer:
            peer.settimeout(3)
            peer.connect(str(runtime / f"{name}.control"))
            peer.sendall(struct.pack("!I", len(body)) + body)
            response = bytearray()
            while len(response) < 4:
                chunk = peer.recv(4096)
                assert chunk, response
                response.extend(chunk)
            length = struct.unpack("!I", response[:4])[0]
            while len(response) < length + 4:
                chunk = peer.recv(4096)
                assert chunk, response
                response.extend(chunk)
            return tomllib.loads(response[4:].decode())

    def detach():
        global client
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client = None

    try:
        client = Session(extra_env=env, arguments=("new", name))
        client.expect(b"RUSTMUX_READY>")
        first = panes()[0]
        client.send(("cd " + shlex.quote(str(source)) + "; stty -echo; KEEP=source; printf 'SOURCE_READY\\n'\n").encode())
        client.expect(b"SOURCE_READY")
        window_command = command("window")
        window = int(run("window new", "--name", "task", "--command", window_command).stdout)
        initial = check_record("window", source, (20, 78))
        assert initial["pid"] == next(p["pid"] for p in panes() if p["id"] == window)
        split_command = command("split")
        split = int(run("pane split", "-p", first["id"], "--down", "--cwd", work,
                        "--command", split_command).stdout)
        check_record("split", work, (9, 78))
        assert active() == split, "inactive split did not select its new pane"
        assert next(p["pid"] for p in panes() if p["id"] == first["id"]) == first["pid"]
        run("pane send-keys", "-p", first["id"], "--literal", "--enter", "printf 'PRESERVED:%s\\n' $KEEP")
        wait_until(lambda: "PRESERVED:source" in run("pane capture", "-p", first["id"]).stdout,
                   "source process state was lost")
        ordinary = int(run("window new", "--cwd", work).stdout)
        run("pane send-keys", "-p", ordinary, "--literal", "--enter", command("ordinary"))
        check_record("ordinary", work, (20, 78))
        inherited_command = command("inherited")
        inherited = int(run("pane split", "-p", window, "--command", inherited_command).stdout)
        check_record("inherited", source, (20, 38))
        assert active() == inherited
        assert not (root / "cwd-executed").exists() and not (work / "cwd-executed").exists()
        detach()

        client = Session(extra_env=env, arguments=("attach", name))
        client.expect(b"RUSTMUX_READY>")
        finite_command = command("finite", finite=True)
        finite = int(run("window new", "--name", "finite", "--cwd", work, "--command", finite_command).stdout)
        check_record("finite", work, (20, 78))
        wait_until(lambda: any(p["id"] == finite and p["output_complete"] for p in panes()), "job did not exit")
        stopped = next(p for p in panes() if p["id"] == finite)
        assert stopped["exit_code"] == 7 and stopped["exited"]
        assert int(run("pane respawn", "-p", finite).stdout) == finite
        check_record("finite", work, (20, 78), count=2)
        wait_until(lambda: any(p["id"] == finite and p["output_complete"] for p in panes()), "respawn did not exit")
        assert next(p["pid"] for p in panes() if p["id"] == finite) != stopped["pid"]
        client.send(b"\x02[")
        expect_footer(client, b"HISTORY")
        before = panes()
        would_run = "touch " + shlex.quote(str(root / "invalid-command-ran"))
        for action, target in (("window new", []), ("pane split", ["-p", str(window)])):
            for invalid in ([], ["--cwd", "relative"], ["--cwd", str(root / "missing")], ["--cwd", str(probe)]):
                args = ["--command", ""] if not invalid else ["--command", would_run, *invalid]
                run(action, *target, *args, success=False)
            run(action, *target, "--command", " \n\t", success=False)
            run(action, *target, "--command", "x" * 4097, success=False)
        run("pane split", "-p", 99999, "--command", would_run, success=False)
        run("window new", "--name", "", "--command", would_run, success=False)
        for action in ("new-window", "split-pane"):
            fields = f"pane={window}\ndown=false\n" if action == "split-pane" else ""
            body = (f"action='{action}'\n" + fields + 'command="bad\\u0000command"\n').encode()
            assert not wire(body)["ok"], body
        assert panes() == before
        assert not (root / "invalid-command-ran").exists()
        # A real exec failure after validation must restore a background
        # target's remembered selection and preserve the active overlay.
        offline_shell = root / "offline-shell.sh"
        shell.rename(offline_shell)
        try:
            run("window new", "--cwd", work, "--command", would_run, success=False)
            run("pane split", "-p", window, "--cwd", work, "--command", would_run, success=False)
            assert panes() == before
        finally:
            offline_shell.rename(shell)
        client.send(b"/startup-check")
        expect_footer(client, b"Search /startup-check")
        # Impossible split geometry is rejected before its factory runs.
        run("pane resize", "-p", first["id"], "--direction", "up", "--cells", 65535)
        before = panes()
        run("pane split", "-p", first["id"], "--down", "--command", would_run, success=False)
        assert panes() == before and not (root / "invalid-command-ran").exists()
        run("pane resize", "-p", first["id"], "--direction", "down", "--cells", 8)
        expect_footer(client, b"LOCKED")
        detach()

        detached_command = command("detached")
        detached = int(run("window new", "--name", "detached", "--cwd", work,
                           "--command", detached_command).stdout)
        check_record("detached", work, (20, 78))
        assert active() == detached
        subprocess.run([BINARY, "save", name], env=env, capture_output=True, check=True, timeout=8)
        snapshot = root / f"state/rustmux/main-human/sessions/{name}.toml"
        saved = tomllib.loads(snapshot.read_text())
        jobs = [p for w in saved["windows"] for p in w["panes"]]
        assert len(jobs) == 7 and saved["active_window"] == 4
        assert [p.get("command") for p in jobs] == [None, split_command, window_command, inherited_command,
                                                   None, finite_command, detached_command]
        assert Path(jobs[1]["directory"]).resolve() == work.resolve(), jobs[1]
        assert Path(jobs[2]["directory"]).resolve() == source.resolve(), jobs[2]
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, check=True, timeout=8)
        client = Session(extra_env=env, arguments=("attach", name, "--create"))
        client.expect(b"STARTUP_detached")
        check_record("window", source, (20, 38), count=2)
        check_record("split", work, (9, 78), count=2)
        check_record("inherited", source, (20, 38), count=2)
        check_record("finite", work, (20, 78), count=3)
        check_record("detached", work, (20, 78), count=2)
        assert len(records("ordinary")) == 1, "typed command was replayed as a startup command"
        assert len(panes()) == 7 and next(p["window"] for p in panes() if p["active"]) == 5
        detach()
    finally:
        if client:
            client.close()
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=8)

print("script pane startup: cwd/inheritance, actual PTYs, failure isolation, respawn and snapshot replay passed")

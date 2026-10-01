"""Open real frontend terminals even when Darwin ttyname_r lookup fails."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

from terminal_loop_support import BINARY, Session


with tempfile.TemporaryDirectory(prefix="rustmux-terminal-device-") as temporary:
    root = Path(temporary)
    env = dict(os.environ, XDG_CONFIG_HOME=str(root / "config"),
               XDG_STATE_HOME=str(root / "state"), RUSTMUX_SHELL="/bin/sh",
               PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    client_env = dict(env)
    if sys.platform == "darwin":
        source = Path(__file__).parent / "fixtures/ttyname_erange.c"
        library = root / "ttyname-erange.dylib"
        probe = root / "ttyname-probe"
        for arguments in [
            ["-dynamiclib", str(source), "-o", str(library)],
            ["-DRUSTMUX_TTYNAME_PROBE", str(source), "-o", str(probe)],
        ]:
            subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", *arguments],
                           check=True, capture_output=True, timeout=20)
        client_env["DYLD_INSERT_LIBRARIES"] = str(library)
        # Prove the loader actually forces ERANGE; this must fail without the
        # interposer so a stripped DYLD environment cannot silently pass.
        normal = subprocess.run([str(probe)], env=env, timeout=5)
        injected = subprocess.run([str(probe)], env=client_env, timeout=5)
        assert normal.returncode == 1 and injected.returncode == 0, (normal, injected)

    # Foreground setup, bidirectional I/O and mode restoration use the real
    # terminal device boundary, rather than mocking Rust helpers.
    session = Session(extra_env=client_env)
    try:
        session.expect(b"RUSTMUX_READY>")
        session.send(b"stty -echo; printf 'DEVICE_FOREGROUND_OK\\n'\n")
        session.expect(b"DEVICE_FOREGROUND_OK")
        session.send(b"exit 0\n")
        session.finish(0)
    finally:
        session.close()

    name = f"device-open-{os.getpid()}"
    subprocess.run([BINARY, "new", name, "--detached"], env=env,
                   capture_output=True, check=True, timeout=10)
    try:
        for iteration in range(3):
            session = Session(arguments=("attach", name), extra_env=client_env)
            try:
                session.expect(b"RUSTMUX_READY>")
                session.send(f"stty -echo; printf 'DEVICE_ATTACH_{iteration}\\n'\n".encode())
                session.expect(f"DEVICE_ATTACH_{iteration}".encode())
                session.send(b"\x02d")
                session.finish(0)
            finally:
                session.close()

        # The standalone manager opens its own terminal before handing it to
        # the attached client; both opens must work under the same injected fault.
        session = Session(arguments=("attach",), extra_env=client_env)
        try:
            deadline = time.monotonic() + 5
            while b"Session Manager" not in session.output:
                session.read()
                assert time.monotonic() < deadline, bytes(session.output[-2000:])
            session.send(b"/" + name.encode())
            deadline = time.monotonic() + 5
            while b"Search: " + name.encode() + b"_" not in session.output:
                session.read()
                assert time.monotonic() < deadline, bytes(session.output[-2000:])
            session.send(b"\r")
            session.expect(b"RUSTMUX_READY>")
            session.send(b"printf 'DEVICE_PICKER_OK\\n'\n")
            session.expect(b"DEVICE_PICKER_OK")
            session.send(b"exit 0\n")
            session.finish(0)
        finally:
            session.close()
    finally:
        # Only remove the server owned by this scenario if an assertion failed.
        subprocess.run([BINARY, "kill", name], env=env, capture_output=True, timeout=10)

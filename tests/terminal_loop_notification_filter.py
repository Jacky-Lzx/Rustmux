"""Foreground application filtering applies at command completion and reload."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import tomllib

from terminal_loop_support import BINARY, Session

with tempfile.TemporaryDirectory(prefix="rustmux-notification-filter-") as temporary:
    root = Path(temporary)
    config = root / "rustmux/config.toml"
    config.parent.mkdir()
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"notification-filter-{os.getpid()}"
    client = None
    python_names = list({name.casefold(): name for name in
                         ["python", "python3", Path(sys.executable).name]}.values())
    probe = root / "probe.py"
    probe.write_text("""import json,os,sys,time
from pathlib import Path
root,label,bell=sys.argv[1:]
root=Path(root)
os.write(1,b'\\x1b]2;nvim\\x1b\\\\\\x1b]133;C\\x1b\\\\'+(label+'_START').encode())
(root/(label+'.pid')).write_text(str(os.getpid()))
while not (root/(label+'.go')).exists(): time.sleep(0.01)
if bell=='yes': os.write(1,b'\\x07')
os.write(1,b'\\x1b]133;D;0\\x1b\\\\'+(label+'_DONE').encode())
""")

    def run(action, *args, success=True):
        result = subprocess.run([BINARY, action, "-s", name, *map(str,args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert (result.returncode == 0) == success, (action,args,result)
        return result

    def status():
        return tomllib.loads(run("show-config").stdout)

    def panes():
        return tomllib.loads(run("list-panes", "--toml").stdout)["panes"]

    def wait(predicate, detail):
        end = time.monotonic()+5
        while not predicate():
            if client: client.read(0.01)
            assert time.monotonic()<end, detail
            time.sleep(0.01)

    def write(excluded, enabled=True):
        config.write_text("remain_on_exit=true\nsave_scrollback=true\n[notifications]\n"
                          + f"enabled={str(enabled).lower()}\nlong_command_bell=true\ncommand_duration_seconds=1\n"
                          + "exclude_applications="+json.dumps(excluded)+"\n")
        wait(lambda: status()["settings"]["notification_excluded_applications"] == excluded
             and status()["settings"]["notifications_enabled"] == enabled and not status().get("error"),
             "notification configuration did not apply")

    def launch(pane, label, bell=False):
        command = "python3 -u "+shlex.quote(str(probe))+" "+shlex.quote(str(root))+" "+label+" "+("yes" if bell else "no")
        run("send-keys", "-p", pane, "--literal", "--enter", command)
        wait(lambda: label+"_START" in run("capture-pane", "-p", pane).stdout and (root/(label+'.pid')).exists(),
             "probe did not begin a parsed semantic command")
        return time.monotonic()

    def complete(pane, label, started, reminder, marker=None):
        wait(lambda: time.monotonic()-started>=1.15, "command threshold wait")
        if client:
            client.read(0.05)
            client.output.clear()
            client.frames.clear()
        (root/(label+'.go')).touch()
        wait(lambda: label+"_DONE" in run("capture-pane", "-p", pane).stdout, "probe did not complete")
        if client:
            client.read(0.1)
            assert (b"\x07" in client.output) == reminder, (label,bytes(client.output))
            if marker is not None:
                wait(lambda: (b"[!]" in client.physical_rows[0]) == marker, (label,client.physical_rows[0]))

    def clear_marker(pane, watcher):
        run("select-pane", "-p", pane)
        run("select-pane", "-p", watcher)
        wait(lambda: b"[!]" not in client.physical_rows[0], "previous reminder was not cleared")

    def detach():
        global client
        client.send(b"\x02d")
        client.finish(0)
        client.close()
        client=None

    try:
        config.write_text("remain_on_exit=true\n[notifications]\ncommand_duration_seconds=1\n")
        client=Session(extra_env=env,arguments=("new",name))
        client.expect(b"RUSTMUX_READY>")
        first=next(p["id"] for p in panes() if p["active"])
        watcher=int(run("new-window", "--name", "watcher").stdout)
        client.expect(b"RUSTMUX_READY>")
        original={p["id"]:p["pid"] for p in panes()}
        assert status()["settings"]["notification_excluded_applications"] == ["yazi","nvim","lazygit"]
        started=launch(first,"allowed")
        # An application-supplied title cannot impersonate an excluded executable.
        assert next(p["title"] for p in panes() if p["id"]==first)=="nvim"
        complete(first,"allowed",started,True,True)
        clear_marker(first,watcher)
        write(python_names)
        started=launch(first,"excluded")
        complete(first,"excluded",started,False,False)
        # A new pane inherits the currently applied filtering policy too.
        second=int(run("split-pane","-p",first).stdout)
        run("select-pane","-p",watcher)
        started=launch(second,"newpane")
        complete(second,"newpane",started,False,False)
        # Removing a filter during a command uses the new policy at completion.
        started=launch(first,"cleared")
        write([])
        complete(first,"cleared",started,True,True)
        clear_marker(first,watcher)
        detach()

        client=Session(extra_env=env,arguments=("attach",name))
        client.expect(b"RUSTMUX_READY>")
        # Adding a filter during an already running, otherwise silent job works.
        started=launch(first,"added")
        write(python_names)
        complete(first,"added",started,False,False)
        before=status()
        config.write_text("[notifications]\nexclude_applications=[1]\n")
        wait(lambda: status().get("error") is not None,"invalid filter did not report a reload error")
        assert status()["settings"]==before["settings"], "invalid reload changed applied settings"
        started=launch(first,"invalid_retained")
        complete(first,"invalid_retained",started,False,False)
        write([],enabled=False)
        started=launch(first,"disabled")
        complete(first,"disabled",started,False,False)
        # Disabling/filtering generated reminders does not erase an application's BEL marker.
        started=launch(first,"ordinary_bell",bell=True)
        complete(first,"ordinary_bell",started,False,True)
        clear_marker(first,watcher)
        assert all(next(p for p in panes() if p["id"]==pane)["pid"]==pid for pane,pid in original.items())
        detach()

        # Startup exec jobs use the original child PID as foreground leader.
        write(python_names)
        client=Session(extra_env=env,arguments=("attach",name))
        client.expect(b"RUSTMUX_READY>")
        command="exec python3 -u "+shlex.quote(str(probe))+" "+shlex.quote(str(root))+" startup no"
        startup=int(run("new-window","--command",command).stdout)
        wait(lambda: "startup_START" in run("capture-pane","-p",startup).stdout
             and (root/"startup.pid").exists(), "startup exec probe did not begin")
        started=time.monotonic()
        assert next(p["pid"] for p in panes() if p["id"]==startup)==int((root/"startup.pid").read_text())
        run("select-pane","-p",watcher)
        complete(startup,"startup",started,False,False)
        run("close-pane","-p",startup)
        detach()

        started=launch(first,"detached")
        complete(first,"detached",started,False)
        client=Session(extra_env=env,arguments=("attach",name))
        client.expect(b"RUSTMUX_READY>")
        wait(lambda: b"[!]" not in client.physical_rows[0], "detached excluded job left an activity marker")
        run("select-pane","-p",first)
        wait(lambda: "detached_DONE" in run("capture-pane","-p",first).stdout,"detached job output was lost")
        assert len(panes())==3 and original[first]==next(p["pid"] for p in panes() if p["id"]==first)
        detach()
    finally:
        if client: client.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)

print("notification filters: existing/new panes, in-flight reloads, invalid isolation, BEL and detached jobs passed")

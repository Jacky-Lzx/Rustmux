"""Normal focus arrows move real panes, retain mode, and share Help/footer dispatch."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_footer

CONFIG = '''clear_defaults=true
[keybinds.locked]
"Ctrl b"={actions=[{action="switch-mode",mode="normal"}]}
[keybinds.normal]
esc={actions=[{action="switch-mode",mode="locked"}],display="hidden"}
"?"={actions=["show-help",{action="switch-mode",mode="locked"}],display="always"}
left={actions=["focus-left"],display="always"}
down={actions=["focus-down"],display="always"}
up={actions=["focus-up"],display="hidden"}
right={actions=["focus-right"],display="help"}
S={actions=[{action="switch-mode",mode="session"}],display="hidden"}
[keybinds.session]
d={actions=["detach"],display="hidden"}
'''

with tempfile.TemporaryDirectory(prefix="rustmux-normal-arrows-") as temporary:
    root = Path(temporary)
    config = root / "config.toml"
    config.write_text(CONFIG)
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / "state"),
               RUSTMUX_SHELL="/bin/sh", PS1="RUSTMUX_READY> ", ENV="", BASH_ENV="")
    name = f"normal-arrows-{os.getpid()}"
    client = None
    probe = root / "probe.py"
    probe.write_text('''import json,os,select,sys,tty
from pathlib import Path
tty.setraw(0)
os.write(1,sys.argv[2].encode()+b"_READY")
data=b""
while True:
    if select.select([0],[],[],0.01)[0]:
        chunk=os.read(0,1024)
        if not chunk: break
        data+=chunk
    target=Path(sys.argv[1])
    target.with_suffix('.tmp').write_text(json.dumps({'pid':os.getpid(),'input':data.hex()}))
    os.replace(target.with_suffix('.tmp'),target)
''')

    def run(action, *args):
        target = [name] if action in ("new", "kill") else ["-s", name]
        result = subprocess.run([BINARY, action, *target, *map(str,args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, (action,args,result)
        return result.stdout

    def panes():
        return tomllib.loads(run("list-panes","--toml"))["panes"]

    def active():
        return next(p["id"] for p in panes() if p["active"])

    def wait(predicate):
        deadline=time.monotonic()+6
        while not predicate():
            client.read(0.01)
            assert time.monotonic()<deadline, (panes(),client.physical_rows)
            time.sleep(0.01)

    def command(label):
        return f"exec python3 -u {shlex.quote(str(probe))} {shlex.quote(str(root/(label+'.json')))} {label}"

    def body():
        return b"\n".join(client.physical_rows)

    def focus(sequence, pane, mode=b"NORMAL"):
        client.send(sequence)
        wait(lambda: active()==pane)
        expect_footer(client,mode)

    def click(label, footer=False):
        wait(lambda: label in (client.physical_rows[-1] if footer else body()))
        rows=[(r,t.index(label)) for r,t in enumerate(client.physical_rows) if label in t]
        assert len(rows)==1,rows
        row,column=rows[0]
        column=len(client.physical_rows[row][:column].decode('utf-8'))
        client.send(f"\x1b[<0;{column+1};{row+1}M\x1b[<0;{column+1};{row+1}m".encode())

    try:
        run("new","--detached","--config",config)
        client=Session(extra_env=env,arguments=("attach",name),lifetime=90)
        client.expect(b"RUSTMUX_READY>")
        a=panes()[0]["id"]
        run("send-keys","-p",a,"--literal","--enter",command('a'))
        b=int(run("split-pane","-p",a,"--command",command('b')))
        c=int(run("split-pane","-p",a,"--down","--command",command('c')))
        d=int(run("split-pane","-p",b,"--down","--command",command('d')))
        wait(lambda: all((root/(label+'.json')).exists() for label in 'abcd'))
        wait(lambda: all((label+'_READY').encode() in body() for label in 'abcd'))
        original={p['id']:p['pid'] for p in panes()}
        focus(b"\x02\x1b[A",b)  # Hidden Up still works.
        focus(b"\x1b[D",a)
        focus(b"\x1b[B",c)
        focus(b"\x1bOC",d)  # Application cursor encoding.
        focus(b"\x1b[1;1:2D",c)  # Kitty repeat.
        client.send(b"\x1b[1;1:3D")  # Release has no action.
        for _ in range(3): client.read()
        assert active()==c
        expect_footer(client,b"NORMAL")
        focus(b"\x1b[B",c)  # No geometric neighbor still retains Normal.
        assert b"Focus right" not in client.physical_rows[-1]
        client.send(b"?")
        wait(lambda: b"Shortcut Help" in body() and b"Focus right" in body())
        assert b"Focus up" not in body()
        click(b"Focus right")
        wait(lambda: b"Shortcut Help" not in body() and active()==d)
        expect_footer(client,b"NORMAL")
        click(b"Focus left",footer=True)
        wait(lambda: active()==c)
        expect_footer(client,b"NORMAL")
        # A physical arrow from Help uses the same stay-in-Normal policy.
        client.send(b"?")
        wait(lambda: b"Shortcut Help" in body())
        focus(b"\x1b[C",d)
        wait(lambda: b"Shortcut Help" not in body())
        client.send(b"\x1b")
        expect_footer(client,b"LOCKED")
        generation=tomllib.loads(run("show-config"))["generation"]
        config.write_text(CONFIG.replace('right={actions=["focus-right"]',
            'right={actions=["focus-left",{action="switch-mode",mode="locked"}]'))
        wait(lambda: tomllib.loads(run("show-config"))["generation"]>generation)
        focus(b"\x02\x1b[C",c,b"LOCKED")
        # A child handshake proves all earlier local UI input stayed out of its PTY.
        client.send(b"CHILD_MARK")
        wait(lambda: json.loads((root/'c.json').read_text())['input']==b'CHILD_MARK'.hex())
        assert {p['id']:p['pid'] for p in panes()}==original
        for label in 'abd':
            assert json.loads((root/(label+'.json')).read_text())['input']=='',label
        client.send(b"\x02Sd")
        client.finish(0)
        run("kill")
    finally:
        if client: client.close()
        subprocess.run([BINARY,"kill",name],env=env,capture_output=True,timeout=8)

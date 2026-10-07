"""A session-wide floating shell owns a live PTY outside the tiled window order."""
import base64
import fcntl
import os
from pathlib import Path
import shlex
import struct
import subprocess
import tempfile
import termios
import time
import tomllib

from terminal_loop_support import BINARY, Session, expect_footer

with tempfile.TemporaryDirectory(prefix='rustmux-floating-') as temporary:
    root = Path(temporary)
    config = root / 'config.toml'
    config.write_text('''save_scrollback=true
save_scrollback_colors=true
[keybinds.normal]
i={actions=["toggle-floating-terminal",{action="switch-mode",mode="locked"}],display="help"}
"Ctrl p"={actions=[{action="switch-mode",mode="pane"}]}
[keybinds.pane]
w={actions=["toggle-floating-terminal",{action="switch-mode",mode="locked"}],display="help"}
''')
    env = dict(os.environ, XDG_CONFIG_HOME=str(root), XDG_STATE_HOME=str(root / 'state'),
               RUSTMUX_SHELL='/bin/sh', PS1='RUSTMUX_READY> ', ENV='', BASH_ENV='')
    name = f'floating-{os.getpid()}'
    working = root / 'cwd'
    working.mkdir()
    saved = root / 'state/rustmux/main-human/sessions' / f'{name}.toml'
    client = None

    def run(action, *args, success=True):
        target = [name] if action in ['kill', 'save-session'] else ['-s', name]
        result = subprocess.run([BINARY, action, *target, *map(str, args)], env=env,
                                capture_output=True, text=True, timeout=8)
        assert (result.returncode == 0) == success, (action, args, result)
        if not success:
            assert 'unexpected argument' not in result.stderr, result
            assert 'unrecognized subcommand' not in result.stderr, result
        return result.stdout

    def panes():
        return tomllib.loads(run('list-panes', '--toml'))['panes']

    def wait(predicate):
        deadline = time.monotonic() + 8
        while not predicate():
            if client:
                client.read(0.01)
            assert time.monotonic() < deadline, (panes(), client.physical_rows if client else None)
            time.sleep(0.01)

    def body():
        return b'\n'.join(client.physical_rows)

    def popup():
        return next((p for p in panes() if p['floating']), None)

    def output(pane):
        chunk = tomllib.loads(run('read-pane-output', '-p', pane, '--after', 0))
        return base64.b64decode(chunk['bytes_base64'])

    def toggle():
        client.send(b'\x02i')

    def attach(action='new', *args):
        return Session(extra_env=env, arguments=(action, name, '--config', str(config), *args), lifetime=100)

    def detach():
        client.send(b'\x02d')
        client.finish(0)
        client.close()

    try:
        client=attach();client.expect(b'RUSTMUX_READY>')
        base=panes()[0]
        client.send(("stty -echo; KEEP=base; cd "+shlex.quote(str(working))+"; printf '\\033]7;file://localhost%s\\007BASE_%s\\n' \"$PWD\" READY\n").encode())
        client.expect(b'BASE_READY')
        toggle()
        wait(lambda:popup() is not None and popup()['active'])
        first=popup()
        wait(lambda:b'floating' in body())
        assert b'BASE_READY' in body() and b'2 floating' not in client.physical_rows[0]
        client.send(b"stty -echo; KEEP=popup; printf 'FLOAT_CWD:%s\\nFLOAT_SIZE:%s\\n' \"${PWD##*/}\" \"$(stty size)\"\n")
        wait(lambda:b'FLOAT_CWD:cwd' in body() and b'FLOAT_SIZE:13 58' in body())
        assert Path(popup()['directory']).resolve()==working.resolve()
        # Mouse coordinates are relative to the centered child, never the backdrop.
        spy=root/'mouse.py';recorded=root/'mouse.bin'
        spy.write_text('''import os,sys,termios,tty
from pathlib import Path
original=termios.tcgetattr(0)
try:
    tty.setraw(0)
    os.write(1,b"\\x1b[?1000h\\x1b[?1006hMOUSE_READY")
    data=bytearray()
    while not data.endswith(b'm'):
        data.extend(os.read(0,1))
    Path(sys.argv[1]).write_bytes(data)
finally:
    os.write(1,b"\\x1b[?1000l\\x1b[?1006l")
    termios.tcsetattr(0,termios.TCSANOW,original)
os.write(1,b"\\r\\nMOUSE_DONE\\r\\n")
''')
        client.send(f'python3 {shlex.quote(str(spy))} {shlex.quote(str(recorded))}\n'.encode())
        client.expect(b'MOUSE_READY')
        client.send(b'\x1b[<0;14;8M\x1b[<0;14;8m')
        client.expect(b'MOUSE_DONE')
        assert recorded.read_bytes()==b'\x1b[<0;3;3M\x1b[<0;3;3m'
        # An unsafe split is ignored; ordinary panes and the same popup survive.
        client.send(b'\x02%')
        expect_footer(client,b'LOCKED')
        assert len(panes())==2 and popup()['pid']==first['pid']
        run('split-pane',success=False)
        run('break-pane','-p',first['id'],success=False)
        run('join-pane','-p',first['id'],'--to-pane',base['id'],success=False)
        run('join-pane','-p',base['id'],'--to-pane',first['id'],success=False)
        assert popup()['active'] and len(panes())==2
        toggle();wait(lambda:not popup()['active'])
        run('send-keys','-p',first['id'],'--literal','--enter',"printf 'HIDDEN_%s\\n' LIVE")
        wait(lambda: b'HIDDEN_LIVE' in output(first['id']))
        assert b'HIDDEN_LIVE' not in body()
        client.send(b'\x02\x10w')  # Configured Pane action reuses the same shell.
        wait(lambda:popup()['active'])
        wait(lambda:b'HIDDEN_LIVE' in body())
        client.send(b"printf 'KEEP_%s\\n' \"$KEEP\"\n")
        client.expect(b'KEEP_popup')
        assert popup()['pid']==first['pid']
        # Background tiled output appears outside the overlay.
        run('send-keys','-p',base['id'],'--literal','--enter',"printf '\\033[HBACK_%s\\n' LIVE")
        wait(lambda:b'BACK_LIVE' in body())
        fcntl.ioctl(client.slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,100,0,0))
        client.send(b"printf 'RESIZED_%s\\n' \"$(stty size)\"\n")
        client.expect(b'RESIZED_17 73')
        run('save-session')
        snapshot=tomllib.loads(saved.read_text())
        assert len(snapshot['windows'])==1 and snapshot['floating']['visible']
        assert len(snapshot['floating']['window']['panes'])==1
        assert 'HIDDEN_LIVE' in saved.read_text()
        # Detach/reconnect retains floating visibility, size and process identity.
        detach();client=attach('attach')
        wait(lambda:b'floating' in body())
        assert popup()['pid']==first['pid'] and popup()['active']
        toggle();wait(lambda:next(p for p in panes() if not p['floating'])['active'])
        assert next(p for p in panes() if not p['floating'])['pid']==base['pid']
        # Reload resizes the hidden popup without replacing it or the base shell.
        config.write_text('compact=true\n'+config.read_text())
        wait(lambda:tomllib.loads(run('show-config'))['settings']['compact'])
        toggle();wait(lambda:popup()['active'])
        client.send(b"printf 'COMPACT_%s\\n' \"$(stty size)\"\n")
        client.expect(b'COMPACT_14 58')
        assert popup()['pid']==first['pid']
        config.write_text(config.read_text().replace('compact=true\n','compact=false\n',1))
        wait(lambda:not tomllib.loads(run('show-config'))['settings']['compact'])
        toggle();wait(lambda:not popup()['active'])
        # Popup stays outside ordinary numbering; switching windows hides it.
        run('new-window','--name','second')
        assert len([p for p in panes() if not p['floating']])==2
        toggle();wait(lambda:popup()['active'])
        client.send(b'\x02p');wait(lambda:next(p for p in panes() if p['window']==1)['active'])
        assert not popup()['active'] and popup()['pid']==first['pid']
        run('save-session')
        assert not tomllib.loads(saved.read_text())['floating']['visible']
        detach();client=None
        run('kill')
        # Restored shells get fresh PIDs and retained history, including a hidden popup.
        client=attach('new')
        client.expect(b'RUSTMUX_READY>')
        assert popup() is not None and not popup()['active'] and popup()['pid']!=first['pid']
        toggle();wait(lambda:popup()['active'])
        client.send(b'\x02[')
        expect_footer(client,b'HISTORY')
        client.send(b'g');wait(lambda:b'HIDDEN_LIVE' in body())
        client.send(b'q')
        expect_footer(client,b'LOCKED')
        # Closing the popup does not exit the only usable ordinary session.
        run('close-pane','-p',popup()['id'])
        wait(lambda:popup() is None)
        # Keep only one ordinary window: exiting its overlay must not exit the server.
        run('close-window','-w',2)
        toggle();wait(lambda:popup() is not None)
        client.send(b'exit 0\n');wait(lambda:popup() is None)
        assert len([p for p in panes() if not p['floating']])==1
        toggle();wait(lambda:popup() is not None and popup()['active'])
        run('save-session')
        detach();client=None;run('kill')
        # Visible saved overlays restore their input ownership over the saved base.
        client=attach('new');client.expect(b'RUSTMUX_READY>')
        assert popup()['active'] and len(panes())==2
        restored_pid=popup()['pid']
        fcntl.ioctl(client.slave,termios.TIOCSWINSZ,struct.pack('HHHH',1,1,0,0))
        wait(lambda:len(client.physical_rows)==1)
        client.send(b"stty -echo; printf 'SMALL_%s\\n' \"$(stty size)\"\n")
        wait(lambda: b'SMALL_1 1' in output(popup()['id']))
        assert popup()['active'] and popup()['pid']==restored_pid
        fcntl.ioctl(client.slave,termios.TIOCSWINSZ,struct.pack('HHHH',24,80,0,0))
        wait(lambda:len(client.physical_rows)==24)
        client.send(b"stty -echo; printf 'TINY_RETURN_%s\\n' \"$(stty size)\"\n")
        client.expect(b'TINY_RETURN_13 58')
        toggle();wait(lambda:not popup()['active'])
        client.send(b"stty -echo; printf 'BASE_AFTER_RESTORE\\n'\n")
        client.expect(b'BASE_AFTER_RESTORE')
        detach();client=None
    finally:
        if client: client.close()
        subprocess.run([BINARY,'kill',name],env=env,capture_output=True,timeout=8)

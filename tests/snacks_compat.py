"""Installed Neovim/Snacks image detection and rendering without overrides."""
import shutil
import os
import sys
import fcntl
import struct
import subprocess
import termios
import tempfile
import shlex
import json
import time
import select
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
from yazi_compat import wait_for, fixture_png, FIRST_COLORS, assert_fixture_pixels, decode_outer_rgba_overlay
from kitty_large_overlay_compat import decode_tiles

binary = sys.argv[1]
nvim = shutil.which("nvim")
assert nvim, "nvim is required for the optional Snacks smoke test"
plugin = Path(os.environ.get("RUSTMUX_COMPAT_SNACKS", str(Path.home()/".local/share/nvim/lazy/snacks.nvim")))
assert (plugin/"lua/snacks/image/terminal.lua").is_file(), "set RUSTMUX_COMPAT_SNACKS to an installed snacks.nvim checkout"
script = Path(__file__).with_suffix('.lua')
for mode in (sys.argv[2:] or ('default', 'remote', 'named', 'unsupported', 'missing-pixels')):
    with tempfile.TemporaryDirectory(prefix='rustmux-snacks-') as directory:
        root = Path(directory)
        image = root/'fixture.png'
        fixture_png(image, FIRST_COLORS)
        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, * ((0, 0) if mode == 'missing-pixels' else (960, 480))))
        os.set_blocking(master, False)
        env = dict(os.environ, TERM='xterm-kitty', COLORTERM='truecolor', RUSTMUX_SHELL='/bin/sh', PS1='SNACKS_PROBE_READY> ', ENV='', BASH_ENV='', XDG_CONFIG_HOME=str(root/'config'), XDG_STATE_HOME=str(root/'state'), NVIM_LOG_FILE=str(root/'nvim.log'), RUSTMUX_COMPAT_SNACKS=str(plugin))
        for key in ('TMUX','ZELLIJ','SSH_CLIENT','SSH_CONNECTION','SNACKS_KITTY','SNACKS_GHOSTTY','SNACKS_WEZTERM','SNACKS_ZELLIJ','SNACKS_TMUX','SNACKS_SSH','RUSTMUX'):
            env.pop(key, None)
        if mode == 'remote':
            env['SSH_CONNECTION'] = '127.0.0.1 1 127.0.0.1 2'
        def child_setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        session_name = root.name if mode == 'named' else None
        arguments = [binary, 'new', session_name] if session_name else [binary]
        process = subprocess.Popen(arguments, stdin=slave,stdout=slave,stderr=slave,env=env,preexec_fn=child_setup)
        output = bytearray()
        try:
            wait_for(master,output,lambda data:b'\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c' in data,process,8,'outer graphics probe')
            os.write(master, (b'' if mode == 'unsupported' else b'\x1b_Gi=31;OK\x1b\\') + b'\x1b[?1;2c')
            wait_for(master,output,lambda data:b'SNACKS_PROBE_READY>' in data,process,8,'shell')
            output.clear()
            command = f'PROBE_MODE={mode} PROBE_DIR={shlex.quote(str(root))} PROBE_IMAGE={shlex.quote(str(image))} '
            command += f'{shlex.quote(nvim)} -u {shlex.quote(str(script))} --noplugin -i NONE\n'
            os.write(master,command.encode())
            wait_for(master,output,lambda data:(root/'result.json').exists(),process,12,'Snacks result')
            end = time.monotonic()+0.4
            while time.monotonic()<end:
                if select.select([master],[],[],0.05)[0]:
                    output.extend(os.read(master,65536))
            result = json.loads((root/'result.json').read_text())
            tiles = decode_tiles(output)
            result['outer_tiles'] = len(tiles)
            result['outer_tile_keys'] = list(tiles[0]) if tiles else []
            result['placeholder_leak'] = '\U0010eeee'.encode() in output
            supported = mode in ('default', 'remote', 'named')
            assert result['supported'] == supported, result
            assert not result.get('error'), result
            if supported:
                assert result['terminal']['terminal'] == 'rustmux-kitty', result
                assert result['env']['placeholders'], result
                assert any(r.get('U') == 1 for r in result['requests']), result
                assert any(r.get('t') == ('d' if mode == 'remote' else 'f') for r in result['requests']), result
                assert tiles, (result, bytes(output[-1000:]))
                _, width, height, pixels = decode_outer_rgba_overlay(output)
                assert_fixture_pixels(width, height, pixels, FIRST_COLORS)
            else:
                assert result['terminal']['terminal'].startswith('rustmux('), result
                assert not result['requests'] and not tiles, result
            assert not result['placeholder_leak'], result
            print(f"Snacks {mode}: supported={supported}, tiles={len(tiles)}", flush=True)
            os.write(master,b':qa!\n')
        finally:
            os.close(master)
            os.close(slave)
            if session_name:
                subprocess.run([binary, 'kill', session_name], env=env, check=False,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)

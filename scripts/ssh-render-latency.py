#!/usr/bin/env python3
"""Manual loopback SSH experiment; temporary keys/server, no system configuration changes."""
import argparse
import asyncio
import contextlib
import fcntl
import getpass
import json
import math
import os
from pathlib import Path
import re
import selectors
import shlex
import shutil
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time
import tty

ROWS, COLS = 24, 120
CASES = ('single', 'status', 'scattered', 'scroll')
PROFILES = (('local', 0, 1_000_000), ('rtt', 150, 1_000_000),
            ('limited', 100, 32_000), ('congested', 100, 4_000))


def write_all(fd, data):
    while data:
        data = data[os.write(fd, data):]


def fixture(case):
    tty.setraw(0)
    initial = b'\x1b[2J'
    for row in range(1, ROWS):
        initial += ('\x1b[%d;1H' % (row + 1) + chr(97 + row % 26) * COLS).encode()
    write_all(1, initial + b'\x1b[1;1H#000000')
    pending = bytearray()
    while True:
        chunk = os.read(0, 4096)
        if not chunk:
            return
        pending.extend(chunk)
        while b'\n' in pending:
            line, _, tail = pending.partition(b'\n')
            pending[:] = tail
            tick = int(line)
            odd = tick % 2
            if case == 'single':
                output = '\x1b[12;60H' + ('X' if odd else 'l')
            elif case == 'status':
                output = '\x1b[24;5H' + ('NORMAL' if odd else 'xxxxxx')
                output += '\x1b[24;100H' + ('12:34' if odd else 'xxxxx')
            elif case == 'scattered':
                output = ''.join('\x1b[12;%dH%s' % (col, 'X' if odd else 'l')
                                 for col in range(1, COLS + 1, 4))
            else:
                output = ''.join('\x1b[%d;1H%s' % (row + 1, chr(97 + (row + odd) % 26) * COLS)
                                 for row in range(1, ROWS))
            write_all(1, (output + '\x1b[1;1H#%06d' % tick).encode())


def relay(binary, case):
    """Run Rustmux on a real PTY on the SSH-server side; forward its terminal bytes."""
    with tempfile.TemporaryDirectory(prefix='rustmux-latency-child-') as directory:
        shell = Path(directory) / 'fixture'
        shell.write_text('#!' + sys.executable + '\nimport runpy, sys\nsys.argv = ' +
                         repr([str(Path(__file__).resolve()), '--fixture', case]) +
                         '\nrunpy.run_path(sys.argv[0], run_name="__main__")\n')
        shell.chmod(0o700)
        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', ROWS, COLS, 0, 0))
        def setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        child = subprocess.Popen([binary], stdin=slave, stdout=slave, stderr=slave,
                                 env=dict(os.environ, RUSTMUX_SHELL=str(shell)), preexec_fn=setup)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(0, selectors.EVENT_READ)
                selector.register(master, selectors.EVENT_READ)
                while child.poll() is None:
                    for key, _ in selector.select(0.1):
                        data = os.read(key.fd, 65536)
                        if not data:
                            return
                        write_all(master if key.fd == 0 else 1, data)
        finally:
            # Release the PTY before waiting: macOS may wait for terminal drain
            # during child exit while this relay is no longer reading output.
            os.close(master)
            os.close(slave)
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()


class Display:
    """ASCII-only fixture decoder; record a tick only after a complete ANSI frame."""
    end = re.compile(rb'\x1b\[[0-9]+;[0-9]+H\x1b\[\?25[hl]')
    tokens = re.compile(rb'\x1b\[([0-?]*)([ -/]*)([@-~])|\x1b[=>]|([^\x1b]+)')

    def __init__(self):
        self.pending = bytearray()
        self.grid = [[' '] * COLS for _ in range(ROWS)]
        self.row = self.col = 0

    def feed(self, data):
        self.pending.extend(data)
        ticks = []
        while (end := self.end.search(self.pending)) is not None:
            frame = bytes(self.pending[:end.end()])
            del self.pending[:end.end()]
            for token in self.tokens.finditer(frame):
                params, intermediate, final, text = token.groups()
                if final == b'H' and not intermediate:
                    self.row, self.col = (int(n) - 1 for n in params.split(b';'))
                elif text:
                    for char in text:
                        if 32 <= char < 127 and 0 <= self.row < ROWS and self.col < COLS:
                            self.grid[self.row][self.col] = chr(char)
                            self.col += 1
            match = re.match(r'#(\d{6})', ''.join(self.grid[0]))
            if match:
                ticks.append(int(match.group(1)))
        if len(self.pending) > 16 * 1024 * 1024:
            raise RuntimeError('unrecognized/oversized terminal frame')
        return ticks


async def shaped_copy(reader, writer, delay, rate, counters, direction):
    # A FIFO with independent receive/send tasks models serialization followed by
    # propagation, not an extra RTT for every read. Backlog is bounded to 256 KiB.
    queue = asyncio.Queue(maxsize=64)
    async def receive():
        free = asyncio.get_running_loop().time()
        while True:
            data = await reader.read(4096)
            if not data:
                await queue.put(None)
                return
            now = asyncio.get_running_loop().time()
            free = max(free, now) + len(data) / rate
            counters[direction] += len(data)
            await queue.put((free + delay, data))
    async def send():
        while True:
            item = await queue.get()
            if item is None:
                if writer.can_write_eof():
                    writer.write_eof()
                return
            due, data = item
            await asyncio.sleep(max(0, due - asyncio.get_running_loop().time()))
            writer.write(data)
            await writer.drain()
    jobs = [asyncio.create_task(receive()), asyncio.create_task(send())]
    try:
        await asyncio.gather(*jobs)
    finally:
        for job in jobs:
            job.cancel()
        await asyncio.gather(*jobs, return_exceptions=True)


async def measure(args, directory, ssh_port, label, binary, case, profile):
    name, rtt, rate = profile
    counters = dict(up=0, down=0)
    connections = set()
    async def proxy(reader, writer):
        task = asyncio.current_task()
        connections.add(task)
        remote = None
        jobs = []
        try:
            other, remote = await asyncio.open_connection('127.0.0.1', ssh_port)
            jobs = [asyncio.create_task(shaped_copy(reader, remote, rtt / 2000, rate, counters, 'up')),
                    asyncio.create_task(shaped_copy(other, writer, rtt / 2000, rate, counters, 'down'))]
            await asyncio.gather(*jobs)
        except (ConnectionError, BrokenPipeError):
            pass
        finally:
            for job in jobs:
                job.cancel()
            await asyncio.gather(*jobs, return_exceptions=True)
            writer.close()
            if remote:
                remote.close()
            connections.discard(task)
    server = await asyncio.start_server(proxy, '127.0.0.1', 0)
    port = server.sockets[0].getsockname()[1]
    host_key = (directory / 'host.pub').read_text().split()
    known = directory / 'known_hosts'
    known.write_text('[127.0.0.1]:%d %s %s\n' % (port, host_key[0], host_key[1]))
    command = [args.ssh, '-F', '/dev/null', '-T', '-p', str(port), '-i', str(directory / 'client'),
               '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'StrictHostKeyChecking=yes',
               '-o', 'UserKnownHostsFile=' + str(known), '-o', 'Compression=no', '-o', 'LogLevel=ERROR',
               getpass.getuser() + '@127.0.0.1',
               shlex.join([sys.executable, str(Path(__file__).resolve()), '--relay', str(binary), case])]
    process = await asyncio.create_subprocess_exec(*command, stdin=asyncio.subprocess.PIPE,
                                                 stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    errors = asyncio.create_task(process.stderr.read())
    display = Display()
    sent, observed = {}, {}
    async def consume(until):
        while until not in observed:
            data = await process.stdout.read(65536)
            if not data:
                raise RuntimeError('SSH/probe exited: ' + (await errors).decode(errors='replace'))
            now = time.perf_counter()
            for tick in display.feed(data):
                observed.setdefault(tick, now)
    sender = None
    try:
        await asyncio.wait_for(consume(0), 30)
        baseline = counters.copy()
        async def send():
            start = time.perf_counter()
            for tick in range(1, args.samples + 1):
                await asyncio.sleep(max(0, start + (tick - 1) * args.interval_ms / 1000 - time.perf_counter()))
                sent[tick] = time.perf_counter()
                process.stdin.write(('%d\n' % tick).encode())
                await process.stdin.drain()
        sender = asyncio.create_task(send())
        # Allow both the input schedule and a conservative full-frame drain budget.
        timeout = args.samples * args.interval_ms / 1000 + max(45, args.samples * ROWS * COLS * 2 / rate)
        await asyncio.wait_for(consume(args.samples), timeout)
        await sender
        latencies = sorted((observed[tick] - moment) * 1000 for tick, moment in sent.items() if tick in observed)
        result = dict(revision=label, case=case, profile=name, rtt_ms=rtt, bytes_per_second=rate,
                      samples=len(latencies), coalesced=args.samples-len(latencies),
                      p50_ms=statistics.median(latencies), p95_ms=latencies[min(len(latencies)-1, math.ceil(len(latencies)*.95)-1)],
                      ssh_down_bytes=counters['down']-baseline['down'], ssh_up_bytes=counters['up']-baseline['up'])
        print(json.dumps(result), flush=True)
        return result
    finally:
        if sender:
            sender.cancel()
            await asyncio.gather(sender, return_exceptions=True)
        process.stdin.close()
        try:
            await asyncio.wait_for(process.wait(), 3)
        except asyncio.TimeoutError:
            process.kill()
            await process.wait()
        await errors
        server.close()
        await server.wait_closed()
        for task in list(connections):
            task.cancel()
        await asyncio.gather(*list(connections), return_exceptions=True)


async def run(args):
    with tempfile.TemporaryDirectory(prefix='rustmux-ssh-latency-') as temp:
        directory = Path(temp)
        for name in ('host', 'client'):
            subprocess.run([args.keygen, '-q', '-t', 'ed25519', '-N', '', '-f', str(directory/name)], check=True)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        config = directory/'sshd_config'
        config.write_text('ListenAddress 127.0.0.1\nPort %d\nHostKey %s/host\nPidFile %s/pid\n'
                          'AuthorizedKeysFile %s/client.pub\nStrictModes no\nPasswordAuthentication no\n'
                          'KbdInteractiveAuthentication no\nUsePAM no\nAllowUsers %s\n' %
                          (port, directory, directory, directory, getpass.getuser()))
        with (directory/'sshd.log').open('w+') as log:
            daemon = subprocess.Popen([args.sshd, '-D', '-e', '-f', str(config)], stdout=log, stderr=log)
            try:
                await asyncio.sleep(.15)
                if daemon.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(log.read())
                results = []
                for profile in PROFILES:
                    if args.profile and profile[0] != args.profile:
                        continue
                    for case in CASES:
                        for label, binary in [('row', args.row_binary), ('cell', args.cell_binary)]:
                            results.append(await measure(args, directory, port, label, binary, case, profile))
                Path(args.output).write_text(json.dumps(dict(samples=args.samples, interval_ms=args.interval_ms,
                    row_binary=str(args.row_binary), cell_binary=str(args.cell_binary), results=results), indent=2)+'\n')
            finally:
                daemon.terminate()
                try:
                    daemon.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait()


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == '--fixture':
        fixture(sys.argv[2])
    elif len(sys.argv) > 1 and sys.argv[1] == '--relay':
        relay(sys.argv[2], sys.argv[3])
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument('--row-binary', type=Path, required=True)
        parser.add_argument('--cell-binary', type=Path, required=True)
        parser.add_argument('--samples', type=int, default=12)
        parser.add_argument('--interval-ms', type=float, default=50)
        parser.add_argument('--profile', choices=[p[0] for p in PROFILES])
        parser.add_argument('--output', default='ssh-render-latency.json')
        parser.add_argument('--ssh', default=shutil.which('ssh'))
        parser.add_argument('--sshd', default=shutil.which('sshd'))
        parser.add_argument('--keygen', default=shutil.which('ssh-keygen'))
        args = parser.parse_args()
        if not 1 <= args.samples <= 1000 or not math.isfinite(args.interval_ms) or args.interval_ms <= 0:
            parser.error('samples must be 1..1000 and interval-ms must be positive')
        for name in ('row_binary', 'cell_binary'):
            path = getattr(args, name).resolve()
            if not path.is_file():
                parser.error('missing binary: ' + str(path))
            setattr(args, name, path)
        if not all((args.ssh, args.sshd, args.keygen)):
            parser.error('ssh, sshd and ssh-keygen are required')
        asyncio.run(run(args))

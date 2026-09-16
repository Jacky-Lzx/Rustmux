//! Local terminal bridge for one attached session client.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, poll};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};

use super::handshake::{self, ClientPeer};
use super::protocol::{ClientMessage, MAX_FRAME_BYTES, ServerMessage};
use crate::terminal_device::TerminalDevice;

const READ_BYTES: usize = 8192;
const POLL_TIMEOUT_MILLIS: u16 = 50;
const MAX_BUFFERED_OUTPUT_BYTES: usize = MAX_FRAME_BYTES + READ_BYTES;

/// Attach a connected session socket to the controlling terminal.
///
/// The terminal enters raw mode and the alternate screen only after the
/// handshake succeeds. All return paths restore its termios and display modes.
pub fn run(stream: UnixStream) -> io::Result<u8> {
    let file = TerminalDevice::open_controlling()?;
    let size = crate::terminal_device::window_size(&file)?;
    let peer = handshake::client(stream, size.ws_row, size.ws_col)?;
    let signals = ClientSignals::install()?;
    run_attached(file, peer, &signals)
}

fn run_attached(file: File, peer: ClientPeer, signals: &ClientSignals) -> io::Result<u8> {
    let mut terminal = TerminalDevice::enter(file)?;
    let result = bridge(&mut terminal, peer, signals);
    let restored = terminal.restore();
    match result {
        Err(error) => Err(error),
        Ok(status) => restored.map(|()| status),
    }
}

fn bridge(
    terminal: &mut TerminalDevice,
    mut peer: ClientPeer,
    signals: &ClientSignals,
) -> io::Result<u8> {
    let mut outbound = Outbound::default();
    let mut to_terminal = VecDeque::new();
    let mut pending_resize = signals.resize.swap(false, Ordering::Relaxed);
    let mut exit = None;

    apply_server_messages(peer.decode(&[])?, &mut to_terminal, &mut exit)?;
    loop {
        let signal = signals.pending.load(Ordering::Relaxed);
        if signal != 0 {
            return Ok((128 + signal) as u8);
        }
        pending_resize |= signals.resize.swap(false, Ordering::Relaxed);
        if pending_resize && outbound.is_empty() {
            let size = terminal.size()?;
            if size.ws_row != 0 && size.ws_col != 0 {
                outbound.set(ClientMessage::Resize {
                    rows: size.ws_row,
                    columns: size.ws_col,
                })?;
            }
            pending_resize = false;
        }
        if let Some(status) = exit
            && to_terminal.is_empty()
        {
            return exit_status(status);
        }

        let (terminal_ready, socket_ready) = {
            let mut terminal_flags = PollFlags::empty();
            if exit.is_none() && outbound.is_empty() && !pending_resize {
                terminal_flags |= PollFlags::POLLIN;
            }
            if !to_terminal.is_empty() {
                terminal_flags |= PollFlags::POLLOUT;
            }
            let mut socket_flags = PollFlags::empty();
            if exit.is_none() && to_terminal.is_empty() {
                socket_flags |= PollFlags::POLLIN;
            }
            if !outbound.is_empty() {
                socket_flags |= PollFlags::POLLOUT;
            }
            let mut fds = [
                PollFd::new(terminal.file().as_fd(), terminal_flags),
                PollFd::new(peer.stream().as_fd(), socket_flags),
            ];
            match poll(&mut fds, POLL_TIMEOUT_MILLIS) {
                Ok(_) | Err(Errno::EINTR) => {}
                Err(error) => return Err(error.into()),
            }
            (
                fds[0].revents().unwrap_or_else(PollFlags::empty),
                fds[1].revents().unwrap_or_else(PollFlags::empty),
            )
        };

        reject_invalid_fd(terminal_ready, "terminal")?;
        reject_invalid_fd(socket_ready, "session socket")?;
        if terminal_ready.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
            && exit.is_none()
            && outbound.is_empty()
        {
            let mut bytes = [0; READ_BYTES];
            match terminal.file_mut().read(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "terminal input ended",
                    ));
                }
                Ok(count) => outbound.set(ClientMessage::Input(bytes[..count].to_vec()))?,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        if socket_ready.contains(PollFlags::POLLOUT) {
            outbound.write_to(peer.stream_mut())?;
        }
        if socket_ready.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
            && exit.is_none()
            && to_terminal.is_empty()
        {
            let mut bytes = [0; READ_BYTES];
            match peer.stream_mut().read(&mut bytes) {
                Ok(0) => {
                    peer.finish()?;
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "session server disconnected without an exit status",
                    ));
                }
                Ok(count) => apply_server_messages(
                    peer.decode(&bytes[..count])?,
                    &mut to_terminal,
                    &mut exit,
                )?,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        if terminal_ready.contains(PollFlags::POLLOUT) {
            send_terminal(terminal.file_mut(), &mut to_terminal)?;
        }
    }
}

fn apply_server_messages(
    messages: Vec<ServerMessage>,
    output: &mut VecDeque<u8>,
    exit: &mut Option<i32>,
) -> io::Result<()> {
    for message in messages {
        if exit.is_some() {
            return Err(invalid_data("server sent a message after Exit"));
        }
        match message {
            ServerMessage::Output(bytes) => {
                if bytes.len() > MAX_BUFFERED_OUTPUT_BYTES - output.len() {
                    return Err(invalid_data("session output buffer exceeded its limit"));
                }
                output.extend(bytes);
            }
            ServerMessage::Exit { status } => *exit = Some(status),
            ServerMessage::Rejected(message) => {
                return Err(io::Error::new(io::ErrorKind::ConnectionRefused, message));
            }
            ServerMessage::Attached { .. } => {
                return Err(invalid_data("server sent a second Attached message"));
            }
        }
    }
    Ok(())
}

fn send_terminal(writer: &mut impl Write, pending: &mut VecDeque<u8>) -> io::Result<()> {
    let bytes = pending.as_slices().0;
    if bytes.is_empty() {
        return Ok(());
    }
    match writer.write(bytes) {
        Ok(0) => Err(io::ErrorKind::WriteZero.into()),
        Ok(count) => {
            pending.drain(..count);
            Ok(())
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn reject_invalid_fd(events: PollFlags, name: &str) -> io::Result<()> {
    if events.contains(PollFlags::POLLNVAL) {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            format!("invalid {name} descriptor"),
        ))
    } else {
        Ok(())
    }
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn exit_status(status: i32) -> io::Result<u8> {
    u8::try_from(status)
        .map_err(|_| invalid_data(format!("session exit status {status} is outside 0..=255")))
}

#[derive(Debug, Default)]
struct Outbound {
    bytes: Vec<u8>,
    written: usize,
}

impl Outbound {
    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn set(&mut self, message: ClientMessage) -> io::Result<()> {
        debug_assert!(self.is_empty());
        self.bytes = message
            .encode()
            .map_err(|error| invalid_data(error.to_string()))?;
        self.written = 0;
        Ok(())
    }

    fn write_to(&mut self, writer: &mut impl Write) -> io::Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        match writer.write(&self.bytes[self.written..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => self.written += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        if self.written == self.bytes.len() {
            self.bytes.clear();
            self.written = 0;
        }
        Ok(())
    }
}

struct ClientSignals {
    pending: Arc<AtomicUsize>,
    resize: Arc<AtomicBool>,
    ids: Vec<signal_hook::SigId>,
}

impl ClientSignals {
    fn install() -> io::Result<Self> {
        let mut signals = Self {
            pending: Arc::new(AtomicUsize::new(0)),
            // Re-read after installing the handler to cover changes since handshake.
            resize: Arc::new(AtomicBool::new(true)),
            ids: Vec::new(),
        };
        for signal in [SIGHUP, SIGTERM, SIGINT, SIGQUIT] {
            signals.ids.push(signal_hook::flag::register_usize(
                signal,
                signals.pending.clone(),
                signal as usize,
            )?);
        }
        signals.ids.push(signal_hook::flag::register(
            SIGWINCH,
            signals.resize.clone(),
        )?);
        Ok(signals)
    }
}

impl Drop for ClientSignals {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::pty::{Winsize, openpty};
    use nix::sys::termios::{self, LocalFlags};
    use std::thread;

    #[test]
    fn bridges_input_output_resize_and_exit_then_restores_terminal() {
        let size = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pair = openpty(Some(&size), None).unwrap();
        let mut master: File = pair.master.into();
        let slave: File = pair.slave.into();
        let observer = slave.try_clone().unwrap();
        let mut original = termios::tcgetattr(&observer).unwrap();
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut peer = handshake::server(server_stream).unwrap();
            assert_eq!(peer.size(), (24, 80));
            let mut saw_resize = false;
            loop {
                let mut bytes = [0; READ_BYTES];
                match peer.stream_mut().read(&mut bytes) {
                    Ok(count) => {
                        for message in peer.decode(&bytes[..count]).unwrap() {
                            match message {
                                ClientMessage::Resize { rows, columns } => {
                                    assert_eq!((rows, columns), (24, 80));
                                    saw_resize = true;
                                }
                                ClientMessage::Input(bytes) => {
                                    assert!(saw_resize);
                                    assert_eq!(bytes, b"input");
                                    let reply = [
                                        ServerMessage::Output(b"output".to_vec()).encode().unwrap(),
                                        ServerMessage::Exit { status: 7 }.encode().unwrap(),
                                    ]
                                    .concat();
                                    peer.stream_mut().write_all(&reply).unwrap();
                                    return;
                                }
                                other => panic!("unexpected message: {other:?}"),
                            }
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::yield_now(),
                    Err(error) => panic!("server read failed: {error}"),
                }
            }
        });
        let peer = handshake::client(client_stream, 24, 80).unwrap();
        let signals = ClientSignals {
            pending: Arc::new(AtomicUsize::new(0)),
            resize: Arc::new(AtomicBool::new(true)),
            ids: Vec::new(),
        };
        let client = thread::spawn(move || run_attached(slave, peer, &signals).unwrap());

        while termios::tcgetattr(&observer)
            .unwrap()
            .local_flags
            .contains(LocalFlags::ICANON)
        {
            thread::yield_now();
        }
        master.write_all(b"input").unwrap();
        assert_eq!(client.join().unwrap(), 7);
        server.join().unwrap();

        let mut restored = termios::tcgetattr(&observer).unwrap();
        original.local_flags.remove(LocalFlags::PENDIN);
        restored.local_flags.remove(LocalFlags::PENDIN);
        assert_eq!(restored.input_flags, original.input_flags);
        assert_eq!(restored.output_flags, original.output_flags);
        assert_eq!(restored.control_flags, original.control_flags);
        assert_eq!(restored.local_flags, original.local_flags);
        assert_eq!(restored.control_chars, original.control_chars);

        let mut rendered = vec![0; 1024];
        let count = master.read(&mut rendered).unwrap();
        rendered.truncate(count);
        assert!(
            rendered
                .windows(b"output".len())
                .any(|bytes| bytes == b"output"),
            "rendered bytes: {rendered:?}"
        );
    }

    #[test]
    fn rejects_messages_after_exit_and_invalid_statuses() {
        let mut output = VecDeque::new();
        let mut exit = None;
        assert!(
            apply_server_messages(
                vec![
                    ServerMessage::Exit { status: 0 },
                    ServerMessage::Output(vec![1]),
                ],
                &mut output,
                &mut exit,
            )
            .is_err()
        );
        assert_eq!(exit_status(255).unwrap(), 255);
        assert_eq!(
            exit_status(-1).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            exit_status(256).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

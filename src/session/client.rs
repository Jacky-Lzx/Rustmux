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
use super::{SessionName, record_connection};
use crate::terminal_device::TerminalDevice;

const READ_BYTES: usize = 8192;
const POLL_TIMEOUT_MILLIS: u16 = 50;
const MAX_BUFFERED_OUTPUT_BYTES: usize = MAX_FRAME_BYTES + READ_BYTES;

/// Attach a connected session socket to the controlling terminal.
///
/// The terminal enters raw mode and the alternate screen only after the
/// handshake succeeds. All return paths restore its termios and display modes.
pub(crate) fn run(stream: UnixStream, name: &SessionName) -> io::Result<ClientExit> {
    let file = TerminalDevice::open_controlling()?;
    let size = crate::terminal_device::window_size(&file)?;
    let peer = handshake::client(stream, size.ws_row, size.ws_col)?;
    record_connection(name)?;
    let signals = ClientSignals::install()?;
    run_attached(file, peer, &signals)
}

fn run_attached(file: File, peer: ClientPeer, signals: &ClientSignals) -> io::Result<ClientExit> {
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
) -> io::Result<ClientExit> {
    let mut outbound = Outbound::default();
    let mut to_terminal = VecDeque::new();
    let mut pending_resize = signals.resize.swap(false, Ordering::Relaxed);
    let mut exit = None;
    let mut server_control = None;
    let mut input = ClientInput::with_prefix(peer.locked_entry_key());
    let mut client_exit = None;

    apply_server_messages(
        peer.decode(&[])?,
        &mut to_terminal,
        &mut exit,
        &mut server_control,
    )?;
    loop {
        let signal = signals.pending.load(Ordering::Relaxed);
        if signal != 0 {
            return Ok(ClientExit::Process((128 + signal) as u8));
        }
        pending_resize |= signals.resize.swap(false, Ordering::Relaxed);
        if pending_resize && outbound.is_empty() && client_exit.is_none() {
            let size = terminal.size()?;
            if size.ws_row != 0 && size.ws_col != 0 {
                outbound.push(ClientMessage::Resize {
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
        if let Some(control) = server_control
            && to_terminal.is_empty()
        {
            return Ok(control);
        }
        if let Some(client_exit) = client_exit
            && outbound.is_empty()
        {
            return Ok(client_exit);
        }

        let (terminal_ready, socket_ready) = {
            let mut terminal_flags = PollFlags::empty();
            if exit.is_none()
                && server_control.is_none()
                && client_exit.is_none()
                && outbound.is_empty()
                && !pending_resize
            {
                terminal_flags |= PollFlags::POLLIN;
            }
            if !to_terminal.is_empty() {
                terminal_flags |= PollFlags::POLLOUT;
            }
            let mut socket_flags = PollFlags::empty();
            if exit.is_none()
                && server_control.is_none()
                && client_exit.is_none()
                && to_terminal.is_empty()
            {
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
            && client_exit.is_none()
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
                Ok(count) => {
                    let mut forwarded = Vec::with_capacity(count);
                    client_exit = input.feed(&bytes[..count], &mut forwarded);
                    if !forwarded.is_empty() {
                        outbound.push(ClientMessage::Input(forwarded))?;
                    }
                    if client_exit.is_some() {
                        outbound.push(ClientMessage::Detach)?;
                    }
                }
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
            && client_exit.is_none()
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
                    &mut server_control,
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
    server_control: &mut Option<ClientExit>,
) -> io::Result<()> {
    for message in messages {
        if exit.is_some() || server_control.is_some() {
            return Err(invalid_data("server sent a message after terminal control"));
        }
        match message {
            ServerMessage::Output(bytes) => {
                if bytes.len() > MAX_BUFFERED_OUTPUT_BYTES - output.len() {
                    return Err(invalid_data("session output buffer exceeded its limit"));
                }
                output.extend(bytes);
            }
            ServerMessage::Exit { status } => *exit = Some(status),
            ServerMessage::OpenSessionManager => *server_control = Some(ClientExit::SessionManager),
            ServerMessage::Detach => *server_control = Some(ClientExit::Detached),
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

fn exit_status(status: i32) -> io::Result<ClientExit> {
    u8::try_from(status)
        .map(ClientExit::Process)
        .map_err(|_| invalid_data(format!("session exit status {status} is outside 0..=255")))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClientExit {
    Process(u8),
    Detached,
    SessionManager,
}

#[derive(Debug, Default)]
struct Outbound {
    frames: VecDeque<Vec<u8>>,
    written: usize,
}

impl Outbound {
    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn push(&mut self, message: ClientMessage) -> io::Result<()> {
        self.frames.push_back(
            message
                .encode()
                .map_err(|error| invalid_data(error.to_string()))?,
        );
        Ok(())
    }

    fn write_to(&mut self, writer: &mut impl Write) -> io::Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        let frame = self.frames.front().expect("nonempty outbound queue");
        match writer.write(&frame[self.written..]) {
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
        if self.written == frame.len() {
            self.frames.pop_front();
            self.written = 0;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ClientInput {
    locked_enter: u8,
    prefix: bool,
    paste: bool,
    tail: VecDeque<u8>,
}

impl Default for ClientInput {
    fn default() -> Self {
        Self::with_prefix(2)
    }
}

impl ClientInput {
    fn with_prefix(locked_enter: u8) -> Self {
        Self {
            locked_enter,
            prefix: false,
            paste: false,
            tail: VecDeque::new(),
        }
    }

    fn feed(&mut self, bytes: &[u8], forwarded: &mut Vec<u8>) -> Option<ClientExit> {
        for &byte in bytes {
            self.tail.push_back(byte);
            if self.tail.len() > 6 {
                self.tail.pop_front();
            }
            let was_paste = self.paste;
            if self.tail.iter().copied().eq(b"\x1b[200~".iter().copied()) {
                self.paste = true;
            }
            if self.tail.iter().copied().eq(b"\x1b[201~".iter().copied()) {
                self.paste = false;
            }
            if was_paste {
                forwarded.push(byte);
            } else if self.prefix {
                self.prefix = false;
                if byte == b'd' {
                    return Some(ClientExit::Detached);
                }
                if byte == 23 {
                    return Some(ClientExit::SessionManager);
                }
                forwarded.push(byte);
            } else if byte == self.locked_enter {
                self.prefix = true;
                // Forward the prefix immediately so the session server can show
                // NORMAL mode while this client waits for the command byte.
                forwarded.push(byte);
            } else {
                forwarded.push(byte);
            }
        }
        None
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
        assert_eq!(client.join().unwrap(), ClientExit::Process(7));
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
    fn server_session_manager_control_flushes_output_and_restores_terminal() {
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
        let (handshake_done_tx, handshake_done_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let mut peer = handshake::server(server_stream).unwrap();
            let reply = [
                ServerMessage::Output(b"before-manager".to_vec())
                    .encode()
                    .unwrap(),
                ServerMessage::OpenSessionManager.encode().unwrap(),
            ]
            .concat();
            peer.stream_mut().write_all(&reply).unwrap();
            handshake_done_rx.recv().unwrap();
        });
        let peer = handshake::client(client_stream, 24, 80).unwrap();
        handshake_done_tx.send(()).unwrap();
        let signals = ClientSignals {
            pending: Arc::new(AtomicUsize::new(0)),
            resize: Arc::new(AtomicBool::new(false)),
            ids: Vec::new(),
        };
        let client = thread::spawn(move || run_attached(slave, peer, &signals).unwrap());

        assert_eq!(client.join().unwrap(), ClientExit::SessionManager);
        server.join().unwrap();
        let mut restored = termios::tcgetattr(&observer).unwrap();
        original.local_flags.remove(LocalFlags::PENDIN);
        restored.local_flags.remove(LocalFlags::PENDIN);
        assert_eq!(restored.local_flags, original.local_flags);

        let mut rendered = vec![0; 1024];
        let count = master.read(&mut rendered).unwrap();
        rendered.truncate(count);
        assert!(
            rendered
                .windows(b"before-manager".len())
                .any(|bytes| bytes == b"before-manager"),
            "rendered bytes: {rendered:?}"
        );
    }

    #[test]
    fn rejects_messages_after_exit_and_invalid_statuses() {
        let mut output = VecDeque::new();
        let mut exit = None;
        let mut server_control = None;
        assert!(
            apply_server_messages(
                vec![
                    ServerMessage::Exit { status: 0 },
                    ServerMessage::Output(vec![1]),
                ],
                &mut output,
                &mut exit,
                &mut server_control,
            )
            .is_err()
        );
        assert_eq!(exit_status(255).unwrap(), ClientExit::Process(255));
        assert_eq!(
            exit_status(-1).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            exit_status(256).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn session_manager_control_preserves_prior_output_and_is_terminal() {
        let mut output = VecDeque::new();
        let mut exit = None;
        let mut server_control = None;
        apply_server_messages(
            vec![
                ServerMessage::Output(b"last frame".to_vec()),
                ServerMessage::OpenSessionManager,
            ],
            &mut output,
            &mut exit,
            &mut server_control,
        )
        .unwrap();
        assert_eq!(output, b"last frame".to_vec());
        assert_eq!(server_control, Some(ClientExit::SessionManager));
        assert!(
            apply_server_messages(
                vec![ServerMessage::Output(vec![1])],
                &mut output,
                &mut exit,
                &mut server_control,
            )
            .is_err()
        );
    }

    #[test]
    fn server_detach_control_preserves_prior_output_and_is_terminal() {
        let mut output = VecDeque::new();
        let mut exit = None;
        let mut server_control = None;
        apply_server_messages(
            vec![
                ServerMessage::Output(b"last frame".to_vec()),
                ServerMessage::Detach,
            ],
            &mut output,
            &mut exit,
            &mut server_control,
        )
        .unwrap();
        assert_eq!(output, b"last frame".to_vec());
        assert_eq!(server_control, Some(ClientExit::Detached));
        assert!(
            apply_server_messages(
                vec![ServerMessage::Output(vec![1])],
                &mut output,
                &mut exit,
                &mut server_control,
            )
            .is_err()
        );
    }

    #[test]
    fn detach_shortcut_preserves_prior_input_and_ignores_paste_contents() {
        let mut input = ClientInput::default();
        let mut forwarded = Vec::new();
        assert_eq!(input.feed(b"before\x02", &mut forwarded), None);
        assert_eq!(
            input.feed(b"dafter", &mut forwarded),
            Some(ClientExit::Detached)
        );
        assert_eq!(forwarded, b"before\x02");

        let mut input = ClientInput::default();
        let mut forwarded = Vec::new();
        assert_eq!(
            input.feed(b"\x1b[200~paste\x02d\x1b[201~", &mut forwarded),
            None
        );
        assert_eq!(forwarded, b"\x1b[200~paste\x02d\x1b[201~");
        assert_eq!(
            input.feed(b"\x02d", &mut forwarded),
            Some(ClientExit::Detached)
        );
        assert_eq!(forwarded.last(), Some(&2));
    }

    #[test]
    fn session_manager_shortcut_is_local_and_ignored_in_paste_contents() {
        let mut input = ClientInput::default();
        let mut forwarded = Vec::new();
        assert_eq!(
            input.feed(b"before\x02\x17after", &mut forwarded),
            Some(ClientExit::SessionManager)
        );
        assert_eq!(forwarded, b"before\x02");

        let mut input = ClientInput::default();
        let mut forwarded = Vec::new();
        assert_eq!(
            input.feed(b"\x1b[200~paste\x02\x17\x1b[201~", &mut forwarded),
            None
        );
        assert_eq!(forwarded, b"\x1b[200~paste\x02\x17\x1b[201~");
    }

    #[test]
    fn non_detach_prefixes_are_forwarded_for_the_server_parser() {
        let mut input = ClientInput::default();
        let mut forwarded = Vec::new();
        assert_eq!(input.feed(b"\x02", &mut forwarded), None);
        assert_eq!(forwarded, b"\x02");
        assert_eq!(input.feed(b"c\x02\x02", &mut forwarded), None);
        assert_eq!(forwarded, b"\x02c\x02\x02");
    }

    #[test]
    fn remapped_prefix_controls_client_shortcuts_and_preserves_paste() {
        let mut input = ClientInput::with_prefix(1);
        let mut forwarded = Vec::new();
        assert_eq!(input.feed(b"\x02d", &mut forwarded), None);
        assert_eq!(forwarded, b"\x02d");
        assert_eq!(input.feed(b"\x1b[200~\x01d\x1b[201~", &mut forwarded), None);
        assert_eq!(
            input.feed(b"\x01\x17", &mut forwarded),
            Some(ClientExit::SessionManager)
        );
        assert!(forwarded.ends_with(b"\x1b[200~\x01d\x1b[201~\x01"));

        let mut input = ClientInput::with_prefix(1);
        let mut forwarded = Vec::new();
        assert_eq!(
            input.feed(b"\x01d", &mut forwarded),
            Some(ClientExit::Detached)
        );
        assert_eq!(forwarded, b"\x01");
    }
}

//! Nonblocking server-side adapter between the session protocol and terminal I/O.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::time::Duration;

use nix::pty::Winsize;

use super::handshake::ServerPeer;
use super::protocol::{ClientMessage, MAX_FRAME_BYTES, ServerMessage};

const READ_BYTES: usize = 8192;
const MAX_BUFFERED_INPUT_BYTES: usize = 2 * MAX_FRAME_BYTES;
const EXIT_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Result of servicing the client side of an attached session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Attached,
    Detached,
    Disconnected,
}

/// Converts framed client messages into terminal input and framed server output.
#[derive(Debug)]
pub struct ServerFrontend {
    peer: ServerPeer,
    input: VecDeque<u8>,
    resize: Option<Winsize>,
    state: ConnectionState,
    outbound: Outbound,
}

impl ServerFrontend {
    pub fn new(peer: ServerPeer) -> Self {
        let (rows, columns) = peer.size();
        Self {
            peer,
            input: VecDeque::new(),
            resize: Some(winsize(rows, columns)),
            state: ConnectionState::Attached,
            outbound: Outbound::default(),
        }
    }

    pub fn poll_fd(&self) -> BorrowedFd<'_> {
        self.peer.stream().as_fd()
    }

    pub fn state(&self) -> ConnectionState {
        self.state
    }

    pub fn buffered_input_len(&self) -> usize {
        self.input.len()
    }

    /// Move at most `limit` total bytes into the event loop's input queue.
    pub fn drain_input(&mut self, pending: &mut VecDeque<u8>, limit: usize) -> usize {
        let count = self.input.len().min(limit.saturating_sub(pending.len()));
        pending.extend(self.input.drain(..count));
        count
    }

    /// Return the newest size received since the previous call.
    pub fn take_resize(&mut self) -> Option<Winsize> {
        self.resize.take()
    }

    /// Read and decode at most one bounded socket chunk.
    pub fn receive(&mut self) -> io::Result<ConnectionState> {
        if self.state != ConnectionState::Attached || self.input.len() >= MAX_FRAME_BYTES {
            return Ok(self.state);
        }

        let retained = self.peer.decode(&[])?;
        if !retained.is_empty() {
            self.apply(retained)?;
            return Ok(self.state);
        }

        let mut bytes = [0; READ_BYTES];
        let messages = match self.peer.stream_mut().read(&mut bytes) {
            Ok(0) => {
                self.peer.finish()?;
                self.state = ConnectionState::Disconnected;
                return Ok(self.state);
            }
            Ok(count) => self.peer.decode(&bytes[..count])?,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                return Ok(self.state);
            }
            Err(error) => return Err(error),
        };

        self.apply(messages)?;
        Ok(self.state)
    }

    fn apply(&mut self, messages: Vec<ClientMessage>) -> io::Result<()> {
        for message in messages {
            if self.state != ConnectionState::Attached {
                return Err(invalid_data("client sent a message after Detach"));
            }
            match message {
                ClientMessage::Input(bytes) => {
                    if bytes.len() > MAX_BUFFERED_INPUT_BYTES - self.input.len() {
                        return Err(invalid_data("session input buffer exceeded its limit"));
                    }
                    self.input.extend(bytes);
                }
                ClientMessage::Resize { rows, columns } => {
                    self.resize = Some(winsize(rows, columns));
                }
                ClientMessage::Detach => self.state = ConnectionState::Detached,
                ClientMessage::Hello { .. } => {
                    return Err(invalid_data("client sent a second Hello message"));
                }
            }
        }
        Ok(())
    }

    /// Write at most once, retaining source bytes until their whole protocol frame is sent.
    pub fn send_output(&mut self, pending: &mut VecDeque<u8>) -> io::Result<()> {
        if self.state != ConnectionState::Attached {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "session client is no longer attached",
            ));
        }
        self.outbound.write_to(self.peer.stream_mut(), pending)
    }

    pub fn has_pending_output(&self) -> bool {
        !self.outbound.bytes.is_empty()
    }

    /// Finish an attached stream with the process status after rendered output drains.
    pub fn send_exit(&mut self, status: i32) -> io::Result<()> {
        self.send_control(ServerMessage::Exit { status })
    }

    /// Ask the attached client to restore its terminal and open the session manager.
    pub fn send_session_manager(&mut self) -> io::Result<()> {
        self.send_control(ServerMessage::OpenSessionManager)
    }

    /// Ask the attached client to restore its terminal and detach.
    pub fn send_detach(&mut self) -> io::Result<()> {
        self.send_control(ServerMessage::Detach)
    }

    fn send_control(&mut self, message: ServerMessage) -> io::Result<()> {
        if self.state != ConnectionState::Attached {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "session client is no longer attached",
            ));
        }
        if self.has_pending_output() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "session output frame is still pending",
            ));
        }
        let frame = message
            .encode()
            .map_err(|error| invalid_data(error.to_string()))?;
        let stream = self.peer.stream_mut();
        stream.set_nonblocking(false)?;
        stream.set_write_timeout(Some(EXIT_WRITE_TIMEOUT))?;
        let written = stream.write_all(&frame);
        let timeout = stream.set_write_timeout(None);
        let nonblocking = stream.set_nonblocking(true);
        written?;
        timeout?;
        nonblocking
    }
}

fn winsize(rows: u16, columns: u16) -> Winsize {
    Winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[derive(Debug, Default)]
struct Outbound {
    bytes: Vec<u8>,
    written: usize,
    source_bytes: usize,
}

impl Outbound {
    fn write_to(&mut self, writer: &mut impl Write, source: &mut VecDeque<u8>) -> io::Result<()> {
        if self.bytes.is_empty() {
            let count = source.len().min(MAX_FRAME_BYTES);
            if count == 0 {
                return Ok(());
            }
            let payload: Vec<_> = source.iter().take(count).copied().collect();
            self.bytes = ServerMessage::Output(payload)
                .encode()
                .map_err(|error| invalid_data(error.to_string()))?;
            self.source_bytes = count;
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
            source.drain(..self.source_bytes);
            self.bytes.clear();
            self.written = 0;
            self.source_bytes = 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::handshake::{self, ClientPeer};
    use crate::session::protocol::{ClientMessage, ServerMessage};
    use std::os::unix::net::UnixStream;
    use std::thread;

    fn connected(rows: u16, columns: u16) -> (ClientPeer, ServerFrontend) {
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || handshake::server(server_stream).unwrap());
        let client = handshake::client(client_stream, rows, columns).unwrap();
        let frontend = ServerFrontend::new(server.join().unwrap());
        (client, frontend)
    }

    fn write_messages(client: &mut ClientPeer, messages: &[ClientMessage]) {
        let bytes: Vec<_> = messages
            .iter()
            .flat_map(|message| message.encode().unwrap())
            .collect();
        client.stream_mut().write_all(&bytes).unwrap();
    }

    #[test]
    fn receives_input_latest_resize_and_explicit_detach() {
        let (mut client, mut frontend) = connected(24, 80);
        let initial = frontend.take_resize().unwrap();
        assert_eq!((initial.ws_row, initial.ws_col), (24, 80));

        write_messages(
            &mut client,
            &[
                ClientMessage::Input(b"first".to_vec()),
                ClientMessage::Resize {
                    rows: 40,
                    columns: 100,
                },
                ClientMessage::Resize {
                    rows: 41,
                    columns: 101,
                },
                ClientMessage::Input(b"second".to_vec()),
                ClientMessage::Detach,
            ],
        );
        assert_eq!(frontend.receive().unwrap(), ConnectionState::Detached);
        let resize = frontend.take_resize().unwrap();
        assert_eq!((resize.ws_row, resize.ws_col), (41, 101));
        let mut input = VecDeque::new();
        assert_eq!(frontend.drain_input(&mut input, 7), 7);
        assert_eq!(input, b"firstse".to_vec());
        assert_eq!(frontend.drain_input(&mut input, 11), 4);
        assert_eq!(input, b"firstsecond".to_vec());
    }

    #[test]
    fn distinguishes_clean_disconnect_from_detach() {
        let (client, mut frontend) = connected(24, 80);
        drop(client);
        assert_eq!(frontend.receive().unwrap(), ConnectionState::Disconnected);
    }

    #[test]
    fn preserves_input_coalesced_with_the_handshake_before_eof() {
        let (mut client_stream, server_stream) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || handshake::server(server_stream).unwrap());
        let mut bytes = ClientMessage::Hello {
            version: crate::session::protocol::PROTOCOL_VERSION,
            rows: 24,
            columns: 80,
        }
        .encode()
        .unwrap();
        bytes.extend(ClientMessage::Input(b"retained".to_vec()).encode().unwrap());
        client_stream.write_all(&bytes).unwrap();

        let mut decoder = crate::session::protocol::ServerDecoder::default();
        loop {
            let mut reply = [0; 32];
            match client_stream.read(&mut reply) {
                Ok(count) if !decoder.push(&reply[..count]).unwrap().is_empty() => break,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("handshake reply failed: {error}"),
            }
        }
        drop(client_stream);

        let mut frontend = ServerFrontend::new(server.join().unwrap());
        assert_eq!(frontend.receive().unwrap(), ConnectionState::Attached);
        let mut input = VecDeque::new();
        frontend.drain_input(&mut input, MAX_FRAME_BYTES);
        assert_eq!(input, b"retained".to_vec());
        assert_eq!(frontend.receive().unwrap(), ConnectionState::Disconnected);
    }

    #[test]
    fn frames_large_output_without_losing_source_bytes() {
        let (mut client, mut frontend) = connected(24, 80);
        let expected = vec![b'x'; MAX_FRAME_BYTES + 3];
        let mut source = VecDeque::from(expected.clone());
        let mut received = Vec::new();
        let mut bytes = [0; READ_BYTES];

        for _ in 0..100 {
            frontend.send_output(&mut source).unwrap();
            loop {
                match client.stream_mut().read(&mut bytes) {
                    Ok(0) => panic!("server disconnected"),
                    Ok(count) => {
                        for message in client.decode(&bytes[..count]).unwrap() {
                            match message {
                                ServerMessage::Output(bytes) => received.extend(bytes),
                                other => panic!("unexpected message: {other:?}"),
                            }
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("read failed: {error}"),
                }
            }
            if source.is_empty() && !frontend.has_pending_output() {
                break;
            }
        }
        assert!(source.is_empty());
        assert_eq!(received, expected);
    }

    #[test]
    fn sends_exit_after_output_has_drained() {
        let (mut client, mut frontend) = connected(24, 80);
        frontend.send_exit(-15).unwrap();
        let mut bytes = [0; 64];
        let count = client.stream_mut().read(&mut bytes).unwrap();
        assert_eq!(
            client.decode(&bytes[..count]).unwrap(),
            [ServerMessage::Exit { status: -15 }]
        );
    }

    #[test]
    fn sends_session_manager_only_after_output_has_drained() {
        let (mut client, mut frontend) = connected(24, 80);
        let mut output = VecDeque::from(b"frame".to_vec());
        frontend.send_output(&mut output).unwrap();
        if frontend.has_pending_output() {
            assert_eq!(
                frontend.send_session_manager().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            while frontend.has_pending_output() {
                frontend.send_output(&mut output).unwrap();
            }
        }
        frontend.send_session_manager().unwrap();

        let mut messages = Vec::new();
        let mut bytes = [0; 64];
        while messages.len() < 2 {
            match client.stream_mut().read(&mut bytes) {
                Ok(count) => messages.extend(client.decode(&bytes[..count]).unwrap()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::yield_now(),
                Err(error) => panic!("read failed: {error}"),
            }
        }
        assert_eq!(
            messages,
            [
                ServerMessage::Output(b"frame".to_vec()),
                ServerMessage::OpenSessionManager,
            ]
        );
    }

    #[test]
    fn sends_detach_control_to_attached_client() {
        let (mut client, mut frontend) = connected(24, 80);
        frontend.send_detach().unwrap();
        let mut bytes = [0; 64];
        let count = client.stream_mut().read(&mut bytes).unwrap();
        assert_eq!(
            client.decode(&bytes[..count]).unwrap(),
            [ServerMessage::Detach]
        );
    }

    #[test]
    fn partial_frame_keeps_source_until_the_last_write() {
        struct ShortWriter(Vec<u8>);
        impl Write for ShortWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let count = bytes.len().min(3);
                self.0.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut outbound = Outbound::default();
        let mut source = VecDeque::from(b"abcdef".to_vec());
        let mut writer = ShortWriter(Vec::new());
        outbound.write_to(&mut writer, &mut source).unwrap();
        assert_eq!(source, b"abcdef".to_vec());
        while !outbound.bytes.is_empty() {
            outbound.write_to(&mut writer, &mut source).unwrap();
        }
        assert!(source.is_empty());
        let mut decoder = crate::session::protocol::ServerDecoder::default();
        assert_eq!(
            decoder.push(&writer.0).unwrap(),
            [ServerMessage::Output(b"abcdef".to_vec())]
        );
    }
}

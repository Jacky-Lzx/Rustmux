//! Versioned handshake over a local Unix stream.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::protocol::{
    ClientDecoder, ClientMessage, PROTOCOL_VERSION, ProtocolError, ServerDecoder, ServerMessage,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// A client-side stream after the server has accepted the protocol version.
#[derive(Debug)]
pub struct ClientPeer {
    stream: UnixStream,
    decoder: ServerDecoder,
    pending: VecDeque<ServerMessage>,
    locked_enter: u8,
    legacy_client_shortcuts: bool,
}

impl ClientPeer {
    pub fn locked_entry_key(&self) -> u8 {
        self.locked_enter
    }

    pub fn legacy_client_shortcuts(&self) -> bool {
        self.legacy_client_shortcuts
    }

    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }

    pub fn stream_mut(&mut self) -> &mut UnixStream {
        &mut self.stream
    }

    /// Decode bytes after the handshake, including messages read with its reply.
    pub fn decode(&mut self, bytes: &[u8]) -> io::Result<Vec<ServerMessage>> {
        let mut messages: Vec<_> = self.pending.drain(..).collect();
        messages.extend(self.decoder.push(bytes).map_err(invalid_protocol)?);
        if messages
            .iter()
            .any(|message| matches!(message, ServerMessage::Attached { .. }))
        {
            return Err(invalid_data("server sent a second Attached message"));
        }
        Ok(messages)
    }

    pub fn finish(&mut self) -> io::Result<()> {
        self.decoder.finish().map_err(invalid_protocol)
    }
}

/// A server-side stream after receiving a compatible client hello.
#[derive(Debug)]
pub struct ServerPeer {
    stream: UnixStream,
    decoder: ClientDecoder,
    pending: VecDeque<ClientMessage>,
    rows: u16,
    columns: u16,
}

impl ServerPeer {
    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }

    pub fn stream_mut(&mut self) -> &mut UnixStream {
        &mut self.stream
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.columns)
    }

    /// Decode bytes after the handshake, including messages read with the hello.
    pub fn decode(&mut self, bytes: &[u8]) -> io::Result<Vec<ClientMessage>> {
        let mut messages: Vec<_> = self.pending.drain(..).collect();
        messages.extend(self.decoder.push(bytes).map_err(invalid_protocol)?);
        if messages
            .iter()
            .any(|message| matches!(message, ClientMessage::Hello { .. }))
        {
            return Err(invalid_data("client sent a second Hello message"));
        }
        Ok(messages)
    }

    pub fn finish(&mut self) -> io::Result<()> {
        self.decoder.finish().map_err(invalid_protocol)
    }
}

/// Send a hello, verify the reply and leave the connected stream nonblocking.
pub fn client(mut stream: UnixStream, rows: u16, columns: u16) -> io::Result<ClientPeer> {
    begin(&stream)?;
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        rows,
        columns,
    }
    .encode()
    .map_err(invalid_protocol)?;
    stream.write_all(&hello)?;

    let mut decoder = ServerDecoder::default();
    let mut messages = read_server_messages(&mut stream, &mut decoder)?;
    let (locked_enter, legacy_client_shortcuts) =
        match messages.pop_front().expect("reader returns a message") {
            ServerMessage::Attached {
                version,
                locked_enter,
                legacy_client_shortcuts,
            } if version == PROTOCOL_VERSION && (1..=26).contains(&locked_enter) => {
                (locked_enter, legacy_client_shortcuts)
            }
            ServerMessage::Attached { version, .. } if version != PROTOCOL_VERSION => {
                return Err(invalid_data(format!(
                    "server selected protocol version {version}; expected {PROTOCOL_VERSION}"
                )));
            }
            ServerMessage::Attached { .. } => {
                return Err(invalid_data("server selected an invalid LOCKED entry key"));
            }
            ServerMessage::Rejected(message) => {
                return Err(io::Error::new(io::ErrorKind::ConnectionRefused, message));
            }
            _ => {
                return Err(invalid_data(
                    "first server message must be Attached or Rejected",
                ));
            }
        };
    reject_server_handshakes(&messages)?;
    finish_handshake(&stream)?;
    Ok(ClientPeer {
        stream,
        decoder,
        pending: messages,
        locked_enter,
        legacy_client_shortcuts,
    })
}

/// Verify the first client message, reply and leave the accepted stream nonblocking.
pub fn server(stream: UnixStream) -> io::Result<ServerPeer> {
    server_with_keybinds(stream, 2, true)
}

pub fn server_with_keybinds(
    mut stream: UnixStream,
    locked_enter: u8,
    legacy_client_shortcuts: bool,
) -> io::Result<ServerPeer> {
    if !(1..=26).contains(&locked_enter) {
        return Err(invalid_data("invalid LOCKED entry key"));
    }
    begin(&stream)?;
    let mut decoder = ClientDecoder::default();
    let mut messages = read_client_messages(&mut stream, &mut decoder)?;
    let (version, rows, columns) = match messages.pop_front().expect("reader returns a message") {
        ClientMessage::Hello {
            version,
            rows,
            columns,
        } => (version, rows, columns),
        _ => return reject(&mut stream, "first client message must be Hello"),
    };
    if version != PROTOCOL_VERSION {
        return reject(
            &mut stream,
            &format!("unsupported protocol version {version}; expected {PROTOCOL_VERSION}"),
        );
    }
    reject_client_handshakes(&messages)?;
    stream.write_all(
        &ServerMessage::Attached {
            version: PROTOCOL_VERSION,
            locked_enter,
            legacy_client_shortcuts,
        }
        .encode()
        .map_err(invalid_protocol)?,
    )?;
    finish_handshake(&stream)?;
    Ok(ServerPeer {
        stream,
        decoder,
        pending: messages,
        rows,
        columns,
    })
}

fn begin(stream: &UnixStream) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))
}

fn finish_handshake(stream: &UnixStream) -> io::Result<()> {
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    stream.set_nonblocking(true)
}

fn read_client_messages(
    stream: &mut UnixStream,
    decoder: &mut ClientDecoder,
) -> io::Result<VecDeque<ClientMessage>> {
    loop {
        let mut bytes = [0; 8192];
        let count = read_handshake_bytes(stream, &mut bytes)?;
        let messages = decoder.push(&bytes[..count]).map_err(invalid_protocol)?;
        if !messages.is_empty() {
            return Ok(messages.into());
        }
    }
}

fn read_server_messages(
    stream: &mut UnixStream,
    decoder: &mut ServerDecoder,
) -> io::Result<VecDeque<ServerMessage>> {
    loop {
        let mut bytes = [0; 8192];
        let count = read_handshake_bytes(stream, &mut bytes)?;
        let messages = decoder.push(&bytes[..count]).map_err(invalid_protocol)?;
        if !messages.is_empty() {
            return Ok(messages.into());
        }
    }
}

fn read_handshake_bytes(stream: &mut UnixStream, bytes: &mut [u8]) -> io::Result<usize> {
    loop {
        match stream.read(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "session peer disconnected during handshake",
                ));
            }
            Ok(count) => return Ok(count),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn reject(stream: &mut UnixStream, message: &str) -> io::Result<ServerPeer> {
    stream.write_all(
        &ServerMessage::Rejected(message.to_owned())
            .encode()
            .map_err(invalid_protocol)?,
    )?;
    Err(invalid_data(message))
}

fn reject_client_handshakes(messages: &VecDeque<ClientMessage>) -> io::Result<()> {
    if messages
        .iter()
        .any(|message| matches!(message, ClientMessage::Hello { .. }))
    {
        Err(invalid_data("client sent a second Hello message"))
    } else {
        Ok(())
    }
}

fn reject_server_handshakes(messages: &VecDeque<ServerMessage>) -> io::Result<()> {
    if messages
        .iter()
        .any(|message| matches!(message, ServerMessage::Attached { .. }))
    {
        Err(invalid_data("server sent a second Attached message"))
    } else {
        Ok(())
    }
}

fn invalid_protocol(error: ProtocolError) -> io::Error {
    invalid_data(error.to_string())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn compatible_peers_exchange_size_and_preserve_coalesced_messages() {
        let (mut client_stream, server_stream) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || server(server_stream).unwrap());

        let mut bytes = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            rows: 37,
            columns: 101,
        }
        .encode()
        .unwrap();
        bytes.extend(ClientMessage::Input(b"pending".to_vec()).encode().unwrap());
        client_stream.write_all(&bytes).unwrap();

        let mut decoder = ServerDecoder::default();
        let reply = read_server_messages(&mut client_stream, &mut decoder).unwrap();
        assert_eq!(
            reply,
            [ServerMessage::Attached {
                version: PROTOCOL_VERSION,
                locked_enter: 2,
                legacy_client_shortcuts: true,
            }]
        );

        let mut peer = server.join().unwrap();
        assert_eq!(peer.size(), (37, 101));
        assert_eq!(
            peer.decode(&[]).unwrap(),
            [ClientMessage::Input(b"pending".to_vec())]
        );
        assert!(peer.stream().peer_addr().is_ok());
    }

    #[test]
    fn client_and_server_helpers_complete_a_real_handshake() {
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || server(server_stream).unwrap());
        let mut client = client(client_stream, 24, 80).unwrap();
        let server = server.join().unwrap();
        assert_eq!(server.size(), (24, 80));
        assert_eq!(client.locked_entry_key(), 2);
        assert!(client.legacy_client_shortcuts());
        assert!(client.decode(&[]).unwrap().is_empty());
        assert!(client.stream_mut().write(&[]).is_ok());
    }

    #[test]
    fn client_uses_the_server_selected_locked_entry_key() {
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || server_with_keybinds(server_stream, 1, false).unwrap());
        let client = client(client_stream, 24, 80).unwrap();
        assert_eq!(client.locked_entry_key(), 1);
        assert!(!client.legacy_client_shortcuts());
        server.join().unwrap();
    }

    #[test]
    fn incompatible_version_is_rejected_with_a_bounded_reason() {
        let (mut client_stream, server_stream) = UnixStream::pair().unwrap();
        let server_thread = thread::spawn(move || server(server_stream).unwrap_err());
        client_stream
            .write_all(
                &ClientMessage::Hello {
                    version: PROTOCOL_VERSION + 1,
                    rows: 24,
                    columns: 80,
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        let mut decoder = ServerDecoder::default();
        let reply = read_server_messages(&mut client_stream, &mut decoder).unwrap();
        assert!(matches!(
            reply.front(),
            Some(ServerMessage::Rejected(message)) if message.contains("unsupported protocol")
        ));
        assert_eq!(
            server_thread.join().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn client_rejects_an_unexpected_server_version() {
        let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
        let server_thread = thread::spawn(move || {
            let mut decoder = ClientDecoder::default();
            let hello = read_client_messages(&mut server_stream, &mut decoder).unwrap();
            assert!(matches!(hello.front(), Some(ClientMessage::Hello { .. })));
            server_stream
                .write_all(
                    &ServerMessage::Attached {
                        version: PROTOCOL_VERSION + 1,
                        locked_enter: 2,
                        legacy_client_shortcuts: true,
                    }
                    .encode()
                    .unwrap(),
                )
                .unwrap();
        });
        let error = client(client_stream, 24, 80).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("server selected protocol"));
        server_thread.join().unwrap();
    }

    #[test]
    fn client_rejects_an_invalid_server_prefix() {
        let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
        let server_thread = thread::spawn(move || {
            let mut decoder = ClientDecoder::default();
            read_client_messages(&mut server_stream, &mut decoder).unwrap();
            server_stream
                .write_all(
                    &ServerMessage::Attached {
                        version: PROTOCOL_VERSION,
                        locked_enter: 0,
                        legacy_client_shortcuts: true,
                    }
                    .encode()
                    .unwrap(),
                )
                .unwrap();
        });
        let error = client(client_stream, 24, 80).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("invalid LOCKED entry key"));
        server_thread.join().unwrap();
    }

    #[test]
    fn first_message_and_repeated_handshake_are_rejected() {
        let (mut client_stream, server_stream) = UnixStream::pair().unwrap();
        let first_server = thread::spawn(move || server(server_stream).unwrap_err());
        client_stream
            .write_all(&ClientMessage::Detach.encode().unwrap())
            .unwrap();
        let mut decoder = ServerDecoder::default();
        let reply = read_server_messages(&mut client_stream, &mut decoder).unwrap();
        assert!(matches!(reply.front(), Some(ServerMessage::Rejected(_))));
        assert_eq!(
            first_server.join().unwrap().kind(),
            io::ErrorKind::InvalidData
        );

        let (mut client_stream, server_stream) = UnixStream::pair().unwrap();
        let repeated_server = thread::spawn(move || server(server_stream).unwrap_err());
        let hello = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            rows: 24,
            columns: 80,
        }
        .encode()
        .unwrap();
        client_stream
            .write_all(&[hello.as_slice(), hello.as_slice()].concat())
            .unwrap();
        assert_eq!(
            repeated_server.join().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

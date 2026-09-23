//! Bounded framing for communication between a session server and its client.

use std::fmt;

pub const PROTOCOL_VERSION: u16 = 5;
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_ERROR_BYTES: usize = 1024;

const HEADER_BYTES: usize = 5;
const CLIENT_HELLO: u8 = 1;
const CLIENT_INPUT: u8 = 2;
const CLIENT_RESIZE: u8 = 3;
const CLIENT_DETACH: u8 = 4;
const SERVER_ATTACHED: u8 = 128;
const SERVER_OUTPUT: u8 = 129;
const SERVER_EXIT: u8 = 130;
const SERVER_REJECTED: u8 = 131;
const SERVER_OPEN_SESSION_MANAGER: u8 = 132;
const SERVER_DETACH: u8 = 133;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientMessage {
    Hello {
        version: u16,
        rows: u16,
        columns: u16,
    },
    Input(Vec<u8>),
    Resize {
        rows: u16,
        columns: u16,
    },
    Detach,
}

impl ClientMessage {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        match self {
            Self::Hello {
                version,
                rows,
                columns,
            } => {
                validate_size(*rows, *columns)?;
                let mut payload = Vec::with_capacity(6);
                payload.extend_from_slice(&version.to_be_bytes());
                payload.extend_from_slice(&rows.to_be_bytes());
                payload.extend_from_slice(&columns.to_be_bytes());
                encode_frame(CLIENT_HELLO, &payload)
            }
            Self::Input(bytes) => encode_frame(CLIENT_INPUT, bytes),
            Self::Resize { rows, columns } => {
                validate_size(*rows, *columns)?;
                let mut payload = Vec::with_capacity(4);
                payload.extend_from_slice(&rows.to_be_bytes());
                payload.extend_from_slice(&columns.to_be_bytes());
                encode_frame(CLIENT_RESIZE, &payload)
            }
            Self::Detach => encode_frame(CLIENT_DETACH, &[]),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerMessage {
    Attached {
        version: u16,
        locked_enter: u8,
        legacy_client_shortcuts: bool,
    },
    Output(Vec<u8>),
    Exit {
        status: i32,
    },
    Rejected(String),
    OpenSessionManager,
    Detach,
}

impl ServerMessage {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        match self {
            Self::Attached {
                version,
                locked_enter,
                legacy_client_shortcuts,
            } => {
                let mut payload = Vec::with_capacity(4);
                payload.extend_from_slice(&version.to_be_bytes());
                payload.push(*locked_enter);
                payload.push(u8::from(*legacy_client_shortcuts));
                encode_frame(SERVER_ATTACHED, &payload)
            }
            Self::Output(bytes) => encode_frame(SERVER_OUTPUT, bytes),
            Self::Exit { status } => encode_frame(SERVER_EXIT, &status.to_be_bytes()),
            Self::Rejected(message) => {
                if message.len() > MAX_ERROR_BYTES {
                    return Err(ProtocolError::ErrorMessageTooLong(message.len()));
                }
                encode_frame(SERVER_REJECTED, message.as_bytes())
            }
            Self::OpenSessionManager => encode_frame(SERVER_OPEN_SESSION_MANAGER, &[]),
            Self::Detach => encode_frame(SERVER_DETACH, &[]),
        }
    }
}

#[derive(Debug, Default)]
pub struct ClientDecoder(FrameDecoder);

impl ClientDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<ClientMessage>, ProtocolError> {
        self.0.push(bytes)?.into_iter().map(decode_client).collect()
    }

    pub fn finish(&mut self) -> Result<(), ProtocolError> {
        self.0.finish()
    }
}

#[derive(Debug, Default)]
pub struct ServerDecoder(FrameDecoder);

impl ServerDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<ServerMessage>, ProtocolError> {
        self.0.push(bytes)?.into_iter().map(decode_server).collect()
    }

    pub fn finish(&mut self) -> Result<(), ProtocolError> {
        self.0.finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    FrameTooLarge(usize),
    ErrorMessageTooLong(usize),
    UnknownMessage(u8),
    InvalidLength {
        message: u8,
        expected: usize,
        actual: usize,
    },
    InvalidTerminalSize,
    InvalidHandshakeFlags(u8),
    InvalidUtf8,
    TruncatedFrame(usize),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge(length) => {
                write!(
                    formatter,
                    "session frame is {length} bytes; limit is {MAX_FRAME_BYTES}"
                )
            }
            Self::ErrorMessageTooLong(length) => write!(
                formatter,
                "session error is {length} bytes; limit is {MAX_ERROR_BYTES}"
            ),
            Self::UnknownMessage(message) => {
                write!(formatter, "unknown session message type {message}")
            }
            Self::InvalidLength {
                message,
                expected,
                actual,
            } => write!(
                formatter,
                "session message type {message} needs {expected} bytes, received {actual}"
            ),
            Self::InvalidTerminalSize => {
                formatter.write_str("terminal rows and columns must be nonzero")
            }
            Self::InvalidHandshakeFlags(flags) => {
                write!(formatter, "invalid session handshake flags {flags}")
            }
            Self::InvalidUtf8 => formatter.write_str("session error message is not UTF-8"),
            Self::TruncatedFrame(length) => {
                write!(
                    formatter,
                    "session stream ended with {length} incomplete bytes"
                )
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Debug)]
struct Frame {
    message: u8,
    payload: Vec<u8>,
}

#[derive(Debug, Default)]
struct FrameDecoder {
    pending: Vec<u8>,
}

impl FrameDecoder {
    fn push(&mut self, mut bytes: &[u8]) -> Result<Vec<Frame>, ProtocolError> {
        let mut frames = Vec::new();
        while !bytes.is_empty() {
            let available = (HEADER_BYTES + MAX_FRAME_BYTES).saturating_sub(self.pending.len());
            if available == 0 {
                self.pending.clear();
                return Err(ProtocolError::FrameTooLarge(MAX_FRAME_BYTES + 1));
            }
            let count = available.min(bytes.len());
            self.pending.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
            self.take_complete(&mut frames)?;
        }
        Ok(frames)
    }

    fn take_complete(&mut self, frames: &mut Vec<Frame>) -> Result<(), ProtocolError> {
        loop {
            if self.pending.len() < HEADER_BYTES {
                return Ok(());
            }
            let length =
                u32::from_be_bytes(self.pending[1..HEADER_BYTES].try_into().unwrap()) as usize;
            if length > MAX_FRAME_BYTES {
                self.pending.clear();
                return Err(ProtocolError::FrameTooLarge(length));
            }
            let frame_length = HEADER_BYTES + length;
            if self.pending.len() < frame_length {
                return Ok(());
            }
            let payload = self.pending[HEADER_BYTES..frame_length].to_vec();
            let message = self.pending[0];
            self.pending.drain(..frame_length);
            frames.push(Frame { message, payload });
        }
    }

    fn finish(&mut self) -> Result<(), ProtocolError> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            let length = self.pending.len();
            self.pending.clear();
            Err(ProtocolError::TruncatedFrame(length))
        }
    }
}

fn encode_frame(message: u8, payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge(payload.len()));
    }
    let mut frame = Vec::with_capacity(HEADER_BYTES + payload.len());
    frame.push(message);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

fn decode_client(frame: Frame) -> Result<ClientMessage, ProtocolError> {
    match frame.message {
        CLIENT_HELLO => {
            require_length(&frame, 6)?;
            let version = u16::from_be_bytes(frame.payload[0..2].try_into().unwrap());
            let rows = u16::from_be_bytes(frame.payload[2..4].try_into().unwrap());
            let columns = u16::from_be_bytes(frame.payload[4..6].try_into().unwrap());
            validate_size(rows, columns)?;
            Ok(ClientMessage::Hello {
                version,
                rows,
                columns,
            })
        }
        CLIENT_INPUT => Ok(ClientMessage::Input(frame.payload)),
        CLIENT_RESIZE => {
            require_length(&frame, 4)?;
            let rows = u16::from_be_bytes(frame.payload[0..2].try_into().unwrap());
            let columns = u16::from_be_bytes(frame.payload[2..4].try_into().unwrap());
            validate_size(rows, columns)?;
            Ok(ClientMessage::Resize { rows, columns })
        }
        CLIENT_DETACH => {
            require_length(&frame, 0)?;
            Ok(ClientMessage::Detach)
        }
        message => Err(ProtocolError::UnknownMessage(message)),
    }
}

fn decode_server(frame: Frame) -> Result<ServerMessage, ProtocolError> {
    match frame.message {
        SERVER_ATTACHED => {
            require_length(&frame, 4)?;
            let version = u16::from_be_bytes(frame.payload[0..2].try_into().unwrap());
            let locked_enter = frame.payload[2];
            let legacy_client_shortcuts = match frame.payload[3] {
                0 => false,
                1 => true,
                flags => return Err(ProtocolError::InvalidHandshakeFlags(flags)),
            };
            Ok(ServerMessage::Attached {
                version,
                locked_enter,
                legacy_client_shortcuts,
            })
        }
        SERVER_OUTPUT => Ok(ServerMessage::Output(frame.payload)),
        SERVER_EXIT => {
            require_length(&frame, 4)?;
            let status = i32::from_be_bytes(frame.payload[0..4].try_into().unwrap());
            Ok(ServerMessage::Exit { status })
        }
        SERVER_REJECTED => {
            if frame.payload.len() > MAX_ERROR_BYTES {
                return Err(ProtocolError::ErrorMessageTooLong(frame.payload.len()));
            }
            let message =
                String::from_utf8(frame.payload).map_err(|_| ProtocolError::InvalidUtf8)?;
            Ok(ServerMessage::Rejected(message))
        }
        SERVER_OPEN_SESSION_MANAGER => {
            require_length(&frame, 0)?;
            Ok(ServerMessage::OpenSessionManager)
        }
        SERVER_DETACH => {
            require_length(&frame, 0)?;
            Ok(ServerMessage::Detach)
        }
        message => Err(ProtocolError::UnknownMessage(message)),
    }
}

fn require_length(frame: &Frame, expected: usize) -> Result<(), ProtocolError> {
    if frame.payload.len() == expected {
        Ok(())
    } else {
        Err(ProtocolError::InvalidLength {
            message: frame.message,
            expected,
            actual: frame.payload.len(),
        })
    }
}

fn validate_size(rows: u16, columns: u16) -> Result<(), ProtocolError> {
    if rows == 0 || columns == 0 {
        Err(ProtocolError::InvalidTerminalSize)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_messages_round_trip_across_every_byte_boundary() {
        let expected = vec![
            ClientMessage::Hello {
                version: PROTOCOL_VERSION,
                rows: 42,
                columns: 120,
            },
            ClientMessage::Input(vec![0, b'a', 0xff, b'\n']),
            ClientMessage::Resize {
                rows: 24,
                columns: 80,
            },
            ClientMessage::Detach,
        ];
        let encoded: Vec<u8> = expected
            .iter()
            .flat_map(|message| message.encode().unwrap())
            .collect();
        let mut decoder = ClientDecoder::default();
        let mut actual = Vec::new();
        for byte in encoded {
            actual.extend(decoder.push(&[byte]).unwrap());
        }
        decoder.finish().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn server_messages_round_trip_when_frames_are_coalesced() {
        let expected = vec![
            ServerMessage::Attached {
                version: PROTOCOL_VERSION,
                locked_enter: 2,
                legacy_client_shortcuts: true,
            },
            ServerMessage::Output(vec![b'\x1b', b'[', b'2', b'J', 0]),
            ServerMessage::Rejected("already attached".to_owned()),
            ServerMessage::OpenSessionManager,
            ServerMessage::Detach,
            ServerMessage::Exit { status: -15 },
        ];
        let encoded: Vec<u8> = expected
            .iter()
            .flat_map(|message| message.encode().unwrap())
            .collect();
        let mut decoder = ServerDecoder::default();
        assert_eq!(decoder.push(&encoded).unwrap(), expected);
        decoder.finish().unwrap();
    }

    #[test]
    fn encoders_reject_invalid_sizes_and_bounded_payloads() {
        assert_eq!(
            ClientMessage::Resize {
                rows: 0,
                columns: 80
            }
            .encode(),
            Err(ProtocolError::InvalidTerminalSize)
        );
        assert_eq!(
            ClientMessage::Input(vec![0; MAX_FRAME_BYTES + 1]).encode(),
            Err(ProtocolError::FrameTooLarge(MAX_FRAME_BYTES + 1))
        );
        assert_eq!(
            ServerMessage::Rejected("x".repeat(MAX_ERROR_BYTES + 1)).encode(),
            Err(ProtocolError::ErrorMessageTooLong(MAX_ERROR_BYTES + 1))
        );
    }

    #[test]
    fn decoders_reject_oversized_unknown_and_malformed_frames() {
        let mut oversized = vec![CLIENT_INPUT];
        oversized.extend_from_slice(&((MAX_FRAME_BYTES as u32) + 1).to_be_bytes());
        assert_eq!(
            ClientDecoder::default().push(&oversized),
            Err(ProtocolError::FrameTooLarge(MAX_FRAME_BYTES + 1))
        );

        let unknown = encode_frame(99, &[]).unwrap();
        assert_eq!(
            ClientDecoder::default().push(&unknown),
            Err(ProtocolError::UnknownMessage(99))
        );

        let malformed = encode_frame(CLIENT_RESIZE, &[0, 1]).unwrap();
        assert_eq!(
            ClientDecoder::default().push(&malformed),
            Err(ProtocolError::InvalidLength {
                message: CLIENT_RESIZE,
                expected: 4,
                actual: 2,
            })
        );

        let malformed_attached =
            encode_frame(SERVER_ATTACHED, &PROTOCOL_VERSION.to_be_bytes()).unwrap();
        assert_eq!(
            ServerDecoder::default().push(&malformed_attached),
            Err(ProtocolError::InvalidLength {
                message: SERVER_ATTACHED,
                expected: 4,
                actual: 2,
            })
        );

        let invalid_flags = encode_frame(SERVER_ATTACHED, &[0, 5, 2, 2]).unwrap();
        assert_eq!(
            ServerDecoder::default().push(&invalid_flags),
            Err(ProtocolError::InvalidHandshakeFlags(2))
        );

        let malformed_detach = encode_frame(SERVER_DETACH, &[0]).unwrap();
        assert_eq!(
            ServerDecoder::default().push(&malformed_detach),
            Err(ProtocolError::InvalidLength {
                message: SERVER_DETACH,
                expected: 0,
                actual: 1,
            })
        );

        let invalid_utf8 = encode_frame(SERVER_REJECTED, &[0xff]).unwrap();
        assert_eq!(
            ServerDecoder::default().push(&invalid_utf8),
            Err(ProtocolError::InvalidUtf8)
        );
    }

    #[test]
    fn truncated_input_is_reported_and_decoder_can_be_reused() {
        let frame = ClientMessage::Input(b"hello".to_vec()).encode().unwrap();
        let mut decoder = ClientDecoder::default();
        assert!(decoder.push(&frame[..frame.len() - 1]).unwrap().is_empty());
        assert_eq!(
            decoder.finish(),
            Err(ProtocolError::TruncatedFrame(frame.len() - 1))
        );
        assert_eq!(
            decoder
                .push(&ClientMessage::Detach.encode().unwrap())
                .unwrap(),
            vec![ClientMessage::Detach]
        );
    }

    #[test]
    fn large_coalesced_reads_do_not_require_an_unbounded_pending_buffer() {
        let frame = ClientMessage::Input(vec![b'x'; MAX_FRAME_BYTES])
            .encode()
            .unwrap();
        let input = [frame.as_slice(), frame.as_slice()].concat();
        let mut decoder = ClientDecoder::default();
        let messages = decoder.push(&input).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages.into_iter().all(
            |message| matches!(message, ClientMessage::Input(bytes) if bytes.len() == MAX_FRAME_BYTES)
        ));
    }
}

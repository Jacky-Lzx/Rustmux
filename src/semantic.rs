//! Bounded OSC 7 directory metadata and OSC 133 command-output capture.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

const MAX_OSC_BYTES: usize = 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Default)]
pub(crate) struct SemanticOutput {
    state: State,
    capturing: bool,
    semantic_boundaries: bool,
    overflowed: bool,
    current: Vec<u8>,
    last: Option<String>,
    current_directory: Option<PathBuf>,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi,
    Osc {
        bytes: Vec<u8>,
        overflowed: bool,
    },
    OscEscape {
        bytes: Vec<u8>,
        overflowed: bool,
    },
    String,
    StringEscape,
}

impl SemanticOutput {
    pub fn advance(&mut self, input: &[u8]) {
        for &byte in input {
            if matches!(byte, 0x18 | 0x1a) {
                self.state = State::Ground;
                continue;
            }
            let state = std::mem::take(&mut self.state);
            self.state = match state {
                State::Ground => self.ground(byte),
                State::Escape => match byte {
                    b'[' => State::Csi,
                    b']' => State::Osc {
                        bytes: Vec::new(),
                        overflowed: false,
                    },
                    b'P' | b'X' | b'^' | b'_' => State::String,
                    0x1b => State::Escape,
                    _ => State::Ground,
                },
                State::Csi => {
                    if byte == 0x1b {
                        State::Escape
                    } else if (0x40..=0x7e).contains(&byte) {
                        State::Ground
                    } else {
                        State::Csi
                    }
                }
                State::Osc {
                    mut bytes,
                    mut overflowed,
                } => match byte {
                    7 => {
                        if !overflowed {
                            self.osc(&bytes);
                        }
                        State::Ground
                    }
                    0x1b => State::OscEscape { bytes, overflowed },
                    _ => {
                        push_bounded(&mut bytes, byte, &mut overflowed);
                        State::Osc { bytes, overflowed }
                    }
                },
                State::OscEscape {
                    mut bytes,
                    mut overflowed,
                } => {
                    if byte == b'\\' {
                        if !overflowed {
                            self.osc(&bytes);
                        }
                        State::Ground
                    } else {
                        push_bounded(&mut bytes, 0x1b, &mut overflowed);
                        push_bounded(&mut bytes, byte, &mut overflowed);
                        State::Osc { bytes, overflowed }
                    }
                }
                State::String => {
                    if byte == 0x1b {
                        State::StringEscape
                    } else {
                        State::String
                    }
                }
                State::StringEscape => match byte {
                    b'\\' => State::Ground,
                    0x1b => State::StringEscape,
                    _ => State::String,
                },
            };
        }
    }

    pub fn last_output(&self) -> Option<String> {
        if self.capturing && !self.semantic_boundaries {
            (!self.overflowed)
                .then(|| fallback_output(&self.current))
                .filter(|output| !output.is_empty())
        } else if self.capturing {
            None
        } else {
            self.last.clone()
        }
    }

    pub fn current_directory(&self) -> Option<&Path> {
        self.current_directory.as_deref()
    }

    pub fn set_current_directory(&mut self, path: PathBuf) {
        self.current_directory = Some(path);
    }

    pub fn cancel_current(&mut self) {
        self.capturing = false;
        self.semantic_boundaries = false;
        self.overflowed = false;
        self.current = Vec::new();
    }

    /// Begin best-effort capture when input submits a command without OSC 133.
    /// Exact semantic boundaries take precedence once a shell emits them.
    pub fn command_submitted(&mut self) {
        if self.capturing && self.semantic_boundaries {
            return;
        }
        if self.capturing {
            self.last = if self.overflowed {
                None
            } else {
                let output = fallback_output(&self.current);
                (!output.is_empty()).then_some(output)
            };
        }
        self.current.clear();
        self.capturing = true;
        self.semantic_boundaries = false;
        self.overflowed = false;
    }

    fn ground(&mut self, byte: u8) -> State {
        match byte {
            0x1b => State::Escape,
            b'\n' | b'\t' if self.capturing => {
                self.push(byte);
                State::Ground
            }
            b'\r' => State::Ground,
            8 if self.capturing => {
                remove_last_scalar(&mut self.current);
                State::Ground
            }
            0x20..=0x7e | 0x80..=0xff if self.capturing => {
                self.push(byte);
                State::Ground
            }
            _ => State::Ground,
        }
    }

    fn osc(&mut self, control: &[u8]) {
        if let Some(path) = control.strip_prefix(b"7;").and_then(osc7_path) {
            self.current_directory = Some(path);
            return;
        }
        let mut fields = control.split(|&byte| byte == b';');
        if fields.next() != Some(b"133".as_slice()) {
            return;
        }
        match fields.next() {
            Some(b"C") => {
                self.current.clear();
                self.capturing = true;
                self.semantic_boundaries = true;
                self.overflowed = false;
            }
            Some(b"D" | b"A") if self.capturing => self.complete(),
            _ => {}
        }
    }

    fn push(&mut self, byte: u8) {
        if self.current.len() == MAX_OUTPUT_BYTES {
            self.overflowed = true;
        } else if !self.overflowed {
            self.current.push(byte);
        }
    }

    fn complete(&mut self) {
        self.last = if self.overflowed {
            None
        } else {
            Some(String::from_utf8_lossy(&std::mem::take(&mut self.current)).into_owned())
        };
        self.cancel_current();
    }
}

fn osc7_path(uri: &[u8]) -> Option<PathBuf> {
    let uri = uri.strip_prefix(b"file://")?;
    let path_start = uri.iter().position(|&byte| byte == b'/')?;
    let encoded = &uri[path_start..];
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] == b'%' {
            let high = hex_digit(*encoded.get(index + 1)?)?;
            let low = hex_digit(*encoded.get(index + 2)?)?;
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(encoded[index]);
            index += 1;
        }
    }
    (!decoded.contains(&0) && decoded.starts_with(b"/"))
        .then(|| PathBuf::from(OsString::from_vec(decoded)))
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn push_bounded(bytes: &mut Vec<u8>, byte: u8, overflowed: &mut bool) {
    if bytes.len() == MAX_OSC_BYTES {
        *overflowed = true;
    } else if !*overflowed {
        bytes.push(byte);
    }
}

fn remove_last_scalar(bytes: &mut Vec<u8>) {
    while let Some(byte) = bytes.pop() {
        if byte & 0xc0 != 0x80 {
            break;
        }
    }
}

fn fallback_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines().collect::<Vec<_>>();
    if !lines.is_empty() {
        lines.remove(0); // The terminal normally echoes the submitted command.
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if !lines.is_empty() {
        lines.pop(); // The final line is the next prompt.
    }
    lines.join("\n").trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_completed_output_and_strips_terminal_controls() {
        let mut output = SemanticOutput::default();
        for chunk in [
            b"ignored\x1b]133;".as_slice(),
            b"C\x07red\x1b[31m \xe4",
            b"\xb8\xad\x1b[0m\r\nnext\tX\x1b]133;D;0\x1b",
            b"\\prompt",
        ] {
            output.advance(chunk);
        }
        assert_eq!(output.last_output(), Some("red 中\nnext\tX".to_owned()));
    }

    #[test]
    fn command_echo_and_prompt_bound_output_without_shell_integration() {
        let mut output = SemanticOutput::default();
        output.command_submitted();
        output.advance(b"printf test\r\ntest\r\n$ ");
        assert_eq!(output.last_output(), Some("test".to_owned()));

        output.command_submitted();
        output.advance(b"printf next\r\nnext\r\n$ ");
        assert_eq!(output.last_output(), Some("next".to_owned()));
    }

    #[test]
    fn osc133_replaces_an_active_heuristic_capture() {
        let mut output = SemanticOutput::default();
        output.command_submitted();
        output.advance(b"echo ignored\r\n\x1b]133;C\x07exact\x1b]133;D\x07$ ");
        assert_eq!(output.last_output(), Some("exact".to_owned()));
    }

    #[test]
    fn running_incomplete_and_oversized_commands_are_unavailable() {
        let mut output = SemanticOutput::default();
        output.advance(b"\x1b]133;C\x07old\x1b]133;D\x07");
        assert_eq!(output.last_output(), Some("old".to_owned()));
        output.advance(b"\x1b]133;C\x07running");
        assert_eq!(output.last_output(), None);
        output.advance(b"\x1b]133;C\x07new\x1b]133;A\x07");
        assert_eq!(output.last_output(), Some("new".to_owned()));

        output.advance(b"\x1b]133;C\x07");
        output.advance(&vec![b'x'; MAX_OUTPUT_BYTES + 1]);
        output.advance(b"\x1b]133;D\x07");
        assert_eq!(output.last_output(), None);

        output.command_submitted();
        output.advance(&vec![b'x'; MAX_OUTPUT_BYTES + 1]);
        assert_eq!(output.last_output(), None);
    }

    #[test]
    fn malformed_cancelled_and_overlong_markers_do_not_create_boundaries() {
        let mut output = SemanticOutput::default();
        output.advance(b"\x1b]133;Cat\x07text\x1b]133;D\x07");
        assert_eq!(output.last_output(), None);
        output.advance(b"\x1b]133;\x18C\x07text\x1b]133;D\x07");
        assert_eq!(output.last_output(), None);

        let mut marker = b"\x1b]133;".to_vec();
        marker.extend(std::iter::repeat_n(b'x', MAX_OSC_BYTES + 1));
        marker.push(7);
        output.advance(&marker);
        assert_eq!(output.last_output(), None);
    }

    #[test]
    fn backspace_removes_one_complete_utf8_scalar() {
        let mut output = SemanticOutput::default();
        output.advance(b"\x1b]133;C\x07A\xe4\xb8\xad\x08B\x1b]133;D\x07");
        assert_eq!(output.last_output(), Some("AB".to_owned()));
    }

    #[test]
    fn osc7_tracks_absolute_percent_decoded_paths_without_interrupting_capture() {
        let mut output = SemanticOutput::default();
        output.advance(b"\x1b]133;C\x07before\x1b]7;file://host/tmp/My%20Project\x1b\\after");
        assert_eq!(
            output.current_directory(),
            Some(Path::new("/tmp/My Project"))
        );
        output.advance(b"\x1b]133;D\x07");
        assert_eq!(output.last_output(), Some("beforeafter".to_owned()));
    }

    #[test]
    fn invalid_osc7_does_not_replace_the_last_valid_directory() {
        let mut output = SemanticOutput::default();
        output.advance(b"\x1b]7;file:///tmp/valid\x07");
        for invalid in [
            b"\x1b]7;https://host/tmp\x07".as_slice(),
            b"\x1b]7;file://host/relative%GG\x07",
            b"\x1b]7;file://host/tmp/%00bad\x07",
        ] {
            output.advance(invalid);
        }
        assert_eq!(output.current_directory(), Some(Path::new("/tmp/valid")));
    }
}

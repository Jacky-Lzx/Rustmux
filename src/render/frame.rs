//! Bounded output queue shared by text frames and outer-terminal images.

use std::collections::VecDeque;
use std::io::{self, Write};

const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";
pub(crate) const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Keep text, placements, and deletions in a single outer-terminal update.
/// End-marker space is reserved even when uploads fill the bounded frame.
pub(crate) fn synchronized_frame(
    output: &mut VecDeque<u8>,
    render: impl FnOnce(&mut VecDeque<u8>) -> io::Result<()>,
) -> io::Result<()> {
    let start = output.len();
    FrameWriter(output).write_all(SYNC_BEGIN)?;
    let end_position = output.len();
    if let Err(error) = FrameWriter(output).write_all(SYNC_END) {
        output.truncate(start);
        return Err(error);
    }
    if let Err(error) = render(output) {
        output.truncate(start);
        return Err(error);
    }
    output.drain(end_position..end_position + SYNC_END.len());
    output.extend(SYNC_END);
    Ok(())
}

pub(crate) struct FrameWriter<'a>(pub(crate) &'a mut VecDeque<u8>);
impl Write for FrameWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME - self.0.len() {
            return Err(io::Error::other("rendered frame exceeds output limit"));
        }
        self.0.try_reserve(bytes.len()).map_err(io::Error::other)?;
        self.0.extend(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronized_frame_reserves_end_marker_at_output_limit() {
        let mut output = VecDeque::new();
        synchronized_frame(&mut output, |frame| {
            let available = MAX_FRAME - frame.len();
            FrameWriter(frame).write_all(&vec![b'x'; available])?;
            assert!(FrameWriter(frame).write_all(b"overflow").is_err());
            Ok(())
        })
        .unwrap();
        let bytes: Vec<_> = output.into();
        assert_eq!(bytes.len(), MAX_FRAME);
        assert!(bytes.starts_with(SYNC_BEGIN));
        assert!(bytes.ends_with(SYNC_END));
        assert!(
            bytes[SYNC_BEGIN.len()..bytes.len() - SYNC_END.len()]
                .iter()
                .all(|&byte| byte == b'x')
        );
    }

    #[test]
    fn synchronized_frame_discards_failed_update_without_leaving_terminal_paused() {
        let mut output = VecDeque::from(b"previous".to_vec());
        let result = synchronized_frame(&mut output, |frame| {
            FrameWriter(frame).write_all(b"partial")?;
            Err(io::Error::other("render failed"))
        });
        assert!(result.is_err());
        assert_eq!(Vec::from(output), b"previous");
    }
}

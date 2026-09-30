//! Bounded output queue shared by text frames and outer-terminal images.

use std::collections::VecDeque;
use std::io::{self, Write};

pub(crate) const MAX_FRAME: usize = 16 * 1024 * 1024;

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

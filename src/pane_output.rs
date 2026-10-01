//! Bounded raw PTY tail. Cursors address bytes, independently of terminal parsing.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, io};

pub(crate) const LIMIT: usize = 64 * 1024;

#[derive(Debug, Default)]
pub(crate) struct Output {
    bytes: VecDeque<u8>,
    end: u64,
    generation: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Chunk {
    pub pane: u64,
    pub server_pid: u32,
    pub generation: u64,
    pub start: u64,
    pub next: u64,
    pub dropped: u64,
    pub complete: bool,
    pub bytes_base64: String,
}

impl Output {
    pub fn append(&mut self, bytes: &[u8]) {
        self.end = self
            .end
            .checked_add(bytes.len() as u64)
            .expect("PTY output cursor exhausted");
        if bytes.len() >= LIMIT {
            self.bytes.clear();
            self.bytes.extend(&bytes[bytes.len() - LIMIT..]);
        } else {
            let discard = (self.bytes.len() + bytes.len()).saturating_sub(LIMIT);
            self.bytes.drain(..discard);
            self.bytes.extend(bytes);
        }
    }

    pub fn restart(&mut self) {
        self.bytes.clear();
        self.generation = self
            .generation
            .checked_add(1)
            .expect("PTY generation exhausted");
    }

    pub fn read(&self, pane: u64, after: Option<u64>, complete: bool) -> io::Result<Chunk> {
        let after = after.unwrap_or(self.end);
        if after > self.end {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output cursor is ahead of pane",
            ));
        }
        let oldest = self.end - self.bytes.len() as u64;
        let start = after.max(oldest);
        let bytes: Vec<_> = self
            .bytes
            .iter()
            .skip((start - oldest) as usize)
            .copied()
            .collect();
        Ok(Chunk {
            pane,
            server_pid: std::process::id(),
            generation: self.generation,
            start,
            next: self.end,
            dropped: start - after,
            complete,
            bytes_base64: STANDARD.encode(bytes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_bytes_and_cursors_across_chunks() {
        let mut output = Output::default();
        output.append(b"\x1b[31m\xff\xe4");
        let first = output.read(7, Some(0), false).unwrap();
        assert_eq!(
            STANDARD.decode(&first.bytes_base64).unwrap(),
            b"\x1b[31m\xff\xe4"
        );
        output.append(b"\xb8\xad");
        let second = output.read(7, Some(first.next), true).unwrap();
        assert_eq!(STANDARD.decode(&second.bytes_base64).unwrap(), b"\xb8\xad");
        assert!(second.complete);
        assert_eq!(second.dropped, 0);
        assert!(output.read(7, Some(second.next + 1), false).is_err());
        assert!(output.read(7, None, false).unwrap().bytes_base64.is_empty());
    }
    #[test]
    fn bounded_tail_reports_loss_and_restart() {
        let mut output = Output::default();
        output.append(&vec![1; LIMIT - 3]);
        output.append(&[2; 9]);
        let chunk = output.read(1, Some(0), false).unwrap();
        assert_eq!(chunk.dropped, 6);
        assert_eq!(STANDARD.decode(chunk.bytes_base64).unwrap().len(), LIMIT);
        output.append(&vec![3; LIMIT + 10]);
        assert_eq!(output.bytes.len(), LIMIT);
        let cursor = output.end;
        output.restart();
        output.append(b"new");
        let chunk = output.read(1, Some(cursor), false).unwrap();
        assert_eq!(chunk.generation, 1);
        assert_eq!(chunk.dropped, 0);
        assert_eq!(STANDARD.decode(chunk.bytes_base64).unwrap(), b"new");
    }
}

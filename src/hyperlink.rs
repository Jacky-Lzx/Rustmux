//! Bounded OSC 8 metadata shared by cells, history and render snapshots.

use std::io::{self, Write};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub(crate) const MAX_URI_BYTES: usize = 2083;
pub(crate) const MAX_PARAMS_BYTES: usize = 256;
pub(crate) const MAX_CONTROL_BYTES: usize = 2 + MAX_PARAMS_BYTES + 1 + MAX_URI_BYTES;
const MAX_LINKS: usize = 1024;
pub(crate) const CLOSE: &[u8] = b"\x1b]8;;\x1b\\";
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Immutable hyperlink identity. Explicit child IDs are scoped to their pane;
/// anonymous openings have independent identities. Cloned cells share the link.
#[derive(Debug, PartialEq, Eq)]
pub struct Hyperlink {
    uri: String,
    child_id: Option<String>,
    emitted_id: String,
}

impl Hyperlink {
    pub fn uri(&self) -> &str {
        &self.uri
    }
    pub fn id(&self) -> &str {
        &self.emitted_id
    }

    pub(crate) fn write_open(&self, output: &mut impl Write) -> io::Result<()> {
        write!(output, "\x1b]8;id={}", self.emitted_id)?;
        write!(output, ";{}\x1b\\", self.uri)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Pool {
    pub active: Option<Arc<Hyperlink>>,
    entries: Vec<Arc<Hyperlink>>,
}

impl Pool {
    pub fn apply(&mut self, payload: &[u8]) {
        // Every attempted open first closes the preceding link, including invalid
        // commands. No rejected payload can accidentally prolong a stale link.
        self.active = None;
        let Some(separator) = payload.iter().position(|&b| b == b';') else {
            return;
        };
        let (parameters, uri) = (&payload[..separator], &payload[separator + 1..]);
        if uri.is_empty() {
            return;
        }
        if parameters.len() > MAX_PARAMS_BYTES
            || uri.len() > MAX_URI_BYTES
            || !parameters.iter().all(|b| (0x21..=0x7e).contains(b))
            || !uri.iter().all(|b| (0x21..=0x7e).contains(b))
        {
            return;
        }
        let parameters = std::str::from_utf8(parameters).expect("ASCII parameters");
        let uri = std::str::from_utf8(uri).expect("ASCII URI");
        let mut child_id = None;
        let mut saw_id = false;
        for field in parameters.split(':').filter(|p| !p.is_empty()) {
            let Some((key, value)) = field.split_once('=') else {
                return;
            };
            if key.is_empty() {
                return;
            }
            if key == "id" {
                if saw_id {
                    return;
                }
                saw_id = true;
                child_id = (!value.is_empty()).then_some(value);
            }
        }
        if let Some(id) = child_id
            && let Some(existing) = self
                .entries
                .iter()
                .find(|link| link.child_id.as_deref() == Some(id) && link.uri == uri)
        {
            self.active = Some(Arc::clone(existing));
            return;
        }
        if self.entries.len() == MAX_LINKS {
            self.entries.retain(|link| Arc::strong_count(link) > 1);
            if self.entries.len() == MAX_LINKS {
                return;
            }
        }
        let link = Arc::new(Hyperlink {
            uri: uri.to_owned(),
            child_id: child_id.map(str::to_owned),
            emitted_id: format!(
                "rmx-{:x}-{:x}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ),
        });
        self.entries.push(Arc::clone(&link));
        self.active = Some(link);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_live_pool_rejects_then_recovers_after_cells_release_links() {
        let mut pool = Pool::default();
        let mut cells = Vec::new();
        for _ in 0..MAX_LINKS {
            pool.apply(b";https://example.test");
            cells.push(pool.active.clone().unwrap());
        }
        pool.apply(b";https://example.test/rejected");
        assert!(pool.active.is_none());
        assert_eq!(cells[0].uri(), "https://example.test");
        cells.clear();
        pool.apply(b";https://example.test/recovered");
        assert_eq!(
            pool.active.as_ref().unwrap().uri(),
            "https://example.test/recovered"
        );
        assert_eq!(pool.entries.len(), 1);
    }
}

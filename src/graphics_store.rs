//! Bounded image-data ownership for one pane. This does not model placements.

use crate::graphics_transfer::AssembledDirectTransfer;
use std::collections::{BTreeMap, VecDeque};

pub const MAX_PANE_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PANE_IMAGES: usize = 256;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ImageFormat {
    Rgb,
    Rgba,
    Png,
}

#[derive(Debug, Eq, PartialEq)]
pub struct StoredImage {
    pub format: ImageFormat,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StoreError {
    UnsupportedIdentity,
    UnsupportedAction,
    TooLarge,
}

#[derive(Debug, Default)]
pub struct ImageStore {
    images: BTreeMap<u32, StoredImage>,
    oldest: VecDeque<u32>,
    total_bytes: usize,
}

impl ImageStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: u32) -> Option<&StoredImage> {
        self.images.get(&id)
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Retain a completed transfer with an explicit nonzero image ID. Query
    /// commands and image-number allocation require a later protocol stage.
    /// Replacement is atomic if the new transfer cannot fit by itself.
    pub fn insert(&mut self, transfer: AssembledDirectTransfer) -> Result<u32, StoreError> {
        if !matches!(transfer.control(b'a'), None | Some(b"t" | b"T")) {
            return Err(StoreError::UnsupportedAction);
        }
        if transfer.control(b'I').is_some() {
            return Err(StoreError::UnsupportedIdentity);
        }
        let id = transfer
            .control(b'i')
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|&id| id != 0)
            .ok_or(StoreError::UnsupportedIdentity)?;
        let format = match transfer.control(b'f') {
            None | Some(b"32") => ImageFormat::Rgba,
            Some(b"24") => ImageFormat::Rgb,
            Some(b"100") => ImageFormat::Png,
            _ => return Err(StoreError::UnsupportedAction),
        };
        let size = transfer.data.len();
        if size > MAX_PANE_IMAGE_BYTES {
            return Err(StoreError::TooLarge);
        }
        self.remove(id);
        while self.images.len() >= MAX_PANE_IMAGES || self.total_bytes + size > MAX_PANE_IMAGE_BYTES
        {
            let oldest = self.oldest.pop_front().expect("nonempty image store");
            let removed = self.images.remove(&oldest).expect("indexed image exists");
            self.total_bytes -= removed.data.len();
        }
        self.total_bytes += size;
        self.oldest.push_back(id);
        self.images.insert(
            id,
            StoredImage {
                format,
                data: transfer.data,
            },
        );
        Ok(id)
    }

    /// Explicit data removal. This is not yet a Kitty `a=d` operation: the
    /// protocol's lowercase/uppercase forms depend on placement references.
    pub fn remove(&mut self, id: u32) -> Option<StoredImage> {
        let removed = self.images.remove(&id)?;
        self.total_bytes -= removed.data.len();
        self.oldest.retain(|&entry| entry != id);
        Some(removed)
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.oldest.clear();
        self.total_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics_transfer::DirectTransferAssembler;

    fn transfer(command: &[u8]) -> AssembledDirectTransfer {
        DirectTransferAssembler::new().accept(command).unwrap()
    }

    #[test]
    fn replacement_and_explicit_removal_account_for_bytes() {
        let mut store = ImageStore::new();
        assert_eq!(
            store.insert(transfer(b"\x1b_Ga=T,f=100,i=7;QUJD\x1b\\")),
            Ok(7)
        );
        assert_eq!(store.total_bytes(), 3);
        assert_eq!(
            store.insert(transfer(b"\x1b_Ga=t,f=100,i=7;RA==\x1b\\")),
            Ok(7)
        );
        assert_eq!(store.len(), 1);
        assert_eq!(store.get(7).unwrap().data, b"D");
        assert_eq!(store.total_bytes(), 1);
        assert_eq!(store.remove(7).unwrap().data, b"D");
        assert!(store.is_empty());
        assert_eq!(store.total_bytes(), 0);
    }

    #[test]
    fn unsupported_identity_and_query_do_not_mutate_store() {
        let mut store = ImageStore::new();
        for command in [
            b"\x1b_Ga=t,f=100;QQ==\x1b\\".as_slice(),
            b"\x1b_Ga=t,f=100,i=0;QQ==\x1b\\",
            b"\x1b_Ga=t,f=100,i=7,I=9;QQ==\x1b\\",
        ] {
            assert_eq!(
                store.insert(transfer(command)),
                Err(StoreError::UnsupportedIdentity)
            );
        }
        assert_eq!(
            store.insert(transfer(b"\x1b_Ga=q,f=100,i=7;QQ==\x1b\\")),
            Err(StoreError::UnsupportedAction)
        );
        assert!(store.is_empty());
    }

    #[test]
    fn oldest_images_are_evicted_at_count_limit() {
        let mut store = ImageStore::new();
        for id in 1..=MAX_PANE_IMAGES + 1 {
            let command = format!("\x1b_Ga=t,f=100,i={id};QQ==\x1b\\");
            store.insert(transfer(command.as_bytes())).unwrap();
        }
        assert_eq!(store.len(), MAX_PANE_IMAGES);
        assert_eq!(store.total_bytes(), MAX_PANE_IMAGES);
        assert!(store.get(1).is_none());
        assert!(store.get(2).is_some());
        store.clear();
        assert!(store.is_empty());
    }
}

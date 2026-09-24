//! Bounded image data and placement references for one pane. Optional cell
//! anchors are recorded, but no pixels are rendered here.

use crate::{graphics::MAX_GRAPHICS_COMMAND_BYTES, graphics_transfer::AssembledDirectTransfer};
use std::collections::{BTreeMap, VecDeque};

pub const MAX_PANE_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PANE_IMAGES: usize = 256;
pub const MAX_PANE_PLACEMENTS: usize = 1024;

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
    pub(crate) declared_width: Option<u32>,
    pub(crate) declared_height: Option<u32>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StoreError {
    UnsupportedIdentity,
    UnsupportedAction,
    InvalidPlacement,
    MissingImage,
    TooLarge,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Placement {
    pub image_id: u32,
    /// None is an anonymous placement (the protocol's absent or zero `p`).
    pub placement_id: Option<u32>,
    /// Only pane-local, cursor-anchored placements have geometry so far.
    pub geometry: Option<PlacementGeometry>,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct CellAnchor {
    pub row: usize,
    pub column: usize,
    pub alternate: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PlacementGeometry {
    pub anchor: CellAnchor,
    /// Explicit cell extent, when requested. Missing values need pixel-cell
    /// geometry before a renderer can infer them.
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub z_index: i32,
    pub cursor_stays: bool,
}

#[derive(Debug, Default)]
pub struct ImageStore {
    images: BTreeMap<u32, StoredImage>,
    oldest: VecDeque<u32>,
    placements: VecDeque<Placement>,
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

    pub fn placements(&self) -> impl Iterator<Item = &Placement> {
        self.placements.iter()
    }

    /// Clear visible, cursor-anchored references on one screen without
    /// discarding their image data. Unanchored virtual/relative references are
    /// not classified as visible until their layout is modeled.
    pub fn clear_screen_placements(&mut self, alternate: bool) {
        self.placements.retain(|placement| {
            placement
                .geometry
                .is_none_or(|geometry| geometry.anchor.alternate != alternate)
        });
    }

    /// Retain a completed transfer with an explicit nonzero image ID. Query
    /// commands and image-number allocation require a later protocol stage.
    /// Replacement is atomic if the new transfer cannot fit by itself.
    pub fn insert(&mut self, transfer: AssembledDirectTransfer) -> Result<u32, StoreError> {
        self.insert_inner(transfer, None)
    }

    /// Pane path: snapshot the cursor at the final chunk of `a=T`.
    pub fn insert_at(
        &mut self,
        transfer: AssembledDirectTransfer,
        anchor: CellAnchor,
    ) -> Result<u32, StoreError> {
        self.insert_inner(transfer, Some(anchor))
    }

    fn insert_inner(
        &mut self,
        transfer: AssembledDirectTransfer,
        anchor: Option<CellAnchor>,
    ) -> Result<u32, StoreError> {
        let display = match transfer.control(b'a') {
            None | Some(b"t") => false,
            Some(b"T") => true,
            _ => return Err(StoreError::UnsupportedAction),
        };
        let placement_id = if display {
            parse_optional_placement_id(transfer.control(b'p'))?
        } else {
            None
        };
        let geometry = if display
            && transfer.control(b'U') != Some(b"1")
            && transfer.control(b'P').is_none()
            && transfer.control(b'Q').is_none()
        {
            let parsed = parse_geometry(anchor.unwrap_or_default(), |key| transfer.control(key))?;
            anchor.map(|_| parsed)
        } else {
            None
        };
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
        let declared_width = transfer.control(b's').and_then(parse_positive_u32);
        let declared_height = transfer.control(b'v').and_then(parse_positive_u32);
        if size > MAX_PANE_IMAGE_BYTES {
            return Err(StoreError::TooLarge);
        }
        self.remove(id);
        while self.images.len() >= MAX_PANE_IMAGES || self.total_bytes + size > MAX_PANE_IMAGE_BYTES
        {
            let oldest = *self.oldest.front().expect("nonempty image store");
            self.remove(oldest);
        }
        self.total_bytes += size;
        self.oldest.push_back(id);
        self.images.insert(
            id,
            StoredImage {
                format,
                data: transfer.data,
                declared_width,
                declared_height,
            },
        );
        if display {
            self.place_with_geometry(id, placement_id, geometry)?;
        }
        Ok(id)
    }

    /// Record only an explicit-ID placement reference without an anchor.
    /// Cursor movement and acknowledgements are not modeled.
    pub fn place(&mut self, image_id: u32, placement_id: Option<u32>) -> Result<(), StoreError> {
        self.place_with_geometry(image_id, placement_id, None)
    }

    fn place_with_geometry(
        &mut self,
        image_id: u32,
        placement_id: Option<u32>,
        geometry: Option<PlacementGeometry>,
    ) -> Result<(), StoreError> {
        if placement_id == Some(0) {
            return Err(StoreError::InvalidPlacement);
        }
        if !self.images.contains_key(&image_id) {
            return Err(StoreError::MissingImage);
        }
        if let Some(placement_id) = placement_id {
            self.placements.retain(|placement| {
                placement.image_id != image_id || placement.placement_id != Some(placement_id)
            });
        }
        if self.placements.len() >= MAX_PANE_PLACEMENTS {
            self.placements.pop_front();
        }
        self.placements.push_back(Placement {
            image_id,
            placement_id,
            geometry,
        });
        Ok(())
    }

    /// A strict, deliberately small APC G control-command subset: `a=p`
    /// with `i`/`p`, and `a=d,d=i/I` with `i` and optional `p`.
    pub fn accept_control(&mut self, command: &[u8]) -> Result<(), StoreError> {
        self.accept_control_inner(command, None)
    }

    /// Pane path: snapshot the cursor when an `a=p` command arrives.
    pub fn accept_control_at(
        &mut self,
        command: &[u8],
        anchor: CellAnchor,
    ) -> Result<(), StoreError> {
        self.accept_control_inner(command, Some(anchor))
    }

    fn accept_control_inner(
        &mut self,
        command: &[u8],
        anchor: Option<CellAnchor>,
    ) -> Result<(), StoreError> {
        let controls = parse_control_command(command).ok_or(StoreError::UnsupportedAction)?;
        match controls.get(&b'a').map(Vec::as_slice) {
            Some(b"p") => {
                if !only_keys(&controls, b"aipqcrzC") {
                    return Err(StoreError::UnsupportedAction);
                }
                let id = required_id(&controls)?;
                let placement_id =
                    parse_optional_placement_id(controls.get(&b'p').map(Vec::as_slice))?;
                let parsed = parse_geometry(anchor.unwrap_or_default(), |key| {
                    controls.get(&key).map(Vec::as_slice)
                })?;
                let geometry = anchor.map(|_| parsed);
                self.place_with_geometry(id, placement_id, geometry)
            }
            Some(b"d") => {
                if !only_keys(&controls, b"adipq") {
                    return Err(StoreError::UnsupportedAction);
                }
                let id = required_id(&controls)?;
                let placement_id =
                    parse_optional_placement_id(controls.get(&b'p').map(Vec::as_slice))?;
                match controls.get(&b'd').map(Vec::as_slice) {
                    Some(b"i") => self.delete_placements(id, placement_id, false),
                    Some(b"I") => self.delete_placements(id, placement_id, true),
                    _ => return Err(StoreError::UnsupportedAction),
                }
                Ok(())
            }
            _ => Err(StoreError::UnsupportedAction),
        }
    }

    fn delete_placements(&mut self, image_id: u32, placement_id: Option<u32>, free_data: bool) {
        self.placements.retain(|placement| {
            placement.image_id != image_id
                || placement_id.is_some_and(|id| placement.placement_id != Some(id))
        });
        if free_data
            && !self
                .placements
                .iter()
                .any(|placement| placement.image_id == image_id)
        {
            self.remove(image_id);
        }
    }

    /// Explicit data removal, including its placement references.
    pub fn remove(&mut self, id: u32) -> Option<StoredImage> {
        let removed = self.images.remove(&id)?;
        self.total_bytes -= removed.data.len();
        self.oldest.retain(|&entry| entry != id);
        self.placements.retain(|placement| placement.image_id != id);
        Some(removed)
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.oldest.clear();
        self.placements.clear();
        self.total_bytes = 0;
    }
}

type Controls = BTreeMap<u8, Vec<u8>>;

fn parse_control_command(command: &[u8]) -> Option<Controls> {
    if command.len() > MAX_GRAPHICS_COMMAND_BYTES {
        return None;
    }
    let body = if let Some(bytes) = command.strip_prefix(b"\x1b_G") {
        bytes.strip_suffix(b"\x1b\\")?
    } else {
        let bytes = command.strip_prefix(&[0x9f, b'G'])?;
        bytes
            .strip_suffix(&[0x9c])
            .or_else(|| bytes.strip_suffix(b"\x1b\\"))?
    };
    let body = body.strip_suffix(b";").unwrap_or(body);
    if body.is_empty() || body.contains(&b';') {
        return None;
    }
    let mut controls = Controls::new();
    for pair in body.split(|&byte| byte == b',') {
        let equals = pair.iter().position(|&byte| byte == b'=')?;
        let (key, with_equals) = pair.split_at(equals);
        let value = &with_equals[1..];
        if key.len() != 1
            || !key[0].is_ascii_alphabetic()
            || value.is_empty()
            || !value.iter().all(u8::is_ascii_graphic)
            || controls.insert(key[0], value.to_vec()).is_some()
        {
            return None;
        }
    }
    if controls
        .get(&b'q')
        .is_some_and(|q| !matches!(q.as_slice(), b"0" | b"1" | b"2"))
    {
        return None;
    }
    Some(controls)
}

fn only_keys(controls: &Controls, allowed: &[u8]) -> bool {
    controls.keys().all(|key| allowed.contains(key))
}

fn required_id(controls: &Controls) -> Result<u32, StoreError> {
    controls
        .get(&b'i')
        .and_then(|bytes| parse_positive_u32(bytes))
        .ok_or(StoreError::UnsupportedIdentity)
}

fn parse_optional_placement_id(value: Option<&[u8]>) -> Result<Option<u32>, StoreError> {
    value
        .map(|bytes| {
            let id = parse_u32(bytes).ok_or(StoreError::InvalidPlacement)?;
            Ok((id != 0).then_some(id))
        })
        .transpose()
        .map(Option::flatten)
}

fn parse_positive_u32(bytes: &[u8]) -> Option<u32> {
    parse_u32(bytes).filter(|&value| value != 0)
}

fn parse_u32(bytes: &[u8]) -> Option<u32> {
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse::<u32>().ok()
}

fn parse_geometry<'a>(
    anchor: CellAnchor,
    control: impl Fn(u8) -> Option<&'a [u8]>,
) -> Result<PlacementGeometry, StoreError> {
    let extent = |key| {
        control(key)
            .map(|bytes| parse_u32(bytes).ok_or(StoreError::InvalidPlacement))
            .transpose()
            .map(|value| value.filter(|&number| number != 0))
    };
    let z_index = control(b'z')
        .map(|bytes| {
            std::str::from_utf8(bytes)
                .ok()
                .and_then(|value| value.parse::<i32>().ok())
                .ok_or(StoreError::InvalidPlacement)
        })
        .transpose()?
        .unwrap_or(0);
    let cursor_stays = match control(b'C') {
        None | Some(b"0") => false,
        Some(b"1") => true,
        _ => return Err(StoreError::InvalidPlacement),
    };
    Ok(PlacementGeometry {
        anchor,
        columns: extent(b'c')?,
        rows: extent(b'r')?,
        z_index,
        cursor_stays,
    })
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

    #[test]
    fn transmit_and_put_track_anonymous_and_named_references() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=T,f=100,i=7,p=9;QQ==\x1b\\"))
            .unwrap();
        assert_eq!(
            store.placements().copied().collect::<Vec<_>>(),
            [Placement {
                image_id: 7,
                placement_id: Some(9),
                geometry: None,
            }]
        );
        store.accept_control(b"\x1b_Ga=p,i=7,p=9\x1b\\").unwrap();
        assert_eq!(store.placements().count(), 1);
        store.accept_control(b"\x1b_Ga=p,i=7,p=0\x1b\\").unwrap();
        store.accept_control(b"\x1b_Ga=p,i=7\x1b\\").unwrap();
        assert_eq!(store.placements().count(), 3);
        assert_eq!(
            store
                .placements()
                .filter(|placement| placement.placement_id.is_none())
                .count(),
            2
        );
        store
            .insert(transfer(b"\x1b_Ga=t,f=100,i=7;Qg==\x1b\\"))
            .unwrap();
        assert_eq!(store.placements().count(), 0);
        assert_eq!(store.get(7).unwrap().data, b"B");
    }

    #[test]
    fn soft_and_hard_id_deletion_have_distinct_data_lifetimes() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=T,f=100,i=7,p=1;QQ==\x1b\\"))
            .unwrap();
        store.place(7, Some(2)).unwrap();
        store
            .accept_control(b"\x1b_Ga=d,d=I,i=7,p=1\x1b\\")
            .unwrap();
        assert!(store.get(7).is_some());
        assert_eq!(store.placements().count(), 1);
        store
            .accept_control(b"\x1b_Ga=d,d=i,i=7,p=2\x1b\\")
            .unwrap();
        assert!(store.get(7).is_some());
        assert_eq!(store.placements().count(), 0);
        store.accept_control(b"\x1b_Ga=d,d=I,i=7\x1b\\").unwrap();
        assert!(store.is_empty());
        assert_eq!(store.total_bytes(), 0);
    }

    #[test]
    fn invalid_controls_and_missing_images_do_not_change_references() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=t,f=100,i=7;QQ==\x1b\\"))
            .unwrap();
        assert_eq!(
            store.accept_control(b"\x1b_Ga=p,i=8,p=2\x1b\\"),
            Err(StoreError::MissingImage)
        );
        assert_eq!(store.place(7, Some(0)), Err(StoreError::InvalidPlacement));
        for command in [
            b"\x1b_Ga=p,i=7,p=no\x1b\\".as_slice(),
            b"\x1b_Ga=d,d=I,i=7,p=no\x1b\\",
            b"\x1b_Ga=d,d=A\x1b\\",
            b"\x1b_Ga=p,i=7,z=no\x1b\\",
            b"\x1b_Ga=d,d=I,i=7;payload\x1b\\",
        ] {
            assert!(store.accept_control(command).is_err());
        }
        assert!(store.get(7).is_some());
        assert_eq!(store.placements().count(), 0);
        store.accept_control(b"\x9fGa=p,i=7,p=2\x9c").unwrap();
        assert_eq!(store.placements().count(), 1);
        let oversized = format!(
            "\x1b_Ga=p,i=7,q={};\x1b\\",
            "0".repeat(MAX_GRAPHICS_COMMAND_BYTES)
        );
        assert_eq!(
            store.accept_control(oversized.as_bytes()),
            Err(StoreError::UnsupportedAction)
        );
        assert_eq!(store.placements().count(), 1);
    }

    #[test]
    fn evicting_image_also_removes_its_references() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=T,f=100,i=1;QQ==\x1b\\"))
            .unwrap();
        for id in 2..=MAX_PANE_IMAGES + 1 {
            let command = format!("\x1b_Ga=t,f=100,i={id};QQ==\x1b\\");
            store.insert(transfer(command.as_bytes())).unwrap();
        }
        assert!(store.get(1).is_none());
        assert_eq!(store.placements().count(), 0);
    }

    #[test]
    fn placement_references_are_bounded() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=t,f=100,i=1;QQ==\x1b\\"))
            .unwrap();
        for id in 1..=MAX_PANE_PLACEMENTS + 1 {
            store.place(1, Some(id as u32)).unwrap();
        }
        assert_eq!(store.placements().count(), MAX_PANE_PLACEMENTS);
        assert!(
            !store
                .placements()
                .any(|placement| placement.placement_id == Some(1))
        );
        assert!(
            store
                .placements()
                .any(|placement| placement.placement_id == Some(2))
        );
    }

    #[test]
    fn anchored_placement_records_explicit_cell_layout_and_replacement() {
        let mut store = ImageStore::new();
        let first = CellAnchor {
            row: 2,
            column: 3,
            alternate: false,
        };
        store
            .insert_at(
                transfer(b"\x1b_Ga=T,f=100,i=7,p=9,c=2,r=3,z=-4,C=1;QQ==\x1b\\"),
                first,
            )
            .unwrap();
        assert_eq!(
            store.placements().next().unwrap().geometry,
            Some(PlacementGeometry {
                anchor: first,
                columns: Some(2),
                rows: Some(3),
                z_index: -4,
                cursor_stays: true,
            })
        );
        let second = CellAnchor {
            row: 4,
            column: 5,
            alternate: true,
        };
        store
            .accept_control_at(b"\x1b_Ga=p,i=7,p=9,c=1,r=2,z=3\x1b\\", second)
            .unwrap();
        let placements: Vec<_> = store.placements().copied().collect();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].geometry.unwrap().anchor, second);
        assert_eq!(placements[0].geometry.unwrap().columns, Some(1));
        assert_eq!(placements[0].geometry.unwrap().rows, Some(2));
        assert_eq!(placements[0].geometry.unwrap().z_index, 3);
        assert!(!placements[0].geometry.unwrap().cursor_stays);
    }

    #[test]
    fn malformed_layout_does_not_replace_existing_image() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=t,f=100,i=7;QQ==\x1b\\"))
            .unwrap();
        let anchor = CellAnchor::default();
        assert_eq!(
            store.insert_at(transfer(b"\x1b_Ga=T,f=100,i=7,c=no;Qg==\x1b\\"), anchor,),
            Err(StoreError::InvalidPlacement)
        );
        assert_eq!(store.get(7).unwrap().data, b"A");
        assert!(store.placements().next().is_none());
    }
}

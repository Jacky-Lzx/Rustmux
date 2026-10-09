//! Render and retain Kitty image overlays for one outer-terminal attachment.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::time::{Duration, Instant};

use super::{
    decode::DecodedImage,
    geometry::CellPixelSize,
    output::{
        EncodedKittyPng, kitty_png_passthrough_len, kitty_rgb_placement_len,
        kitty_rgba_placement_len, write_kitty_png_passthrough_with_limit,
        write_kitty_rgb_placement_with_limit, write_kitty_rgba_placement_with_limit,
    },
    shared_memory_output::SharedPixels,
    snapshot::{ImageBand, SourceImagePlacement, SourceImageProgress},
};
use crate::{
    layout::{PaneId, Rect},
    pane::Pane,
    pane_set::PaneSet,
    render::frame::{FrameWriter, MAX_FRAME},
    window::WindowId,
};

const MAX_PENDING_SHM_BYTES: usize = 64 * 1024 * 1024;
const MAX_CACHED_OUTER_IMAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_CACHED_OUTER_IMAGES: usize = 8;
const MAX_CACHED_VIRTUAL_SOURCE_BYTES: usize = 32 * 1024 * 1024;

// Yazi paints virtual-image placeholders over several PTY reads. Give an
// incomplete rectangle time to finish before composing an expensive PNG.
const INCOMPLETE_VIRTUAL_QUIET: Duration = Duration::from_millis(350);

// Preferred raw tile target; an indivisible larger cell gets exact frame preflight.
const MAX_KITTY_TILE_RAW_BYTES: usize = 8 * 1024 * 1024;

// Tiny images are cheaper to send as raw RGBA than to compress on every render.
const MIN_KITTY_PNG_CANDIDATE_BYTES: usize = 256 * 1024;

/// Drop transparent pane-sized margins before encoding an outer placement.
/// Keeping the crop aligned to cells lets the cursor represent its origin
/// without introducing a separate pixel-offset protocol path.
fn crop_overlay_to_visible_cells(
    image: DecodedImage,
    cell: CellPixelSize,
) -> Option<(DecodedImage, usize, usize)> {
    let width = usize::try_from(image.width).ok()?;
    let height = usize::try_from(image.height).ok()?;
    if width == 0
        || height == 0
        || image.pixels.len() != width.checked_mul(height)?.checked_mul(4)?
    {
        return None;
    }
    let mut left = width;
    let mut top = height;
    let mut right = 0;
    let mut bottom = 0;
    for (index, pixel) in image.pixels.as_chunks::<4>().0.iter().enumerate() {
        if pixel[3] != 0 {
            let x = index % width;
            let y = index / width;
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    if left == width {
        return None;
    }
    let cell_width = usize::from(cell.width());
    let cell_height = usize::from(cell.height());
    left = left / cell_width * cell_width;
    top = top / cell_height * cell_height;
    right = right
        .div_ceil(cell_width)
        .saturating_mul(cell_width)
        .min(width);
    bottom = bottom
        .div_ceil(cell_height)
        .saturating_mul(cell_height)
        .min(height);
    let cropped_width = right - left;
    let cropped_height = bottom - top;
    if left == 0 && top == 0 && right == width && bottom == height {
        return Some((image, 0, 0));
    }
    let mut pixels = Vec::with_capacity(cropped_width * cropped_height * 4);
    for y in top..bottom {
        let start = (y * width + left) * 4;
        pixels.extend_from_slice(&image.pixels[start..start + cropped_width * 4]);
    }
    Some((
        DecodedImage {
            width: u32::try_from(cropped_width).ok()?,
            height: u32::try_from(cropped_height).ok()?,
            pixels,
        },
        left / cell_width,
        top / cell_height,
    ))
}

/// Split a large image into cell-aligned rectangles. Keeping complete cells
/// together avoids relying on Kitty's pixel-offset placement controls.
fn overlay_tile_size(image: &DecodedImage, cell: CellPixelSize) -> Option<(usize, usize)> {
    let cell_width = usize::from(cell.width());
    let cell_height = usize::from(cell.height());
    let cell_bytes = cell_width.checked_mul(cell_height)?.checked_mul(4)?;
    // A single physical cell can exceed the preferred tile target while its
    // actual encoded placement still fits a frame. The exact output preflight
    // below decides that case instead of silently dropping it here.
    let max_cells = (MAX_KITTY_TILE_RAW_BYTES / cell_bytes).max(1);
    let columns = usize::try_from(image.width).ok()?.div_ceil(cell_width);
    if columns == 0 {
        return None;
    }
    let tile_columns = columns.min(max_cells);
    let tile_rows = max_cells / tile_columns;
    Some((
        tile_columns.checked_mul(cell_width)?,
        tile_rows.checked_mul(cell_height)?,
    ))
}

fn overlay_tile(
    image: &DecodedImage,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Option<DecodedImage> {
    let source_width = usize::try_from(image.width).ok()?;
    let mut pixels = Vec::with_capacity(width.checked_mul(height)?.checked_mul(4)?);
    for row in y..y + height {
        let start = (row * source_width + x) * 4;
        pixels.extend_from_slice(&image.pixels[start..start + width * 4]);
    }
    Some(DecodedImage {
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        pixels,
    })
}

enum OverlayPayload {
    Rgb,
    Rgba,
    Png(EncodedKittyPng),
}

fn prepare_overlay_payload(
    image: &DecodedImage,
    image_id: u32,
    z_index: i32,
) -> io::Result<(OverlayPayload, usize)> {
    let rgba_len = kitty_rgba_placement_len(image, image_id, z_index)?;
    let (raw_payload, raw_len) = match kitty_rgb_placement_len(image, image_id, z_index) {
        Ok(rgb_len) if rgb_len < rgba_len => (OverlayPayload::Rgb, rgb_len),
        _ => (OverlayPayload::Rgba, rgba_len),
    };
    if image.pixels.len() >= MIN_KITTY_PNG_CANDIDATE_BYTES
        && let Ok(png) = EncodedKittyPng::from_rgba(image)
        && let Ok(png_len) = png.placement_len(image_id, z_index)
        && png_len < raw_len
    {
        return Ok((OverlayPayload::Png(png), png_len));
    }
    Ok((raw_payload, raw_len))
}

// The outer alternate screen belongs to this attachment. Only delete IDs we
// allocated here; deleting all images could erase another client's graphics.
#[derive(Clone, Copy, Eq, PartialEq)]
struct KittyOverlay {
    window: WindowId,
    pane: PaneId,
    band: ImageBand,
    revision: u64,
    placeholder_revision: u64,
    rect: Rect,
    cell: CellPixelSize,
    alternate: bool,
    image_id: u32,
    placement_id: Option<u32>,
    column_offset: usize,
    row_offset: usize,
}

struct CachedOuterImage {
    image_id: u32,
    format: u8,
    width: u32,
    height: u32,
    generation: u64,
    size: usize,
    // Virtual preview applications may retransmit identical bytes under new IDs.
    content: Option<(u64, Vec<u8>)>,
    last_used: u64,
}

fn source_fingerprint(source: &SourceImagePlacement<'_>) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    (source.format, source.width, source.height).hash(&mut hash);
    source.data.hash(&mut hash);
    hash.finish()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OuterImageDelete {
    Image(u32),
    Placement { image_id: u32, placement_id: u32 },
}

impl KittyOverlay {
    fn deletion(self) -> OuterImageDelete {
        match self.placement_id {
            Some(placement_id) => OuterImageDelete::Placement {
                image_id: self.image_id,
                placement_id,
            },
            None => OuterImageDelete::Image(self.image_id),
        }
    }
}

impl OuterImageDelete {
    fn command(self) -> String {
        match self {
            Self::Image(image_id) => format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\"),
            Self::Placement {
                image_id,
                placement_id,
            } => format!("\x1b_Ga=d,d=i,i={image_id},p={placement_id},q=2\x1b\\"),
        }
    }
}

struct UploadWriter<'a> {
    output: &'a mut VecDeque<u8>,
    staging: bool,
}

impl Write for UploadWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.staging && bytes.starts_with(b"\x1b_Ga=T,") {
            FrameWriter(self.output).write_all(b"\x1b_Ga=t,")?;
            FrameWriter(self.output).write_all(&bytes[7..])?;
            Ok(bytes.len())
        } else {
            FrameWriter(self.output).write(bytes)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct PendingPngTile {
    overlay: KittyOverlay,
    png: EncodedKittyPng,
    placement_len: usize,
}

struct IncompleteVirtual {
    window: WindowId,
    pane: PaneId,
    revision: u64,
    placeholder_revision: u64,
    rect: Rect,
    cell: CellPixelSize,
    alternate: bool,
    last_change: Instant,
    waiting: bool,
}

pub(crate) struct KittyOverlays {
    entries: Vec<KittyOverlay>,
    retired: Vec<KittyOverlay>,
    staged_placements: Vec<(KittyOverlay, Vec<u8>)>,
    pending_delete: VecDeque<OuterImageDelete>,
    cached_images: Vec<CachedOuterImage>,
    pending_png: Option<PendingPngTile>,
    incomplete_virtual: Vec<IncompleteVirtual>,
    next_id: u32,
    next_placement_id: u32,
    cache_clock: u64,
    pending_retry: bool,
    pending_shm: Vec<SharedPixels>,
    shm_supported: bool,
}

impl Default for KittyOverlays {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            retired: Vec::new(),
            staged_placements: Vec::new(),
            pending_delete: VecDeque::new(),
            cached_images: Vec::new(),
            pending_png: None,
            incomplete_virtual: Vec::new(),
            next_id: 0x8000_0000,
            next_placement_id: 1,
            cache_clock: 0,
            pending_retry: false,
            pending_shm: Vec::new(),
            shm_supported: false,
        }
    }
}

impl KittyOverlays {
    pub(crate) fn shared_memory_supported(&self) -> bool {
        self.shm_supported
    }

    pub(crate) fn enable_shared_memory(&mut self, output: &mut VecDeque<u8>) -> io::Result<()> {
        self.clear(output)?;
        self.shm_supported = true;
        Ok(())
    }

    pub(crate) fn needs_retry(&self) -> bool {
        self.pending_retry
    }

    pub(crate) fn invalidate_cached_image(&mut self, image_id: u32) -> bool {
        let Some(index) = self
            .cached_images
            .iter()
            .position(|image| image.image_id == image_id)
        else {
            return false;
        };
        self.cached_images.remove(index);
        self.entries.retain(|entry| entry.image_id != image_id);
        self.retired.retain(|entry| entry.image_id != image_id);
        self.staged_placements
            .retain(|(entry, _)| entry.image_id != image_id);
        self.pending_delete
            .push_back(OuterImageDelete::Image(image_id));
        self.pending_retry = true;
        true
    }

    fn cached_source(&self, source: &SourceImagePlacement<'_>) -> Option<usize> {
        if let Some(index) = self
            .cached_images
            .iter()
            .position(|cached| cached.generation == source.generation)
        {
            return Some(index);
        }
        // Regular PDF movement never scans source bytes. Preserve content
        // deduplication only for a newly uploaded virtual preview.
        if source.geometry.is_some() {
            return None;
        }
        let fingerprint = source_fingerprint(source);
        self.cached_images.iter().position(|cached| {
            cached.format == source.format
                && cached.width == source.width
                && cached.height == source.height
                && cached
                    .content
                    .as_ref()
                    .is_some_and(|(hash, data)| *hash == fingerprint && data == source.data)
        })
    }

    fn retain_virtual_source(&self, source: &SourceImagePlacement<'_>) -> Option<(u64, Vec<u8>)> {
        let retained: usize = self
            .cached_images
            .iter()
            .filter_map(|cached| cached.content.as_ref().map(|(_, data)| data.len()))
            .sum();
        if source.geometry.is_some()
            || source.data.len() > MAX_CACHED_VIRTUAL_SOURCE_BYTES.saturating_sub(retained)
        {
            return None;
        }
        let mut data = Vec::new();
        data.try_reserve_exact(source.data.len()).ok()?;
        data.extend_from_slice(source.data);
        Some((source_fingerprint(source), data))
    }

    fn make_cache_room(&mut self, size: usize, output: &mut VecDeque<u8>) -> io::Result<bool> {
        if size > MAX_CACHED_OUTER_IMAGE_BYTES {
            return Ok(false);
        }
        let mut room_available = true;
        while self.cached_images.len() >= MAX_CACHED_OUTER_IMAGES
            || self
                .cached_images
                .iter()
                .map(|image| image.size)
                .sum::<usize>()
                > MAX_CACHED_OUTER_IMAGE_BYTES - size
        {
            let victim = self
                .cached_images
                .iter()
                .enumerate()
                .filter(|(_, image)| {
                    !self
                        .entries
                        .iter()
                        .chain(&self.retired)
                        .any(|entry| entry.image_id == image.image_id)
                })
                .min_by_key(|(_, image)| image.last_used)
                .map(|(index, _)| index);
            let Some(victim) = victim else {
                room_available = false;
                break;
            };
            let image = self.cached_images.remove(victim);
            self.pending_delete
                .push_back(OuterImageDelete::Image(image.image_id));
        }
        self.flush_deletes(output)?;
        if !self.pending_delete.is_empty() {
            self.pending_retry = true;
            return Ok(false);
        }
        Ok(room_available)
    }

    fn defer_incomplete_virtual(&mut self, candidate: IncompleteVirtual) -> bool {
        let now = candidate.last_change;
        if let Some(state) = self
            .incomplete_virtual
            .iter_mut()
            .find(|state| state.window == candidate.window && state.pane == candidate.pane)
        {
            if state.revision != candidate.revision
                || state.placeholder_revision != candidate.placeholder_revision
                || state.rect != candidate.rect
                || state.cell != candidate.cell
                || state.alternate != candidate.alternate
            {
                *state = candidate;
            }
            if state.waiting && now.duration_since(state.last_change) >= INCOMPLETE_VIRTUAL_QUIET {
                state.waiting = false;
            }
            return state.waiting;
        }
        self.incomplete_virtual.push(candidate);
        true
    }

    pub(crate) fn next_deferred_retry(&self) -> Option<Instant> {
        self.incomplete_virtual
            .iter()
            .filter(|state| state.waiting)
            .map(|state| state.last_change + INCOMPLETE_VIRTUAL_QUIET)
            .min()
    }

    pub(crate) fn render(
        &mut self,
        window: WindowId,
        panes: &PaneSet<Pane>,
        cell: CellPixelSize,
        outer_rows: u16,
        cursor: (usize, usize),
        output: &mut VecDeque<u8>,
    ) -> io::Result<()> {
        self.pending_shm.retain(|object| !object.consumed());
        let retry_missing = self.pending_retry;
        self.pending_retry = false;
        let visible = panes.layout().content_geometry().panes;
        self.incomplete_virtual.retain(|state| {
            state.window == window
                && state.cell == cell
                && visible.iter().any(|(id, rect)| {
                    *id == state.pane
                        && *rect == state.rect
                        && panes.get(*id).is_some_and(|pane| {
                            pane.image_store().revision() == state.revision
                                && pane.virtual_placeholder_revision() == state.placeholder_revision
                                && pane.screen().is_alternate() == state.alternate
                                && pane.image_store().placements().next().is_some()
                        })
                })
        });
        // A deferred PNG belongs to the same scene revision as its tile. Drop
        // it before considering output if that scene has changed meanwhile.
        if self.pending_png.as_ref().is_some_and(|pending| {
            let cached = pending.overlay;
            cached.window != window
                || cached.cell != cell
                || !visible.iter().any(|(id, rect)| {
                    *id == cached.pane
                        && *rect == cached.rect
                        && panes.get(*id).is_some_and(|pane| {
                            pane.image_store().revision() == cached.revision
                                && pane.virtual_placeholder_revision()
                                    == cached.placeholder_revision
                                && pane.screen().is_alternate() == cached.alternate
                        })
                })
        }) {
            self.pending_png = None;
        }
        // A replacement may span several bounded output frames. Keep the
        // displayed scene while its new tiles are uploaded without placements.
        // Geometry changes and genuine clears must still remove old images.
        let can_retain = |old: &KittyOverlay| {
            old.window == window
                && old.cell == cell
                && visible
                    .iter()
                    .any(|(id, rect)| *id == old.pane && *rect == old.rect)
                && panes.get(old.pane).is_some_and(|pane| {
                    pane.screen().is_alternate() == old.alternate
                        && pane.image_store().placements().any(|placement| {
                            placement.geometry.is_some_and(|geometry| {
                                geometry.anchor.alternate == old.alternate
                                    && old.band.contains(geometry.z_index)
                            })
                        })
                })
        };
        for old in std::mem::take(&mut self.retired) {
            if can_retain(&old) {
                self.retired.push(old);
            } else {
                self.pending_delete.push_back(old.deletion());
            }
        }
        let mut moved_cursor = false;
        for old in std::mem::take(&mut self.entries) {
            let current = visible.iter().find(|(id, _)| *id == old.pane);
            let unchanged = old.window == window
                && current.is_some_and(|(_, rect)| *rect == old.rect)
                && panes.get(old.pane).is_some_and(|pane| {
                    pane.image_store().revision() == old.revision
                        && pane.virtual_placeholder_revision() == old.placeholder_revision
                        && pane.screen().is_alternate() == old.alternate
                })
                && old.cell == cell;
            if unchanged {
                self.entries.push(old);
            } else {
                let staged = self
                    .staged_placements
                    .iter()
                    .any(|(entry, _)| *entry == old);
                self.staged_placements.retain(|(entry, _)| *entry != old);
                if !staged && can_retain(&old) {
                    self.retired.push(old);
                } else {
                    self.pending_delete.push_back(old.deletion());
                }
            }
        }
        let staging = !self.retired.is_empty() || !self.staged_placements.is_empty();
        self.flush_deletes(output)?;
        if !self.pending_delete.is_empty() {
            return Ok(());
        }
        for (id, rect) in visible {
            if !retry_missing
                && self
                    .entries
                    .iter()
                    .any(|entry| entry.window == window && entry.pane == id)
            {
                continue;
            }
            let pane = panes.get(id).expect("visible pane has content");
            if pane.image_store().placements().next().is_none() {
                continue;
            }
            let row = usize::from(outer_rows > 1) + usize::from(rect.row) + 1;
            let column = usize::from(rect.column) + 1;
            let restore = format!("\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1);
            for band in ImageBand::ALL {
                let source_progress = pane.source_image_band_progress(cell, band);
                if matches!(&source_progress, Some(SourceImageProgress::Incomplete)) {
                    if self.defer_incomplete_virtual(IncompleteVirtual {
                        window,
                        pane: id,
                        revision: pane.image_store().revision(),
                        placeholder_revision: pane.virtual_placeholder_revision(),
                        rect,
                        cell,
                        alternate: pane.screen().is_alternate(),
                        last_change: Instant::now(),
                        waiting: true,
                    }) {
                        continue;
                    }
                } else if band == ImageBand::AboveText {
                    self.incomplete_virtual
                        .retain(|state| state.window != window || state.pane != id);
                }
                if self.shm_supported
                    && let Some(SourceImageProgress::Complete(raw)) = &source_progress
                {
                    let position = format!("\x1b[{};{}H", row + raw.row, column + raw.column);
                    if self.entries.iter().any(|entry| {
                        entry.window == window && entry.pane == id && entry.band == band
                    }) {
                        continue;
                    }
                    if let Some(cached) = self.cached_source(raw)
                        && let Some(next_placement_id) = self.next_placement_id.checked_add(1)
                    {
                        let image_id = self.cached_images[cached].image_id;
                        let placement_id = self.next_placement_id;
                        let command = format!(
                            "\x1b_Ga=p,i={image_id},p={placement_id}{},z={},C=1,q=1\x1b\\",
                            raw.placement_controls(),
                            band.output_z()
                        )
                        .into_bytes();
                        if position.len() + command.len() + restore.len() > MAX_FRAME - output.len()
                        {
                            self.pending_retry = true;
                            continue;
                        }
                        let overlay = KittyOverlay {
                            window,
                            pane: id,
                            band,
                            revision: pane.image_store().revision(),
                            placeholder_revision: pane.virtual_placeholder_revision(),
                            rect,
                            cell,
                            alternate: pane.screen().is_alternate(),
                            image_id,
                            placement_id: Some(placement_id),
                            column_offset: raw.column,
                            row_offset: raw.row,
                        };
                        if staging {
                            let mut display = position.into_bytes();
                            display.extend_from_slice(&command);
                            self.staged_placements.push((overlay, display));
                        } else {
                            FrameWriter(output).write_all(position.as_bytes())?;
                            FrameWriter(output).write_all(&command)?;
                            moved_cursor = true;
                        }
                        self.next_placement_id = next_placement_id;
                        self.cache_clock = self.cache_clock.wrapping_add(1);
                        self.cached_images[cached].last_used = self.cache_clock;
                        self.cached_images[cached].generation = raw.generation;
                        self.entries.push(overlay);
                        continue;
                    }
                    let image_id = self.next_id;
                    if let Some(next_id) = image_id.checked_add(1) {
                        let retained: usize = self.pending_shm.iter().map(SharedPixels::len).sum();
                        if raw.data.len() <= MAX_PENDING_SHM_BYTES.saturating_sub(retained)
                            && let Ok(object) = SharedPixels::create(raw.data)
                        {
                            let next_placement_id = self.next_placement_id.checked_add(1);
                            let mut cache_source = next_placement_id.is_some()
                                && raw.data.len() <= MAX_CACHED_OUTER_IMAGE_BYTES;
                            let mut command = object.source_command(
                                raw,
                                image_id,
                                cache_source.then_some(self.next_placement_id),
                                band.output_z(),
                            );
                            if position.len() + command.len() + restore.len()
                                > MAX_FRAME - output.len()
                            {
                                self.pending_retry = true;
                                continue;
                            }
                            if cache_source && !self.make_cache_room(raw.data.len(), output)? {
                                if self.pending_retry {
                                    continue;
                                }
                                cache_source = false;
                                command =
                                    object.source_command(raw, image_id, None, band.output_z());
                            }
                            if position.len() + command.len() + restore.len()
                                > MAX_FRAME - output.len()
                            {
                                self.pending_retry = true;
                                continue;
                            }
                            let placement_id = cache_source.then_some(self.next_placement_id);
                            let overlay = KittyOverlay {
                                window,
                                pane: id,
                                band,
                                revision: pane.image_store().revision(),
                                placeholder_revision: pane.virtual_placeholder_revision(),
                                rect,
                                cell,
                                alternate: pane.screen().is_alternate(),
                                image_id,
                                placement_id,
                                column_offset: raw.column,
                                row_offset: raw.row,
                            };
                            FrameWriter(output).write_all(position.as_bytes())?;
                            UploadWriter { output, staging }.write_all(&command)?;
                            if staging {
                                let placement =
                                    placement_id.map_or_else(String::new, |id| format!(",p={id}"));
                                let display = format!(
                                    "{position}\x1b_Ga=p,i={image_id}{placement}{},z={},C=1,q=1\x1b\\",
                                    raw.placement_controls(),
                                    band.output_z()
                                );
                                self.staged_placements.push((overlay, display.into_bytes()));
                            }
                            moved_cursor = true;
                            self.next_id = next_id;
                            self.entries.push(overlay);
                            self.pending_shm.push(object);
                            if cache_source {
                                self.next_placement_id =
                                    next_placement_id.expect("cache ID checked");
                                self.cache_clock = self.cache_clock.wrapping_add(1);
                                let content = self.retain_virtual_source(raw);
                                self.cached_images.push(CachedOuterImage {
                                    image_id,
                                    format: raw.format,
                                    width: raw.width,
                                    height: raw.height,
                                    generation: raw.generation,
                                    size: raw.data.len(),
                                    content,
                                    last_used: self.cache_clock,
                                });
                            }
                            continue;
                        }
                    }
                }
                if let Some(SourceImageProgress::Complete(source)) = &source_progress
                    && source.format == 100
                    && source.geometry.is_none()
                    && let Some(next_id) = self.next_id.checked_add(1)
                    && !self.entries.iter().any(|entry| {
                        entry.window == window && entry.pane == id && entry.band == band
                    })
                {
                    let image_id = self.next_id;
                    let position = format!("\x1b[{};{}H", row + source.row, column + source.column);
                    if let Ok(payload_len) =
                        kitty_png_passthrough_len(source.data, image_id, band.output_z())
                    {
                        let frame_len = position.len() + payload_len + restore.len();
                        if frame_len <= MAX_FRAME {
                            if frame_len > MAX_FRAME - output.len() {
                                self.pending_retry = true;
                                continue;
                            }
                            let overlay = KittyOverlay {
                                window,
                                pane: id,
                                band,
                                revision: pane.image_store().revision(),
                                placeholder_revision: pane.virtual_placeholder_revision(),
                                rect,
                                cell,
                                alternate: pane.screen().is_alternate(),
                                image_id,
                                placement_id: None,
                                column_offset: source.column,
                                row_offset: source.row,
                            };
                            FrameWriter(output).write_all(position.as_bytes())?;
                            write_kitty_png_passthrough_with_limit(
                                source.data,
                                image_id,
                                band.output_z(),
                                MAX_FRAME - output.len() - restore.len(),
                                &mut UploadWriter { output, staging },
                            )?;
                            if staging {
                                self.stage_placement(overlay, &position, None);
                            }
                            moved_cursor = true;
                            self.next_id = next_id;
                            self.entries.push(overlay);
                            continue;
                        }
                    }
                }
                // A malformed or over-budget band must not suppress the other
                // two or take down the terminal session.
                let Ok(Some(image)) = pane.compose_image_band(cell, band) else {
                    continue;
                };
                let Some((image, column_offset, row_offset)) =
                    crop_overlay_to_visible_cells(image, cell)
                else {
                    continue;
                };
                let Some((tile_width, tile_height)) = overlay_tile_size(&image, cell) else {
                    continue;
                };
                let width = usize::try_from(image.width).expect("validated image width");
                let height = usize::try_from(image.height).expect("validated image height");
                for y in (0..height).step_by(tile_height) {
                    for x in (0..width).step_by(tile_width) {
                        let tile_column = column_offset + x / usize::from(cell.width());
                        let tile_row = row_offset + y / usize::from(cell.height());
                        if self.entries.iter().any(|entry| {
                            entry.window == window
                                && entry.pane == id
                                && entry.band == band
                                && entry.column_offset == tile_column
                                && entry.row_offset == tile_row
                        }) {
                            continue;
                        }
                        let tile_image =
                            if x == 0 && y == 0 && tile_width >= width && tile_height >= height {
                                None
                            } else {
                                let Some(tile) = overlay_tile(
                                    &image,
                                    x,
                                    y,
                                    tile_width.min(width - x),
                                    tile_height.min(height - y),
                                ) else {
                                    continue;
                                };
                                Some(tile)
                            };
                        let tile = tile_image.as_ref().unwrap_or(&image);
                        let position = format!("\x1b[{};{}H", row + tile_row, column + tile_column);
                        let image_id = self.next_id;
                        let Some(next_id) = image_id.checked_add(1) else {
                            continue;
                        };
                        let overlay = KittyOverlay {
                            window,
                            pane: id,
                            band,
                            revision: pane.image_store().revision(),
                            placeholder_revision: pane.virtual_placeholder_revision(),
                            rect,
                            cell,
                            alternate: pane.screen().is_alternate(),
                            image_id,
                            placement_id: None,
                            column_offset: tile_column,
                            row_offset: tile_row,
                        };
                        let prepared = if self
                            .pending_png
                            .as_ref()
                            .is_some_and(|pending| pending.overlay == overlay)
                        {
                            let pending = self.pending_png.take().expect("matching cached tile");
                            Ok((OverlayPayload::Png(pending.png), pending.placement_len))
                        } else {
                            prepare_overlay_payload(tile, image_id, band.output_z())
                        };
                        let Ok((payload, payload_len)) = prepared else {
                            continue;
                        };
                        let frame_len = position.len() + payload_len + restore.len();
                        if frame_len > MAX_FRAME {
                            continue;
                        }
                        if frame_len > MAX_FRAME - output.len() {
                            self.pending_retry = true;
                            if let OverlayPayload::Png(png) = payload
                                && self.pending_png.is_none()
                            {
                                self.pending_png = Some(PendingPngTile {
                                    overlay,
                                    png,
                                    placement_len: payload_len,
                                });
                            }
                            continue;
                        }
                        FrameWriter(output).write_all(position.as_bytes())?;
                        let remaining = MAX_FRAME - output.len() - restore.len();
                        match payload {
                            OverlayPayload::Rgb => write_kitty_rgb_placement_with_limit(
                                tile,
                                image_id,
                                band.output_z(),
                                remaining,
                                &mut UploadWriter { output, staging },
                            )?,
                            OverlayPayload::Rgba => write_kitty_rgba_placement_with_limit(
                                tile,
                                image_id,
                                band.output_z(),
                                remaining,
                                &mut UploadWriter { output, staging },
                            )?,
                            OverlayPayload::Png(png) => png.write_with_limit(
                                image_id,
                                band.output_z(),
                                remaining,
                                &mut UploadWriter { output, staging },
                            )?,
                        }
                        if staging {
                            self.stage_placement(overlay, &position, None);
                        }
                        moved_cursor = true;
                        self.next_id = next_id;
                        self.entries.push(overlay);
                    }
                }
            }
        }
        if moved_cursor {
            write!(
                FrameWriter(output),
                "\x1b[{};{}H",
                cursor.0 + 1,
                cursor.1 + 1
            )?;
        }
        if !self.pending_retry {
            self.pending_png = None;
            self.publish_replacement(cursor, output)?;
        }
        Ok(())
    }

    fn stage_placement(&mut self, overlay: KittyOverlay, position: &str, fit: Option<(u32, u32)>) {
        let placement = overlay
            .placement_id
            .map_or_else(String::new, |id| format!(",p={id}"));
        let fit = fit.map_or_else(String::new, |(columns, rows)| {
            format!(",c={columns},r={rows}")
        });
        let command = format!(
            "{position}\x1b_Ga=p,i={}{placement}{fit},z={},C=1,q=1\x1b\\",
            overlay.image_id,
            overlay.band.output_z()
        );
        self.staged_placements.push((overlay, command.into_bytes()));
    }

    fn publish_replacement(
        &mut self,
        cursor: (usize, usize),
        output: &mut VecDeque<u8>,
    ) -> io::Result<()> {
        if self.retired.is_empty() && self.staged_placements.is_empty() {
            return Ok(());
        }
        let restore = format!("\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1);
        let length = self
            .staged_placements
            .iter()
            .map(|(_, command)| command.len())
            .sum::<usize>()
            + self
                .retired
                .iter()
                .map(|old| old.deletion().command().len())
                .sum::<usize>()
            + restore.len();
        if length > MAX_FRAME - output.len() {
            self.pending_retry = true;
            return Ok(());
        }
        for (_, command) in self.staged_placements.drain(..) {
            FrameWriter(output).write_all(&command)?;
        }
        for old in self.retired.drain(..) {
            self.pending_delete.push_back(old.deletion());
        }
        self.flush_deletes(output)?;
        FrameWriter(output).write_all(restore.as_bytes())?;
        Ok(())
    }

    pub(crate) fn clear(&mut self, output: &mut VecDeque<u8>) -> io::Result<()> {
        self.pending_retry = false;
        self.pending_png = None;
        self.incomplete_virtual.clear();
        self.staged_placements.clear();
        for entry in self.entries.drain(..).chain(self.retired.drain(..)) {
            if entry.placement_id.is_none() {
                self.pending_delete
                    .push_back(OuterImageDelete::Image(entry.image_id));
            }
        }
        for image in self.cached_images.drain(..) {
            self.pending_delete
                .push_back(OuterImageDelete::Image(image.image_id));
        }
        self.flush_deletes(output)?;
        Ok(())
    }

    fn flush_deletes(&mut self, output: &mut VecDeque<u8>) -> io::Result<()> {
        while let Some(&delete) = self.pending_delete.front() {
            let command = delete.command();
            if command.len() > MAX_FRAME - output.len() {
                self.pending_retry = true;
                break;
            }
            FrameWriter(output).write_all(command.as_bytes())?;
            self.pending_delete.pop_front();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::Windows;
    use std::ffi::OsStr;

    fn image_window(rows: u16, columns: u16) -> io::Result<PaneSet<Pane>> {
        let content_rows = if rows >= 3 { rows - 2 } else { rows };
        let content_columns = if columns >= 3 { columns - 2 } else { columns };
        PaneSet::new(
            rows,
            columns,
            Pane::spawn_in(
                OsStr::new("/bin/sh"),
                None,
                content_rows,
                content_columns,
                crate::config::Notifications::default(),
                crate::config::DEFAULT_SCROLLBACK_LINES,
            )?,
        )
    }

    #[test]
    fn outer_image_crop_preserves_cell_aligned_origin_and_pixels() {
        let cell = CellPixelSize::new(2, 2).unwrap();
        let mut pixels = vec![0; 7 * 5 * 4];
        pixels[(3 * 7 + 3) * 4..(3 * 7 + 3) * 4 + 4].copy_from_slice(&[4, 5, 6, 255]);
        let image = DecodedImage {
            width: 7,
            height: 5,
            pixels,
        };
        let (cropped, column, row) = crop_overlay_to_visible_cells(image, cell).unwrap();
        assert_eq!((column, row), (1, 1));
        assert_eq!((cropped.width, cropped.height), (2, 2));
        assert_eq!(&cropped.pixels[12..16], &[4, 5, 6, 255]);
        assert_eq!(cropped.pixels.iter().filter(|&&byte| byte != 0).count(), 4);
        assert!(
            crop_overlay_to_visible_cells(
                DecodedImage {
                    width: 2,
                    height: 2,
                    pixels: vec![0; 16],
                },
                cell,
            )
            .is_none()
        );
    }

    #[test]
    fn outer_image_tile_accepts_one_large_cell_when_encoded_output_fits() {
        let cell = CellPixelSize::new(1600, 1600).unwrap();
        let image = DecodedImage {
            width: 1600,
            height: 1600,
            pixels: vec![255; 1600 * 1600 * 4],
        };
        assert!(image.pixels.len() > MAX_KITTY_TILE_RAW_BYTES);
        assert_eq!(overlay_tile_size(&image, cell), Some((1600, 1600)));
        let placement = kitty_rgba_placement_len(&image, 0x8000_0000, 0).unwrap();
        assert!(b"\x1b[3;2H".len() + placement + b"\x1b[4;3H".len() <= MAX_FRAME);

        let too_large = DecodedImage {
            width: 2000,
            height: 2000,
            pixels: vec![255; 2000 * 2000 * 4],
        };
        let large_cell = CellPixelSize::new(2000, 2000).unwrap();
        assert_eq!(
            overlay_tile_size(&too_large, large_cell),
            Some((2000, 2000))
        );
        assert!(kitty_rgba_placement_len(&too_large, 0x8000_0000, 0).unwrap() > MAX_FRAME);
        let (payload, encoded_len) = prepare_overlay_payload(&too_large, 0x8000_0000, 0).unwrap();
        assert!(matches!(payload, OverlayPayload::Png(_)));
        assert!(encoded_len < MAX_FRAME);
    }

    #[test]
    fn overlay_uses_png_only_when_it_reduces_wire_bytes() {
        let small = DecodedImage {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 4],
        };
        assert!(matches!(
            prepare_overlay_payload(&small, 7, 0).unwrap().0,
            OverlayPayload::Rgba
        ));

        let small_opaque = DecodedImage {
            width: 2,
            height: 1,
            pixels: vec![1, 2, 3, 255, 4, 5, 6, 255],
        };
        let (payload, rgb_len) = prepare_overlay_payload(&small_opaque, 7, 0).unwrap();
        assert!(matches!(payload, OverlayPayload::Rgb));
        assert_eq!(
            rgb_len,
            kitty_rgb_placement_len(&small_opaque, 7, 0).unwrap()
        );

        let flat = DecodedImage {
            width: 512,
            height: 512,
            pixels: vec![255; 512 * 512 * 4],
        };
        let (payload, png_len) = prepare_overlay_payload(&flat, 7, 0).unwrap();
        assert!(matches!(payload, OverlayPayload::Png(_)));
        assert!(png_len < kitty_rgba_placement_len(&flat, 7, 0).unwrap());

        let mut state = 0x1234_5678u32;
        let noise = DecodedImage {
            width: 512,
            height: 512,
            pixels: (0..512 * 512 * 4)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state as u8
                })
                .collect(),
        };
        let (payload, wire_len) = prepare_overlay_payload(&noise, 7, 0).unwrap();
        assert!(matches!(payload, OverlayPayload::Rgba));
        assert_eq!(wire_len, kitty_rgba_placement_len(&noise, 7, 0).unwrap());
    }
    #[test]
    fn cached_outer_images_evict_the_oldest_inactive_source() {
        let mut overlays = KittyOverlays::default();
        for index in 0..MAX_CACHED_OUTER_IMAGES {
            overlays.cached_images.push(CachedOuterImage {
                image_id: 0x8000_0000 + index as u32,
                format: 32,
                width: 1,
                height: 1,
                generation: index as u64,
                size: 1,
                content: None,
                last_used: index as u64,
            });
        }
        let mut output = VecDeque::new();
        assert!(overlays.make_cache_room(1, &mut output).unwrap());
        assert_eq!(overlays.cached_images.len(), MAX_CACHED_OUTER_IMAGES - 1);
        assert!(
            overlays
                .cached_images
                .iter()
                .all(|image| image.image_id != 0x8000_0000)
        );
        assert_eq!(
            output.into_iter().collect::<Vec<_>>(),
            b"\x1b_Ga=d,d=I,i=2147483648,q=2\x1b\\"
        );
    }

    #[test]
    fn kitty_overlay_uploads_once_and_deletes_only_its_own_image() {
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut windows = Windows::default();
        let window_id = windows
            .create("image".into(), image_window(4, 4).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        panes.active_mut().process_output_with_image_store_sized(
            b"\x1b_Ga=T,f=32,s=1,v=1,i=7,p=1,c=1,r=1,C=1;AQIDBA==\x1b\\",
            &mut |_| {},
            cell,
        );
        let mut cache = KittyOverlays::default();
        let mut output = VecDeque::new();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let upload: Vec<_> = output.drain(..).collect();
        assert!(
            upload.starts_with(b"\x1b[3;2H\x1b_Ga=T,f=32,s=1,v=1,i=2147483648,z=0,C=1,q=2,m=0;")
        );
        assert!(upload.ends_with(b"\x1b[4;3H"));
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert!(
            output.is_empty(),
            "unchanged image must not be retransmitted"
        );

        panes.active_mut().image_store_mut().clear();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert_eq!(
            output.drain(..).collect::<Vec<_>>(),
            b"\x1b_Ga=d,d=I,i=2147483648,q=2\x1b\\"
        );
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn regular_crop_moves_reuse_source_and_replacements_upload_atomically() {
        use base64::Engine;
        let cell = CellPixelSize::new(10, 10).unwrap();
        let mut windows = Windows::default();
        let window = windows
            .create("crop".into(), image_window(8, 8).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        let pixels = vec![255; 4 * 4 * 3];
        let upload = format!(
            "\x1b_Ga=T,f=24,s=4,v=4,i=7,p=1,c=2,r=2,x=0,y=0,w=2,h=2,X=1,Y=2,z=-1,C=1;{}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(&pixels)
        );
        panes.active_mut().process_output_with_image_store_sized(
            upload.as_bytes(),
            &mut |_| {},
            cell,
        );
        let band = ImageBand::BehindText;
        let source = panes.active().source_image_band(cell, band).unwrap();
        assert_eq!(
            source.placement_controls(),
            ",x=0,y=0,w=2,h=2,c=2,r=2,X=1,Y=2"
        );
        let generation = source.generation;
        let mut cache = KittyOverlays {
            shm_supported: true,
            ..KittyOverlays::default()
        };
        let mut output = VecDeque::new();
        cache
            .render(window, panes, cell, 10, (0, 0), &mut output)
            .unwrap();
        let first = String::from_utf8(output.drain(..).collect()).unwrap();
        assert!(
            first.contains(
                "a=T,t=s,f=24,s=4,v=4,S=48,i=2147483648,p=1,x=0,y=0,w=2,h=2,c=2,r=2,X=1,Y=2,z=-1"
            ),
            "{first}"
        );
        for y in [1, 2, 0] {
            let movement =
                format!("\x1b[2;2H\x1b_Ga=p,i=7,p=1,c=2,r=2,x=1,y={y},w=2,h=2,z=-1,C=1\x1b\\");
            panes.active_mut().process_output_with_image_store_sized(
                movement.as_bytes(),
                &mut |_| {},
                cell,
            );
            assert_eq!(
                panes
                    .active()
                    .source_image_band(cell, band)
                    .unwrap()
                    .generation,
                generation
            );
            cache
                .render(window, panes, cell, 10, (0, 0), &mut output)
                .unwrap();
            let moved = String::from_utf8(output.drain(..).collect()).unwrap();
            assert!(
                moved.contains(&format!(
                    "a=p,i=2147483648,p={},x=1,y={y},w=2,h=2,c=2,r=2,z=-1",
                    cache.next_placement_id - 1
                )),
                "{moved}"
            );
            assert!(!moved.contains("t=s") && !moved.contains("a=T") && !moved.contains("a=t"));
            assert!(moved.find("a=p").unwrap() < moved.find("a=d").unwrap());
            assert_eq!(
                cache.pending_shm.len(),
                1,
                "movement must not copy source pixels"
            );
            assert_eq!(cache.cached_images.len(), 1);
        }
        // Explicit child IDs may be reused for different pixels. Never reuse
        // stale outer pixels, and stage this upload before displaying it.
        panes.active_mut().process_output_with_image_store_sized(
            upload.as_bytes(),
            &mut |_| {},
            cell,
        );
        assert_ne!(
            panes
                .active()
                .source_image_band(cell, band)
                .unwrap()
                .generation,
            generation
        );
        cache
            .render(window, panes, cell, 10, (0, 0), &mut output)
            .unwrap();
        let replaced = String::from_utf8(output.drain(..).collect()).unwrap();
        assert!(
            replaced.contains("a=t,t=s") && !replaced.contains("a=T"),
            "{replaced}"
        );
        assert!(replaced.find("a=t").unwrap() < replaced.find("a=p").unwrap());
        assert!(replaced.find("a=p").unwrap() < replaced.find("a=d").unwrap());
        assert_eq!(cache.cached_images.len(), 2);
        // Pane clipping and overlapping placements still require composition.
        panes.active_mut().process_output_with_image_store_sized(
            b"\x1b[8;8H\x1b_Ga=p,i=7,p=1,c=2,r=2,z=-1,C=1\x1b\\",
            &mut |_| {},
            cell,
        );
        assert!(panes.active().source_image_band(cell, band).is_none());
        panes.active_mut().process_output_with_image_store_sized(b"\x1b[1;1H\x1b_Ga=p,i=7,p=1,c=2,r=2,z=-1,C=1\x1b\\\x1b_Ga=p,i=7,p=2,c=2,r=2,z=-1,C=1\x1b\\", &mut |_| {}, cell);
        assert!(panes.active().source_image_band(cell, band).is_none());
    }

    #[test]
    fn kitty_overlay_forwards_complete_virtual_rgb_without_compositing() {
        use base64::Engine;

        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut windows = Windows::default();
        let window_id = windows
            .create("raw preview".into(), image_window(6, 6).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        let pixels = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
        let encoded = base64::engine::general_purpose::STANDARD.encode(pixels);
        let child = format!(
            "\x1b_Ga=T,f=24,s=2,v=2,i=7,p=1,U=1;{encoded}\x1b\\\
             \x1b[38;5;7m\x1b[58;5;1m\
             \x1b[2;2H\u{10eeee}\u{0305}\u{0305}\u{10eeee}\u{0305}\u{030d}\
             \x1b[3;2H\u{10eeee}\u{030d}\u{0305}\u{10eeee}\u{030d}\u{030d}"
        );
        panes.active_mut().process_output_with_image_store_sized(
            child.as_bytes(),
            &mut |_| {},
            cell,
        );
        let raw = panes
            .active()
            .source_image_band(cell, ImageBand::AboveText)
            .unwrap();
        assert_eq!((raw.column, raw.row, raw.columns, raw.rows), (1, 1, 2, 2));
        assert_eq!(raw.data, pixels);
        assert_eq!(raw.format, 24);
        let mut cache = KittyOverlays {
            shm_supported: true,
            ..KittyOverlays::default()
        };
        let mut output = VecDeque::new();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let upload: Vec<_> = output.drain(..).collect();
        assert!(upload.starts_with(
            b"\x1b[4;3H\x1b_Ga=T,t=s,f=24,s=2,v=2,S=12,i=2147483648,p=1,c=2,r=2,z=0,C=1,q=1;"
        ));
        assert!(upload.ends_with(b"\x1b[4;3H"));
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.pending_shm.len(), 1);
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert!(output.is_empty());

        // Yazi retransmits the same preview when returning to a file. A new
        // upload generation must still reuse its retained virtual source.
        panes.active_mut().process_output_with_image_store_sized(
            child.as_bytes(),
            &mut |_| {},
            cell,
        );
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let reused = String::from_utf8(output.drain(..).collect()).unwrap();
        assert!(reused.contains("a=p,i=2147483648,p=2,c=2,r=2"), "{reused}");
        assert!(!reused.contains("t=s"));
        assert_eq!(cache.cached_images.len(), 1);
        assert_eq!(cache.pending_shm.len(), 1);

        // An incomplete rectangle must not queue a provisional composite while
        // the child is still painting it. A stable sparse scene still falls
        // back to the normal clipped composition after the quiet period.
        panes
            .active_mut()
            .process_output_with_image_store_sized(b"\x1b[2;3H ", &mut |_| {}, cell);
        assert!(
            panes
                .active()
                .source_image_band(cell, ImageBand::AboveText)
                .is_none()
        );
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert_eq!(
            output.drain(..).collect::<Vec<_>>(),
            b"\x1b_Ga=d,d=i,i=2147483648,p=2,q=2\x1b\\"
        );
        assert!(!cache.pending_retry);
        assert!(cache.next_deferred_retry().is_some());
        cache.incomplete_virtual[0].last_change -= INCOMPLETE_VIRTUAL_QUIET;
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let fallback: Vec<_> = output.drain(..).collect();
        assert!(fallback.windows(6).any(|window| window == b"\x1b_Ga=T"));
        assert!(!cache.pending_retry);
        assert!(cache.next_deferred_retry().is_none());
        panes.active_mut().process_output_with_image_store_sized(
            "\x1b[2;3H\u{10eeee}\u{0305}\u{030d}".as_bytes(),
            &mut |_| {},
            cell,
        );
        assert!(
            panes
                .active()
                .source_image_band(cell, ImageBand::AboveText)
                .is_some()
        );
        panes.active_mut().process_output_with_image_store_sized(
            b"\x1b_Ga=p,i=7,p=2,U=1,c=2,r=2;\x1b\\",
            &mut |_| {},
            cell,
        );
        assert!(
            panes
                .active()
                .source_image_band(cell, ImageBand::AboveText)
                .is_none()
        );
    }

    #[test]
    fn kitty_overlay_forwards_validated_virtual_png_bytes_without_reencoding() {
        use base64::Engine;

        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut windows = Windows::default();
        let window_id = windows
            .create("png preview".into(), image_window(6, 6).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        let pixels = [
            255, 0, 0, 128, 0, 255, 0, 255, 0, 0, 255, 64, 255, 255, 255, 255,
        ];
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&pixels).unwrap();
            writer.finish().unwrap();
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
        let child = format!(
            "\x1b_Ga=T,f=100,s=2,v=2,i=9,p=1,U=1;{encoded}\x1b\\\
             \x1b[38;5;9m\x1b[58;5;1m\
             \x1b[2;2H\u{10eeee}\u{0305}\u{0305}\u{10eeee}\u{0305}\u{030d}\
             \x1b[3;2H\u{10eeee}\u{030d}\u{0305}\u{10eeee}\u{030d}\u{030d}"
        );
        panes.active_mut().process_output_with_image_store_sized(
            child.as_bytes(),
            &mut |_| {},
            cell,
        );
        let source = panes
            .active()
            .source_image_band(cell, ImageBand::AboveText)
            .unwrap();
        assert_eq!(source.data, png);
        assert_eq!(source.format, 100);
        assert_eq!(
            (source.column, source.row, source.columns, source.rows),
            (1, 1, 2, 2)
        );

        let mut direct = KittyOverlays::default();
        let mut output = VecDeque::new();
        direct
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let upload: Vec<_> = output.drain(..).collect();
        assert!(upload.starts_with(b"\x1b[4;3H\x1b_Ga=T,f=100,i=2147483648,z=0,C=1,q=2,m=0;"));
        let commands = crate::graphics::GraphicsFramer::new().advance(&upload);
        let mut assembler = crate::graphics_transfer::DirectTransferAssembler::new();
        let transfer = commands
            .into_iter()
            .find_map(|event| match event {
                crate::graphics::GraphicsEvent::Command(command) => assembler.accept(&command),
                _ => None,
            })
            .unwrap();
        assert_eq!(transfer.data, png);
        direct
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert!(output.is_empty(), "unchanged PNG must not be retransmitted");

        let mut shared = KittyOverlays {
            shm_supported: true,
            ..KittyOverlays::default()
        };
        shared
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let upload: Vec<_> = output.drain(..).collect();
        let prefix = format!(
            "\x1b[4;3H\x1b_Ga=T,t=s,f=100,s=2,v=2,S={},i=2147483648,p=1,z=0,C=1,q=1;",
            png.len()
        );
        assert!(upload.starts_with(prefix.as_bytes()));
        assert_eq!(shared.pending_shm.len(), 1);
        let encoded_name = upload[prefix.len()..]
            .split(|&byte| byte == 0x1b)
            .next()
            .unwrap();
        let name = base64::engine::general_purpose::STANDARD
            .decode(encoded_name)
            .unwrap();
        let name = std::ffi::CString::new(name).unwrap();
        let fd = unsafe { nix::libc::shm_open(name.as_ptr(), nix::libc::O_RDONLY, 0) };
        assert!(fd >= 0);
        let mapped = unsafe {
            nix::libc::mmap(
                std::ptr::null_mut(),
                png.len(),
                nix::libc::PROT_READ,
                nix::libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(mapped, nix::libc::MAP_FAILED);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(mapped.cast::<u8>(), png.len()) },
            png
        );
        unsafe {
            nix::libc::munmap(mapped, png.len());
            nix::libc::close(fd);
            nix::libc::shm_unlink(name.as_ptr());
        }
    }

    #[test]
    fn kitty_overlay_forwards_translucent_virtual_rgba_without_compositing() {
        use base64::Engine;

        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut windows = Windows::default();
        let window_id = windows
            .create("rgba preview".into(), image_window(6, 6).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        let pixels = [
            255, 0, 0, 128, 0, 255, 0, 0, 0, 0, 255, 64, 255, 255, 255, 255,
        ];
        let encoded = base64::engine::general_purpose::STANDARD.encode(pixels);
        let child = format!(
            "\x1b_Ga=T,f=32,s=2,v=2,i=8,p=1,U=1;{encoded}\x1b\\\
             \x1b[38;5;8m\x1b[58;5;1m\
             \x1b[2;2H\u{10eeee}\u{0305}\u{0305}\u{10eeee}\u{0305}\u{030d}\
             \x1b[3;2H\u{10eeee}\u{030d}\u{0305}\u{10eeee}\u{030d}\u{030d}"
        );
        panes.active_mut().process_output_with_image_store_sized(
            child.as_bytes(),
            &mut |_| {},
            cell,
        );
        let raw = panes
            .active()
            .source_image_band(cell, ImageBand::AboveText)
            .unwrap();
        assert_eq!(raw.data, pixels);
        assert_eq!(raw.format, 32);

        let mut cache = KittyOverlays {
            shm_supported: true,
            ..KittyOverlays::default()
        };
        let mut output = VecDeque::new();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let upload: Vec<_> = output.drain(..).collect();
        assert!(upload.starts_with(
            b"\x1b[4;3H\x1b_Ga=T,t=s,f=32,s=2,v=2,S=16,i=2147483648,p=1,c=2,r=2,z=0,C=1,q=1;"
        ));
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.pending_shm.len(), 1);
        panes
            .active_mut()
            .process_output_with_image_store_sized(b"\x1b[2;3H ", &mut |_| {}, cell);
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert_eq!(
            output.drain(..).collect::<Vec<_>>(),
            b"\x1b_Ga=d,d=i,i=2147483648,p=1,q=2\x1b\\"
        );
        assert!(cache.next_deferred_retry().is_some());
    }

    #[test]
    fn kitty_overlay_retains_displayed_image_when_replacement_frame_is_full() {
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut windows = Windows::default();
        let window_id = windows
            .create("delete retry".into(), image_window(4, 4).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        panes.active_mut().process_output_with_image_store_sized(
            b"\x1b_Ga=T,f=32,s=1,v=1,i=7,p=1,c=1,r=1,C=1;AQIDBA==\x1b\\",
            &mut |_| {},
            cell,
        );
        let mut cache = KittyOverlays::default();
        let mut output = VecDeque::new();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert_eq!(cache.entries.len(), 1);
        panes.active_mut().image_store_mut().clear();
        panes.active_mut().process_output_with_image_store_sized(
            b"\x1b_Ga=T,f=32,s=1,v=1,i=8,p=1,c=1,r=1,C=1;AQIDBA==\x1b\\",
            &mut |_| {},
            cell,
        );
        let delete = b"\x1b_Ga=d,d=I,i=2147483648,q=2\x1b\\";
        output = VecDeque::from(vec![0; MAX_FRAME - delete.len() + 1]);
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert!(cache.pending_delete.is_empty());
        assert_eq!(cache.retired.len(), 1);
        assert_eq!(cache.retired[0].image_id, 0x8000_0000);
        assert!(cache.pending_retry);
        assert!(cache.entries.is_empty());
        let image = panes
            .active()
            .compose_image_band(cell, ImageBand::AboveText)
            .unwrap()
            .unwrap();
        let (image, _, _) = crop_overlay_to_visible_cells(image, cell).unwrap();
        let allowance = b"\x1b[3;2H".len()
            + kitty_rgba_placement_len(&image, cache.next_id, 0).unwrap()
            + b"\x1b[4;3H".len();
        output = VecDeque::from(vec![0; MAX_FRAME - allowance]);
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let mut refreshed: Vec<_> = output.drain(MAX_FRAME - allowance..).collect();
        assert!(cache.pending_retry);
        assert_eq!(cache.staged_placements.len(), 1);
        assert_eq!(cache.retired.len(), 1);
        assert!(
            !refreshed
                .windows(6)
                .any(|w| w == b"\x1b_Ga=p" || w == b"\x1b_Ga=d")
        );
        output.clear();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let commit: Vec<_> = output.into();
        assert!(
            !commit.windows(6).any(|w| w == b"\x1b_Ga=t"),
            "completed upload must not repeat"
        );
        refreshed.extend(commit);
        let upload = refreshed
            .windows(b"a=t,f=32".len())
            .position(|w| w == b"a=t,f=32")
            .unwrap();
        let placement = refreshed
            .windows(b"a=p,i=2147483649".len())
            .position(|w| w == b"a=p,i=2147483649")
            .unwrap();
        let deletion = refreshed
            .windows(delete.len())
            .position(|w| w == delete)
            .unwrap();
        assert!(upload < placement && placement < deletion);
        assert!(cache.retired.is_empty());
        assert!(cache.staged_placements.is_empty());
        assert!(
            refreshed
                .windows(b"i=2147483649,z=0".len())
                .any(|bytes| bytes == b"i=2147483649,z=0")
        );
        assert!(cache.pending_delete.is_empty());
        assert!(!cache.pending_retry);
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn kitty_overlay_follows_virtual_placeholder_appearance_and_erasure() {
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut windows = Windows::default();
        let window_id = windows
            .create("virtual".into(), image_window(4, 4).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        panes.active_mut().process_output_with_image_store_sized(
            "\x1b_Ga=T,f=32,s=1,v=1,i=7,p=1,U=1,c=1,r=1;AQIDBA==\x1b\\\x1b[38;5;7m\u{10eeee}\u{0305}\u{0305}"
                .as_bytes(),
            &mut |_| {},
            cell,
        );
        let mut cache = KittyOverlays::default();
        let mut output = VecDeque::new();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert_eq!(cache.entries.len(), 1);
        output.clear();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert!(output.is_empty());

        panes
            .active_mut()
            .process_output_with_image_store_sized(b"\x1b[1;1H ", &mut |_| {}, cell);
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert_eq!(
            output.drain(..).collect::<Vec<_>>(),
            b"\x1b_Ga=d,d=I,i=2147483648,q=2\x1b\\"
        );
        assert!(cache.entries.is_empty());

        panes.active_mut().process_output_with_image_store_sized(
            "\x1b[2;2H\u{10eeee}\u{0305}\u{0305}".as_bytes(),
            &mut |_| {},
            cell,
        );
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert_eq!(cache.entries.len(), 1);
        assert!(
            output
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .windows(b"i=2147483649".len())
                .any(|bytes| bytes == b"i=2147483649")
        );
    }

    #[test]
    fn kitty_overlay_refreshes_virtual_image_when_only_cell_pixels_change() {
        use base64::Engine;

        let cell = CellPixelSize::new(1, 1).unwrap();
        let wider_cell = CellPixelSize::new(2, 1).unwrap();
        let mut windows = Windows::default();
        let window_id = windows
            .create("virtual resize".into(), image_window(4, 4).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        let encoded = base64::engine::general_purpose::STANDARD.encode([255; 16]);
        let output = format!(
            "\x1b_Ga=T,f=32,s=2,v=2,i=7,p=1,U=1;{encoded}\x1b\\\x1b[38;5;7m\x1b[58;5;1m\u{10eeee}\u{0305}\u{0305}\u{10eeee}\u{0305}\u{030d}"
        );
        panes.active_mut().process_output_with_image_store_sized(
            output.as_bytes(),
            &mut |_| {},
            cell,
        );
        let revision = panes.active().image_store().revision();
        let mut cache = KittyOverlays::default();
        let mut output = VecDeque::new();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert!(output.is_empty());
        assert!(cache.next_deferred_retry().is_some());
        cache.incomplete_virtual[0].last_change -= INCOMPLETE_VIRTUAL_QUIET;
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert!(
            output
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .windows(b"a=T,f=24,s=2,v=1,i=2147483648".len())
                .any(|bytes| bytes == b"a=T,f=24,s=2,v=1,i=2147483648"),
            "{}",
            String::from_utf8_lossy(&output.iter().copied().collect::<Vec<_>>())
        );
        output.clear();

        cache
            .render(window_id, panes, wider_cell, 6, (3, 2), &mut output)
            .unwrap();
        let refreshed: Vec<_> = output.drain(..).collect();
        assert!(refreshed.starts_with(b"\x1b_Ga=d,d=I,i=2147483648,q=2\x1b\\"));
        assert!(
            refreshed
                .windows(b"a=T,f=24,s=2,v=1,i=2147483649".len())
                .any(|bytes| bytes == b"a=T,f=24,s=2,v=1,i=2147483649"),
            "{}",
            String::from_utf8_lossy(&refreshed)
        );
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(panes.active().image_store().revision(), revision);
        let image = panes.active().compose_image_snapshot(wider_cell).unwrap();
        assert_eq!(&image.pixels[0..4], &[255; 4]);
        assert_eq!(&image.pixels[8..12], &[0; 4]);
        cache
            .render(window_id, panes, wider_cell, 6, (3, 2), &mut output)
            .unwrap();
        assert!(
            output.is_empty(),
            "unchanged resized image must not be resent"
        );
    }

    #[test]
    fn kitty_overlay_emits_all_stacking_bands_and_cleans_each_id() {
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut windows = Windows::default();
        let window_id = windows
            .create("layers".into(), image_window(4, 6).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        for (id, z) in [(1, i32::MIN / 2 - 1), (2, -1), (3, 0)] {
            let command =
                format!("\x1b_Ga=T,f=32,s=1,v=1,i={id},p=1,c=1,r=1,z={z},C=1;AQIDBA==\x1b\\");
            panes.active_mut().process_output_with_image_store_sized(
                command.as_bytes(),
                &mut |_| {},
                cell,
            );
        }
        let mut cache = KittyOverlays::default();
        let mut output = VecDeque::new();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let upload: Vec<_> = output.drain(..).collect();
        let headers = [
            b"i=2147483648,z=-2147483648,C=1,q=2".as_slice(),
            b"i=2147483649,z=-1,C=1,q=2",
            b"i=2147483650,z=0,C=1,q=2",
        ];
        let positions: Vec<_> = headers
            .iter()
            .map(|header| {
                upload
                    .windows(header.len())
                    .position(|bytes| bytes == *header)
                    .unwrap()
            })
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(cache.entries.len(), 3);
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert!(output.is_empty());

        panes.active_mut().image_store_mut().clear();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let deleted = output.drain(..).collect::<Vec<_>>();
        for id in 2147483648_u64..=2147483650 {
            let command = format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\");
            assert!(
                deleted
                    .windows(command.len())
                    .any(|bytes| bytes == command.as_bytes())
            );
        }
        assert_eq!(cache.entries.len(), 0);
    }

    #[test]
    fn kitty_overlay_retries_only_bands_that_missed_the_frame_budget() {
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut windows = Windows::default();
        let window_id = windows
            .create("budget".into(), image_window(4, 6).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        for (id, z) in [(1, i32::MIN), (2, -1), (3, 0)] {
            let command =
                format!("\x1b_Ga=T,f=32,s=1,v=1,i={id},p=1,c=1,r=1,z={z},C=1;AQIDBA==\x1b\\");
            panes.active_mut().process_output_with_image_store_sized(
                command.as_bytes(),
                &mut |_| {},
                cell,
            );
        }
        let first = panes
            .active()
            .compose_image_band(cell, ImageBand::BehindBackground)
            .unwrap()
            .unwrap();
        let (first, _, _) = crop_overlay_to_visible_cells(first, cell).unwrap();
        let allowance = b"\x1b[3;2H".len()
            + kitty_rgba_placement_len(&first, 0x8000_0000, i32::MIN).unwrap()
            + b"\x1b[4;3H".len();
        let mut output = VecDeque::from(vec![0; MAX_FRAME - allowance]);
        let mut cache = KittyOverlays::default();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        assert_eq!(output.len(), MAX_FRAME);
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.pending_retry);

        output.clear();
        cache
            .render(window_id, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let retry: Vec<_> = output.drain(..).collect();
        assert_eq!(cache.entries.len(), 3);
        assert!(!cache.pending_retry);
        assert!(
            !retry
                .windows(b"i=2147483648,z=".len())
                .any(|w| w == b"i=2147483648,z=")
        );
        assert!(
            retry
                .windows(b"i=2147483649,z=-1".len())
                .any(|w| w == b"i=2147483649,z=-1")
        );
        assert!(
            retry
                .windows(b"i=2147483650,z=0".len())
                .any(|w| w == b"i=2147483650,z=0")
        );
    }

    #[test]
    fn kitty_overlay_compresses_large_tiles_and_deletes_every_tile() {
        use base64::Engine;
        use std::collections::BTreeSet;

        let cell = CellPixelSize::new(10, 10).unwrap();
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2600, 2000);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&vec![255; 2600 * 2000 * 4])
                .unwrap();
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        let mut windows = Windows::default();
        let window_id = windows
            .create("large image".into(), image_window(202, 262).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        let command = format!("\x1b_Ga=T,f=100,i=7,p=1,c=260,r=200,C=1;{encoded}\x1b\\");
        panes.active_mut().process_output_with_image_store_sized(
            command.as_bytes(),
            &mut |_| {},
            cell,
        );
        let full = panes
            .active()
            .compose_image_band(cell, ImageBand::AboveText)
            .unwrap()
            .unwrap();
        let (full, _, _) = crop_overlay_to_visible_cells(full, cell).unwrap();
        assert!(kitty_rgba_placement_len(&full, 0x8000_0000, 0).unwrap() > MAX_FRAME);
        drop(full);
        let mut cache = KittyOverlays::default();
        // Force a frame-pressure retry before the normal upload. The first
        // compressed tile should be retained instead of compressed again.
        let mut output = VecDeque::from(vec![0; MAX_FRAME - 1]);
        cache
            .render(window_id, panes, cell, 204, (0, 0), &mut output)
            .unwrap();
        assert!(cache.pending_retry);
        assert!(cache.entries.is_empty());
        assert!(cache.pending_png.is_some());
        output.clear();
        cache
            .render(window_id, panes, cell, 204, (0, 0), &mut output)
            .unwrap();
        assert!(!cache.entries.is_empty());
        assert!(cache.pending_png.is_none());
        assert!(output.len() <= MAX_FRAME);
        let first_frame: Vec<_> = output.iter().copied().collect();
        assert!(
            first_frame
                .windows(b"a=T,f=100".len())
                .any(|bytes| bytes == b"a=T,f=100")
        );
        for _ in 0..8 {
            if !cache.pending_retry {
                break;
            }
            output.clear();
            cache
                .render(window_id, panes, cell, 204, (0, 0), &mut output)
                .unwrap();
            assert!(output.len() <= MAX_FRAME);
        }
        assert!(!cache.pending_retry);
        assert!(cache.entries.len() > 1);
        let ids: Vec<_> = cache.entries.iter().map(|entry| entry.image_id).collect();
        assert_eq!(
            ids.iter().copied().collect::<BTreeSet<_>>().len(),
            ids.len()
        );
        let offsets: BTreeSet<_> = cache
            .entries
            .iter()
            .map(|entry| (entry.column_offset, entry.row_offset))
            .collect();
        assert_eq!(offsets.len(), ids.len());

        output.clear();
        cache
            .render(window_id, panes, cell, 204, (0, 0), &mut output)
            .unwrap();
        assert!(output.is_empty(), "unchanged tiles must not be resent");
        let mut stale = cache.entries[0];
        stale.revision += 1;
        cache.pending_png = Some(PendingPngTile {
            overlay: stale,
            png: EncodedKittyPng::from_rgba(&DecodedImage {
                width: 1,
                height: 1,
                pixels: vec![0; 4],
            })
            .unwrap(),
            placement_len: 1,
        });
        cache
            .render(window_id, panes, cell, 204, (0, 0), &mut output)
            .unwrap();
        assert!(
            cache.pending_png.is_none(),
            "stale scene must drop cached PNG"
        );
        assert!(output.is_empty());
        let first_delete = format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", ids[0]);
        output = VecDeque::from(vec![0; MAX_FRAME - first_delete.len()]);
        cache.clear(&mut output).unwrap();
        assert!(cache.pending_retry);
        assert_eq!(cache.pending_delete.len(), ids.len() - 1);
        let mut deletes: Vec<_> = output
            .into_iter()
            .skip(MAX_FRAME - first_delete.len())
            .collect();
        let mut output = VecDeque::new();
        cache.clear(&mut output).unwrap();
        deletes.extend(output);
        assert!(cache.pending_delete.is_empty());
        assert!(!cache.pending_retry);
        for id in ids {
            let command = format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\");
            assert!(
                deletes
                    .windows(command.len())
                    .any(|bytes| bytes == command.as_bytes())
            );
        }
        assert!(cache.entries.is_empty());
    }
    fn replace_three_bands(panes: &mut PaneSet<Pane>, cell: CellPixelSize, pixels: &str) {
        for (id, z) in [(1, i32::MIN), (2, -1), (3, 0)] {
            let command =
                format!("\x1b_Ga=T,f=32,s=1,v=1,i={id},p=1,c=1,r=1,z={z},C=1;{pixels}\x1b\\");
            panes.active_mut().process_output_with_image_store_sized(
                command.as_bytes(),
                &mut |_| {},
                cell,
            );
        }
    }

    #[test]
    fn kitty_overlay_publishes_all_replacement_bands_before_retiring_old_scene() {
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut windows = Windows::default();
        let window = windows
            .create("replacement".into(), image_window(4, 6).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        replace_three_bands(panes, cell, "AQID/w==");
        let mut cache = KittyOverlays::default();
        let mut output = VecDeque::new();
        cache
            .render(window, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let old_ids: Vec<_> = cache.entries.iter().map(|entry| entry.image_id).collect();
        assert_eq!(old_ids.len(), 3);
        replace_three_bands(panes, cell, "BAUG/w==");
        let image = panes
            .active()
            .compose_image_band(cell, ImageBand::BehindBackground)
            .unwrap()
            .unwrap();
        let (image, _, _) = crop_overlay_to_visible_cells(image, cell).unwrap();
        let allowance = b"\x1b[3;2H".len()
            + kitty_rgb_placement_len(&image, cache.next_id, i32::MIN).unwrap()
            + b"\x1b[4;3H".len();
        output = VecDeque::from(vec![0; MAX_FRAME - allowance]);
        cache
            .render(window, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let first: Vec<_> = output.into_iter().skip(MAX_FRAME - allowance).collect();
        assert!(cache.pending_retry);
        assert_eq!(cache.retired.len(), 3);
        assert_eq!(cache.staged_placements.len(), 1);
        assert!(first.windows(6).any(|w| w == b"\x1b_Ga=t"));
        assert!(
            !first
                .windows(6)
                .any(|w| w == b"\x1b_Ga=p" || w == b"\x1b_Ga=d" || w == b"\x1b_Ga=T")
        );

        let mut output = VecDeque::new();
        cache
            .render(window, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let bytes: Vec<_> = output.into();
        let last_upload = bytes.windows(6).rposition(|w| w == b"\x1b_Ga=t").unwrap();
        let first_display = bytes.windows(6).position(|w| w == b"\x1b_Ga=p").unwrap();
        let last_display = bytes.windows(6).rposition(|w| w == b"\x1b_Ga=p").unwrap();
        let first_delete = bytes.windows(6).position(|w| w == b"\x1b_Ga=d").unwrap();
        assert!(last_upload < first_display && last_display < first_delete);
        for id in old_ids {
            let command = OuterImageDelete::Image(id).command();
            assert!(
                bytes
                    .windows(command.len())
                    .any(|w| w == command.as_bytes())
            );
        }
        assert!(!cache.pending_retry);
        assert_eq!(cache.entries.len(), 3);
        assert!(cache.retired.is_empty() && cache.staged_placements.is_empty());
    }

    #[test]
    fn kitty_overlay_clear_cancels_unpublished_replacement() {
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut windows = Windows::default();
        let window = windows
            .create("replacement".into(), image_window(4, 6).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        replace_three_bands(panes, cell, "AQID/w==");
        let mut cache = KittyOverlays::default();
        let mut output = VecDeque::new();
        cache
            .render(window, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        replace_three_bands(panes, cell, "BAUG/w==");
        // Uploads fit, but the final placement/delete batch does not.
        let image = panes
            .active()
            .compose_image_band(cell, ImageBand::BehindBackground)
            .unwrap()
            .unwrap();
        let (image, _, _) = crop_overlay_to_visible_cells(image, cell).unwrap();
        let allowance = b"\x1b[3;2H".len()
            + kitty_rgb_placement_len(&image, cache.next_id, i32::MIN).unwrap()
            + b"\x1b[4;3H".len();
        output = VecDeque::from(vec![0; MAX_FRAME - allowance]);
        cache
            .render(window, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let ids: Vec<_> = cache
            .entries
            .iter()
            .chain(&cache.retired)
            .map(|entry| entry.image_id)
            .collect();
        assert_eq!(ids.len(), 4);
        output.clear();
        cache.clear(&mut output).unwrap();
        let bytes: Vec<_> = output.into();
        for id in ids {
            let command = OuterImageDelete::Image(id).command();
            assert!(
                bytes
                    .windows(command.len())
                    .any(|w| w == command.as_bytes())
            );
        }
        assert!(!bytes.windows(6).any(|w| w == b"\x1b_Ga=p"));
        assert!(
            cache.entries.is_empty()
                && cache.retired.is_empty()
                && cache.staged_placements.is_empty()
        );
    }
    #[test]
    fn kitty_overlay_discards_stale_hidden_tiles_before_publishing_newer_revision() {
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut windows = Windows::default();
        let window = windows
            .create("rapid replacement".into(), image_window(4, 6).unwrap())
            .unwrap();
        let panes = windows.active_mut().unwrap().content_mut();
        replace_three_bands(panes, cell, "AQID/w==");
        let mut cache = KittyOverlays::default();
        let mut output = VecDeque::new();
        cache
            .render(window, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let old = cache.entries[0].image_id;
        replace_three_bands(panes, cell, "BAUG/w==");
        let image = panes
            .active()
            .compose_image_band(cell, ImageBand::BehindBackground)
            .unwrap()
            .unwrap();
        let (image, _, _) = crop_overlay_to_visible_cells(image, cell).unwrap();
        let allowance = b"\x1b[3;2H".len()
            + kitty_rgb_placement_len(&image, cache.next_id, i32::MIN).unwrap()
            + b"\x1b[4;3H".len();
        output = VecDeque::from(vec![0; MAX_FRAME - allowance]);
        cache
            .render(window, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let stale = cache.staged_placements[0].0.image_id;
        replace_three_bands(panes, cell, "BwgJ/w==");
        output.clear();
        cache
            .render(window, panes, cell, 6, (3, 2), &mut output)
            .unwrap();
        let bytes: Vec<_> = output.into();
        let stale_delete = OuterImageDelete::Image(stale).command();
        assert!(
            bytes
                .windows(stale_delete.len())
                .any(|w| w == stale_delete.as_bytes())
        );
        let stale_display = format!("a=p,i={stale},");
        assert!(
            !bytes
                .windows(stale_display.len())
                .any(|w| w == stale_display.as_bytes())
        );
        let last_display = bytes.windows(6).rposition(|w| w == b"\x1b_Ga=p").unwrap();
        let old_delete = OuterImageDelete::Image(old).command();
        let retired = bytes
            .windows(old_delete.len())
            .position(|w| w == old_delete.as_bytes())
            .unwrap();
        assert!(last_display < retired);
        assert!(cache.retired.is_empty() && cache.staged_placements.is_empty());
        assert!(!cache.pending_retry);
    }
}

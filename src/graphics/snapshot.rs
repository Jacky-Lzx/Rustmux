//! Explicit image-only snapshot of one pane's stored Kitty placements.
//! The terminal runtime uses all three stacking bands when Kitty support is known.

use super::geometry::{CellPixelSize, PixelSize};
use crate::{
    graphics_composite::{CompositeError, ImageLayer, compose_image_layers},
    graphics_decode::{
        ClipError, DecodeError, DecodedImage, MAX_DECODED_IMAGE_BYTES, ResampleError,
    },
    graphics_store::ImageStore,
    screen::Screen,
};

mod placeholder_raster;
mod raster;
mod source_placement;

use raster::collect_visible_clips;

#[cfg(test)]
pub(crate) use source_placement::source_image_pane_band;
pub(crate) use source_placement::{
    SourceImagePlacement, SourceImageProgress, source_image_pane_band_progress,
};

/// Kitty's special boundary below which images are also behind cells with a
/// non-default background. At the boundary they remain above those colors.
pub const BACKGROUND_Z_BOUNDARY: i32 = i32::MIN / 2;
pub const MAX_PLANE_CANVAS_BYTES: usize = 64 * 1024 * 1024;

/// One Kitty text/background stacking band. Each band is composited before
/// transmission so source-image z and ID ordering remains pane-local.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ImageBand {
    BehindBackground,
    BehindText,
    AboveText,
}

impl ImageBand {
    pub const ALL: [Self; 3] = [Self::BehindBackground, Self::BehindText, Self::AboveText];

    pub fn output_z(self) -> i32 {
        match self {
            Self::BehindBackground => i32::MIN,
            Self::BehindText => -1,
            Self::AboveText => 0,
        }
    }

    pub(super) fn contains(self, z: i32) -> bool {
        match self {
            Self::BehindBackground => z < BACKGROUND_Z_BOUNDARY,
            Self::BehindText => (BACKGROUND_Z_BOUNDARY..0).contains(&z),
            Self::AboveText => z >= 0,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ImagePlanes {
    /// `z < BACKGROUND_Z_BOUNDARY`: behind non-default cell backgrounds.
    pub behind_background: Option<DecodedImage>,
    /// `BACKGROUND_Z_BOUNDARY <= z < 0`: behind glyphs but above backgrounds.
    pub behind_text: Option<DecodedImage>,
    /// `z >= 0`: above text.
    pub above_text: Option<DecodedImage>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SnapshotError {
    InvalidViewport,
    OutputLimit,
    MissingImage,
    InvalidLayout,
    Decode(DecodeError),
    Resample(ResampleError),
    Clip(ClipError),
    Composite(CompositeError),
}

/// Decode, size, clip, and blend cursor-anchored placements on the selected
/// screen. Unanchored placements and references on the other screen are not
/// visible. An invalid visible image fails the snapshot rather than producing
/// a silently incomplete frame. No image data or placement state is mutated.
pub fn compose_store_snapshot(
    store: &ImageStore,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<DecodedImage, SnapshotError> {
    compose_snapshot(store, None, alternate, viewport, cell)
}

/// Include visible Unicode placeholder cells from the active pane screen.
pub(crate) fn compose_pane_snapshot(
    store: &ImageStore,
    screen: &Screen,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<DecodedImage, SnapshotError> {
    compose_snapshot(store, Some(screen), screen.is_alternate(), viewport, cell)
}

fn compose_snapshot(
    store: &ImageStore,
    screen: Option<&Screen>,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<DecodedImage, SnapshotError> {
    validate_viewport(viewport)?;
    let clipped = collect_visible_clips(store, screen, alternate, viewport, cell, |_| true)?;
    let layers: Vec<_> = clipped
        .iter()
        .map(|(image_id, z_index, placement)| ImageLayer {
            image_id: *image_id,
            z_index: *z_index,
            placement,
        })
        .collect();
    compose_image_layers(viewport, &layers).map_err(SnapshotError::Composite)
}

/// Preserve the three Kitty image/text stacking bands without combining them
/// with cell colors or glyphs. An absent band allocates no canvas. Up to two
/// full-size canvases fit the aggregate output limit; a third is rejected.
pub fn compose_store_planes(
    store: &ImageStore,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<ImagePlanes, SnapshotError> {
    compose_planes(store, None, alternate, viewport, cell)
}

pub(crate) fn compose_pane_planes(
    store: &ImageStore,
    screen: &Screen,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<ImagePlanes, SnapshotError> {
    compose_planes(store, Some(screen), screen.is_alternate(), viewport, cell)
}

fn compose_planes(
    store: &ImageStore,
    screen: Option<&Screen>,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<ImagePlanes, SnapshotError> {
    let canvas_bytes = validate_viewport(viewport)?;
    let clipped = collect_visible_clips(store, screen, alternate, viewport, cell, |_| true)?;
    let mut behind_background = Vec::new();
    let mut behind_text = Vec::new();
    let mut above_text = Vec::new();
    for (image_id, z_index, placement) in &clipped {
        let layer = ImageLayer {
            image_id: *image_id,
            z_index: *z_index,
            placement,
        };
        if *z_index < BACKGROUND_Z_BOUNDARY {
            behind_background.push(layer);
        } else if *z_index < 0 {
            behind_text.push(layer);
        } else {
            above_text.push(layer);
        }
    }
    let populated = usize::from(!behind_background.is_empty())
        + usize::from(!behind_text.is_empty())
        + usize::from(!above_text.is_empty());
    if canvas_bytes * populated > MAX_PLANE_CANVAS_BYTES {
        return Err(SnapshotError::OutputLimit);
    }
    Ok(ImagePlanes {
        behind_background: compose_nonempty(viewport, &behind_background)?,
        behind_text: compose_nonempty(viewport, &behind_text)?,
        above_text: compose_nonempty(viewport, &above_text)?,
    })
}

/// Compose one band without decoding or allocating the other two. An invalid
/// placement in a different band does not suppress this band's output.
pub fn compose_store_band(
    store: &ImageStore,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
    band: ImageBand,
) -> Result<Option<DecodedImage>, SnapshotError> {
    compose_band(store, None, alternate, viewport, cell, band)
}

pub(crate) fn compose_pane_band(
    store: &ImageStore,
    screen: &Screen,
    viewport: PixelSize,
    cell: CellPixelSize,
    band: ImageBand,
) -> Result<Option<DecodedImage>, SnapshotError> {
    compose_band(
        store,
        Some(screen),
        screen.is_alternate(),
        viewport,
        cell,
        band,
    )
}

fn compose_band(
    store: &ImageStore,
    screen: Option<&Screen>,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
    band: ImageBand,
) -> Result<Option<DecodedImage>, SnapshotError> {
    validate_viewport(viewport)?;
    let clipped = collect_visible_clips(store, screen, alternate, viewport, cell, |z| {
        band.contains(z)
    })?;
    let layers: Vec<_> = clipped
        .iter()
        .map(|(image_id, z_index, placement)| ImageLayer {
            image_id: *image_id,
            z_index: *z_index,
            placement,
        })
        .collect();
    compose_nonempty(viewport, &layers)
}

/// Compatibility helper for callers interested only in images above text.
pub fn compose_store_above_text(
    store: &ImageStore,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<Option<DecodedImage>, SnapshotError> {
    compose_store_band(store, alternate, viewport, cell, ImageBand::AboveText)
}

fn compose_nonempty(
    viewport: PixelSize,
    layers: &[ImageLayer<'_>],
) -> Result<Option<DecodedImage>, SnapshotError> {
    if layers.is_empty() {
        return Ok(None);
    }
    compose_image_layers(viewport, layers)
        .map(Some)
        .map_err(SnapshotError::Composite)
}

fn validate_viewport(viewport: PixelSize) -> Result<usize, SnapshotError> {
    if viewport.width == 0 || viewport.height == 0 {
        return Err(SnapshotError::InvalidViewport);
    }
    let canvas_bytes = u128::from(viewport.width) * u128::from(viewport.height) * 4;
    if canvas_bytes > MAX_DECODED_IMAGE_BYTES as u128 {
        return Err(SnapshotError::OutputLimit);
    }
    Ok(usize::try_from(canvas_bytes).unwrap())
}

#[cfg(test)]
mod deferred_snapshot_tests {
    use super::*;
    use crate::{
        graphics_store::{CellAnchor, ImageFormat},
        graphics_transfer::DirectTransferAssembler,
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    #[test]
    fn small_png_samples_only_visible_source_crop() {
        let pixels: Vec<u8> = vec![
            1, 2, 3, 255, 11, 12, 13, 255, 21, 22, 23, 255, 31, 32, 33, 255, 4, 5, 6, 255, 14, 15,
            16, 255, 24, 25, 26, 255, 34, 35, 36, 255,
        ];
        let expected = vec![
            11, 12, 13, 255, 21, 22, 23, 255, 14, 15, 16, 255, 24, 25, 26, 255,
        ];
        let mut data = Vec::new();
        let mut encoder = png::Encoder::new(&mut data, 4, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&pixels)
            .unwrap();
        let command = format!(
            "\x1b_Ga=T,f=100,i=7,x=1,y=0,w=3,h=2,c=3,r=2;{}\x1b\\",
            STANDARD.encode(data)
        );
        let transfer = DirectTransferAssembler::new()
            .accept(command.as_bytes())
            .unwrap();
        let mut store = ImageStore::new();
        store.insert_at(transfer, CellAnchor::default()).unwrap();
        let snapshot = compose_store_snapshot(
            &store,
            false,
            PixelSize {
                width: 2,
                height: 2,
            },
            CellPixelSize::new(1, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(snapshot.pixels, expected);
    }

    #[test]
    fn small_interlaced_png_retains_full_decode_fallback() {
        let mut info = png::Info::with_size(1, 1);
        info.color_type = png::ColorType::Rgba;
        info.bit_depth = png::BitDepth::Eight;
        info.interlaced = true;
        let mut data = Vec::new();
        png::Encoder::with_info(&mut data, info)
            .unwrap()
            .write_header()
            .unwrap()
            .write_image_data(&[11, 12, 13, 255])
            .unwrap();
        let command = format!("\x1b_Ga=T,f=100,i=7;{}\x1b\\", STANDARD.encode(data));
        let transfer = DirectTransferAssembler::new()
            .accept(command.as_bytes())
            .unwrap();
        let mut store = ImageStore::new();
        store.insert_at(transfer, CellAnchor::default()).unwrap();
        let snapshot = compose_store_snapshot(
            &store,
            false,
            PixelSize {
                width: 1,
                height: 1,
            },
            CellPixelSize::new(1, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(snapshot.pixels, [11, 12, 13, 255]);
    }

    #[test]
    fn deferred_large_png_and_zlib_raw_render_without_sized_upload() {
        let width = 3072;
        let height = 3072;
        let pixels = [17, 23, 31, 255].repeat(width * height);
        for format in [ImageFormat::Png, ImageFormat::RgbaZlib] {
            let (controls, data) = if format == ImageFormat::Png {
                let mut data = Vec::new();
                {
                    let mut encoder = png::Encoder::new(&mut data, width as u32, height as u32);
                    encoder.set_color(png::ColorType::Rgba);
                    encoder.set_depth(png::BitDepth::Eight);
                    let mut writer = encoder.write_header().unwrap();
                    writer.write_image_data(&pixels).unwrap();
                    writer.finish().unwrap();
                }
                (format!("f=100,s={width},v={height}"), data)
            } else {
                let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(&pixels).unwrap();
                (
                    format!("f=32,o=z,s={width},v={height}"),
                    encoder.finish().unwrap(),
                )
            };
            let upload = |data: &[u8]| {
                let command = format!(
                    "\x1b_Ga=T,i=7,c=2,r=2,{controls};{}\x1b\\",
                    STANDARD.encode(data)
                );
                DirectTransferAssembler::new()
                    .accept(command.as_bytes())
                    .unwrap()
            };
            let mut store = ImageStore::new();
            store
                .insert_at(upload(&data), CellAnchor::default())
                .unwrap();
            assert_eq!(store.known_image_dimensions(7), None);
            let viewport = PixelSize {
                width: 2,
                height: 2,
            };
            let cell = CellPixelSize::new(1, 1).unwrap();
            let snapshot = compose_store_snapshot(&store, false, viewport, cell).unwrap();
            assert_eq!(snapshot.pixels, [17, 23, 31, 255].repeat(4));
            // Read-only composition does not change the store's validation cache.
            assert_eq!(store.known_image_dimensions(7), None);

            let mut corrupt = data;
            *corrupt.last_mut().unwrap() ^= 1;
            store
                .insert_at(upload(&corrupt), CellAnchor::default())
                .unwrap();
            assert!(matches!(
                compose_store_snapshot(&store, false, viewport, cell),
                Err(SnapshotError::Decode(_))
            ));
        }
    }
}

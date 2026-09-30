//! Child-facing replies for completed Kitty graphics data commands.
//! Query replies do not insert or replace an image.

use super::command::parse_control_command;
use crate::{
    graphics_store::{ImageFormat, StoreError, StoredImage},
    graphics_transfer::{AssembledDirectTransfer, unsupported_medium_controls},
};

/// A recognizable file or temporary-file request can be rejected promptly so
/// a child may retry using direct data. Never access its path or disclose
/// whether it exists. Commands without a usable identity stay silent.
pub(crate) fn unsupported_medium_reply(command: &[u8], can_display: bool) -> Option<Vec<u8>> {
    medium_error_reply(command, can_display, "EINVAL:unsupported medium")
}

/// All shared-memory read failures use the same child-visible message. This
/// prevents a child from using replies to probe the local SHM namespace.
pub(crate) fn shared_memory_read_error_reply(command: &[u8], can_display: bool) -> Option<Vec<u8>> {
    medium_error_reply(command, can_display, "EBADF:Failed to read image file")
}

fn medium_error_reply(command: &[u8], can_display: bool, message: &str) -> Option<Vec<u8>> {
    if !can_display {
        return None;
    }
    let controls = unsupported_medium_controls(command)?;
    let action = controls.get(&b'a').map(Vec::as_slice);
    let id = controls.get(&b'i').and_then(|value| parse_nonzero(value));
    let image_number = controls.get(&b'I').and_then(|value| parse_nonzero(value));
    if action == Some(b"q") {
        if id.is_none() || controls.contains_key(&b'I') {
            return None;
        }
    } else if id.is_none() && image_number.is_none() {
        return None;
    }
    if controls.get(&b'q').is_some_and(|value| value == b"2") {
        return None;
    }
    let placement_id = if action == Some(b"T") {
        controls.get(&b'p').and_then(|value| parse_nonzero(value))
    } else {
        None
    };
    Some(encode_reply_with_number(
        id,
        image_number,
        placement_id,
        message,
    ))
}

/// Return a bounded APC reply only when the current outer attachment can
/// display the supported direct-data format. Silence lets the following
/// primary-DA reply tell a probing child that graphics are unavailable.
pub(crate) fn direct_query_reply(
    transfer: AssembledDirectTransfer,
    can_display: bool,
) -> Option<Vec<u8>> {
    if !can_display || transfer.control(b'a') != Some(b"q".as_slice()) {
        return None;
    }
    let id = parse_nonzero(transfer.control(b'i')?)?;
    let quiet = transfer
        .control(b'q')
        .and_then(|value| value.first())
        .copied();
    if quiet == Some(b'2') {
        return None;
    }
    let format = match (transfer.control(b'f'), transfer.streamed_raw_zlib()) {
        (None | Some(b"32"), false) => Some(ImageFormat::Rgba),
        (None | Some(b"32"), true) => Some(ImageFormat::RgbaZlib),
        (Some(b"24"), false) => Some(ImageFormat::Rgb),
        (Some(b"24"), true) => Some(ImageFormat::RgbZlib),
        (Some(b"100"), false) => Some(ImageFormat::Png),
        _ => None,
    };
    let supported_controls = transfer.supported_data_only_controls();
    let declared_width = transfer.control(b's').and_then(parse_nonzero);
    let declared_height = transfer.control(b'v').and_then(parse_nonzero);
    let valid = format.is_some_and(|format| {
        supported_controls && transfer.control(b'I').is_none() && {
            let image = StoredImage {
                format,
                declared_width,
                declared_height,
                data: transfer.data,
            };
            image.validated_assembled_dimensions().is_ok()
        }
    });
    if valid && quiet == Some(b'1') {
        return None;
    }
    let message = if valid { "OK" } else { "EINVAL:invalid image" };
    Some(encode_reply(id, None, message))
}

/// Capture a completed transfer's identity before it is moved into the image
/// store. An `a=T` response also identifies the placement it creates.
#[derive(Clone, Copy)]
pub(crate) struct TransferReply {
    id: Option<u32>,
    image_number: Option<u32>,
    placement_id: Option<u32>,
    display: bool,
    quiet: u8,
}

impl TransferReply {
    pub(crate) fn for_transfer(
        transfer: &AssembledDirectTransfer,
        can_display: bool,
    ) -> Option<Self> {
        if !can_display || !matches!(transfer.control(b'a'), None | Some(b"t" | b"T")) {
            return None;
        }
        let display = transfer.control(b'a') == Some(b"T");
        let id = transfer.control(b'i').and_then(parse_nonzero);
        let image_number = transfer.control(b'I').and_then(parse_decimal);
        if id.is_none() && image_number.is_none() {
            return None;
        }
        Some(Self {
            id,
            image_number,
            placement_id: if display {
                transfer.control(b'p').and_then(parse_nonzero)
            } else {
                None
            },
            display,
            quiet: transfer
                .control(b'q')
                .and_then(|value| value.first())
                .copied()
                .unwrap_or(b'0'),
        })
    }

    pub(crate) fn response(self, result: Result<u32, StoreError>) -> Option<Vec<u8>> {
        if self.quiet == b'2' || (self.quiet == b'1' && result.is_ok()) {
            return None;
        }
        let message = match result {
            Ok(_) => "OK",
            Err(StoreError::TooLarge) => "E2BIG:image too large",
            Err(StoreError::InvalidPlacement) if self.display => "EINVAL:invalid placement",
            Err(_) => "EINVAL:invalid image",
        };
        Some(encode_reply_with_number(
            result.ok().or(self.id),
            self.image_number,
            self.placement_id,
            message,
        ))
    }
}

/// Keep the same parsed control fields that the image store will apply. A
/// reply is emitted only after the store reports the placement's result.
#[derive(Clone, Copy)]
pub(crate) struct PlacementReply {
    id: Option<u32>,
    image_number: Option<u32>,
    placement_id: Option<u32>,
    quiet: u8,
}

impl PlacementReply {
    pub(crate) fn for_command(command: &[u8], can_display: bool) -> Option<Self> {
        if !can_display {
            return None;
        }
        let controls = parse_control_command(command)?;
        if controls.get(&b'a').map(Vec::as_slice) != Some(b"p") {
            return None;
        }
        let id = controls.get(&b'i').and_then(|value| parse_nonzero(value));
        let image_number = controls.get(&b'I').and_then(|value| parse_decimal(value));
        if id.is_none() && image_number.is_none() {
            return None;
        }
        Some(Self {
            id,
            image_number,
            placement_id: controls.get(&b'p').and_then(|value| parse_nonzero(value)),
            quiet: controls
                .get(&b'q')
                .and_then(|value| value.first())
                .copied()
                .unwrap_or(b'0'),
        })
    }

    pub(crate) fn response(
        self,
        error: Option<StoreError>,
        resolved_id: Option<u32>,
    ) -> Option<Vec<u8>> {
        if self.quiet == b'2' || (self.quiet == b'1' && error.is_none()) {
            return None;
        }
        let message = match error {
            None => "OK",
            Some(StoreError::MissingImage) => "ENOENT:image not found",
            Some(_) => "EINVAL:invalid placement",
        };
        Some(encode_reply_with_number(
            resolved_id.or(self.id),
            self.image_number,
            self.placement_id,
            message,
        ))
    }
}

fn encode_reply(id: u32, placement_id: Option<u32>, message: &str) -> Vec<u8> {
    encode_reply_with_number(Some(id), None, placement_id, message)
}

fn encode_reply_with_number(
    id: Option<u32>,
    image_number: Option<u32>,
    placement_id: Option<u32>,
    message: &str,
) -> Vec<u8> {
    let mut identity = String::new();
    if let Some(id) = id {
        identity.push_str(&format!("i={id}"));
    }
    if let Some(number) = image_number {
        if !identity.is_empty() {
            identity.push(',');
        }
        identity.push_str(&format!("I={number}"));
    }
    if let Some(placement_id) = placement_id {
        identity.push_str(&format!(",p={placement_id}"));
    }
    let response = format!("\x1b_G{identity};{message}\x1b\\").into_bytes();
    debug_assert!(response.len() <= crate::parser::MAX_REPLY_BYTES);
    response
}

fn parse_nonzero(bytes: &[u8]) -> Option<u32> {
    parse_decimal(bytes).filter(|&value| value != 0)
}

fn parse_decimal(bytes: &[u8]) -> Option<u32> {
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics_transfer::DirectTransferAssembler;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    fn assembled(command: &[u8]) -> AssembledDirectTransfer {
        DirectTransferAssembler::new().accept(command).unwrap()
    }

    fn query_with_data(controls: &str, data: &[u8]) -> AssembledDirectTransfer {
        let command = format!("\x1b_G{controls};{}\x1b\\", STANDARD.encode(data));
        assembled(command.as_bytes())
    }

    #[test]
    fn direct_rgb_query_answers_without_storing_or_changing_id() {
        let query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AQID\x1b\\";
        assert_eq!(
            direct_query_reply(assembled(query), true).unwrap(),
            b"\x1b_Gi=31;OK\x1b\\"
        );
        assert_eq!(direct_query_reply(assembled(query), false), None);
    }

    #[test]
    fn invalid_png_returns_error_and_quiet_modes_are_respected() {
        let invalid = b"\x1b_Gi=7,a=q,t=d,f=100;YQ==\x1b\\";
        assert_eq!(
            direct_query_reply(assembled(invalid), true).unwrap(),
            b"\x1b_Gi=7;EINVAL:invalid image\x1b\\"
        );
        let quiet_ok = b"\x1b_Gi=7,a=q,t=d,f=24,s=1,v=1,q=1;AQID\x1b\\";
        assert_eq!(direct_query_reply(assembled(quiet_ok), true), None);
        let quiet_all = b"\x1b_Gi=7,a=q,t=d,f=100,q=2;YQ==\x1b\\";
        assert_eq!(direct_query_reply(assembled(quiet_all), true), None);
    }

    #[test]
    fn large_png_query_uses_bounded_validation_without_storing_image() {
        let (width, height) = (3072, 3072);
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, width, height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&vec![7; (width * height) as usize])
                .unwrap();
        }
        let query = query_with_data("a=q,i=47,f=100,s=3072,v=3072", &png);
        assert_eq!(
            direct_query_reply(query, true).unwrap(),
            b"\x1b_Gi=47;OK\x1b\\"
        );

        *png.last_mut().unwrap() ^= 1;
        assert_eq!(
            direct_query_reply(query_with_data("a=q,i=47,f=100,s=3072,v=3072", &png), true)
                .unwrap(),
            b"\x1b_Gi=47;EINVAL:invalid image\x1b\\"
        );
    }

    #[test]
    fn large_compressed_raw_query_validates_entire_stream() {
        let (width, height) = (3072, 3072);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&vec![7; (width * height * 3) as usize])
            .unwrap();
        let compressed = encoder.finish().unwrap();
        let controls = "a=q,i=48,f=24,s=3072,v=3072,o=z";
        let query = query_with_data(controls, &compressed);
        assert!(query.streamed_raw_zlib());
        assert_eq!(
            direct_query_reply(query, true).unwrap(),
            b"\x1b_Gi=48;OK\x1b\\"
        );

        let mut corrupt = compressed;
        corrupt.push(1);
        assert_eq!(
            direct_query_reply(query_with_data(controls, &corrupt), true).unwrap(),
            b"\x1b_Gi=48;EINVAL:invalid image\x1b\\"
        );
    }

    #[test]
    fn unknown_query_control_must_not_get_false_success() {
        let query = b"\x1b_Gi=7,a=q,t=d,f=24,s=1,v=1,U=1;AQID\x1b\\";
        assert_eq!(
            direct_query_reply(assembled(query), true).unwrap(),
            b"\x1b_Gi=7;EINVAL:invalid image\x1b\\"
        );
        let ambiguous = b"\x1b_Gi=7,I=13,a=q,t=d,f=24,s=1,v=1;AQID\x1b\\";
        assert_eq!(
            direct_query_reply(assembled(ambiguous), true).unwrap(),
            b"\x1b_Gi=7;EINVAL:invalid image\x1b\\"
        );
    }

    #[test]
    fn unsupported_media_reply_is_bounded_private_and_quiet_aware() {
        let query = b"\x1b_Ga=q,t=f,i=31,f=100;L3ByaXZhdGUvcGljLnBuZw==\x1b\\";
        assert_eq!(
            unsupported_medium_reply(query, true).unwrap(),
            b"\x1b_Gi=31;EINVAL:unsupported medium\x1b\\"
        );
        assert_eq!(unsupported_medium_reply(query, false), None);
        let numbered = b"\x1b_Ga=T,t=t,I=13,p=9,f=100,q=1;L25hbWU=\x1b\\";
        assert_eq!(
            unsupported_medium_reply(numbered, true).unwrap(),
            b"\x1b_GI=13,p=9;EINVAL:unsupported medium\x1b\\"
        );
        assert_eq!(
            unsupported_medium_reply(b"\x1b_Ga=t,t=t,i=7,q=2;L3RtcC9pbWc=\x1b\\", true),
            None
        );
    }

    #[test]
    fn malformed_or_unidentifiable_media_stay_silent() {
        for command in [
            b"\x1b_Ga=q,t=f,f=100;L3RtcC9pbWc=\x1b\\".as_slice(),
            b"\x1b_Ga=q,t=f,I=13,f=100;L3RtcC9pbWc=\x1b\\",
            b"\x1b_Ga=t,t=f,i=7,m=1;L3RtcC9pbWc=\x1b\\",
            b"\x1b_Ga=t,t=f,i=7;***\x1b\\",
            b"\x1b_Ga=t,t=f,i=7,q=3;L3RtcC9pbWc=\x1b\\",
            b"\x1b_Ga=t,t=f,i=7,f=999;L3RtcC9pbWc=\x1b\\",
        ] {
            assert_eq!(unsupported_medium_reply(command, true), None);
        }
    }

    #[test]
    fn numbered_upload_reply_includes_assigned_id_and_number() {
        let numbered = assembled(b"\x1b_Ga=T,I=13,p=9,f=24,s=1,v=1;AQID\x1b\\");
        let ack = TransferReply::for_transfer(&numbered, true).unwrap();
        assert_eq!(
            ack.response(Ok(42)).unwrap(),
            b"\x1b_Gi=42,I=13,p=9;OK\x1b\\"
        );
        assert_eq!(
            ack.response(Err(StoreError::InvalidData)).unwrap(),
            b"\x1b_GI=13,p=9;EINVAL:invalid image\x1b\\"
        );
    }

    #[test]
    fn upload_ack_uses_id_and_respects_quiet_modes() {
        let regular = assembled(b"\x1b_Ga=t,i=7,f=24,s=1,v=1;AQID\x1b\\");
        let ack = TransferReply::for_transfer(&regular, true).unwrap();
        assert_eq!(ack.response(Ok(7)).unwrap(), b"\x1b_Gi=7;OK\x1b\\");
        assert!(TransferReply::for_transfer(&regular, false).is_none());
        let upload_and_place = assembled(b"\x1b_Ga=T,i=7,f=24,s=1,v=1;AQID\x1b\\");
        assert!(TransferReply::for_transfer(&upload_and_place, true).is_some());

        let quiet_ok = assembled(b"\x1b_Ga=t,i=7,f=24,s=1,v=1,q=1;AQID\x1b\\");
        let ack = TransferReply::for_transfer(&quiet_ok, true).unwrap();
        assert!(ack.response(Ok(7)).is_none());
        assert_eq!(
            ack.response(Err(StoreError::InvalidData)).unwrap(),
            b"\x1b_Gi=7;EINVAL:invalid image\x1b\\"
        );

        let quiet_all = assembled(b"\x1b_Ga=t,i=7,f=24,s=1,v=1,q=2;AQID\x1b\\");
        let ack = TransferReply::for_transfer(&quiet_all, true).unwrap();
        assert!(ack.response(Ok(7)).is_none());
        assert!(ack.response(Err(StoreError::InvalidData)).is_none());
    }

    #[test]
    fn transmit_and_place_ack_echoes_placement_and_maps_store_errors() {
        let placed = assembled(b"\x1b_Ga=T,i=7,p=9,f=24,s=1,v=1;AQID\x1b\\");
        let ack = TransferReply::for_transfer(&placed, true).unwrap();
        assert_eq!(ack.response(Ok(7)).unwrap(), b"\x1b_Gi=7,p=9;OK\x1b\\");
        assert_eq!(
            ack.response(Err(StoreError::InvalidPlacement)).unwrap(),
            b"\x1b_Gi=7,p=9;EINVAL:invalid placement\x1b\\"
        );
        assert_eq!(
            ack.response(Err(StoreError::InvalidData)).unwrap(),
            b"\x1b_Gi=7,p=9;EINVAL:invalid image\x1b\\"
        );
        assert_eq!(
            ack.response(Err(StoreError::TooLarge)).unwrap(),
            b"\x1b_Gi=7,p=9;E2BIG:image too large\x1b\\"
        );
        assert!(TransferReply::for_transfer(&placed, false).is_none());
        let anonymous = assembled(b"\x1b_Ga=T,i=7,p=0,f=24,s=1,v=1;AQID\x1b\\");
        assert_eq!(
            TransferReply::for_transfer(&anonymous, true)
                .unwrap()
                .response(Ok(7))
                .unwrap(),
            b"\x1b_Gi=7;OK\x1b\\"
        );
    }

    #[test]
    fn transmit_and_place_ack_respects_quiet_modes() {
        let quiet_ok = assembled(b"\x1b_Ga=T,i=7,p=9,q=1,f=24,s=1,v=1;AQID\x1b\\");
        let ack = TransferReply::for_transfer(&quiet_ok, true).unwrap();
        assert!(ack.response(Ok(7)).is_none());
        assert!(ack.response(Err(StoreError::InvalidData)).is_some());
        let quiet_all = assembled(b"\x1b_Ga=T,i=7,p=9,q=2,f=24,s=1,v=1;AQID\x1b\\");
        let ack = TransferReply::for_transfer(&quiet_all, true).unwrap();
        assert!(ack.response(Ok(7)).is_none());
        assert!(ack.response(Err(StoreError::InvalidData)).is_none());
    }

    #[test]
    fn placement_ack_includes_named_id_and_distinguishes_missing_image() {
        let command = b"\x1b_Ga=p,i=7,p=9,q=0\x1b\\";
        let ack = PlacementReply::for_command(command, true).unwrap();
        assert_eq!(
            ack.response(None, Some(7)).unwrap(),
            b"\x1b_Gi=7,p=9;OK\x1b\\"
        );
        assert_eq!(
            ack.response(Some(StoreError::MissingImage), None).unwrap(),
            b"\x1b_Gi=7,p=9;ENOENT:image not found\x1b\\"
        );
        assert!(PlacementReply::for_command(command, false).is_none());
        assert!(PlacementReply::for_command(b"\x1b_Ga=d,i=7\x1b\\", true).is_none());

        let anonymous = PlacementReply::for_command(b"\x1b_Ga=p,i=7,p=0\x1b\\", true).unwrap();
        assert_eq!(
            anonymous.response(None, Some(7)).unwrap(),
            b"\x1b_Gi=7;OK\x1b\\"
        );
        let invalid = PlacementReply::for_command(b"\x1b_Ga=p,i=7,p=bad\x1b\\", true).unwrap();
        assert_eq!(
            invalid
                .response(Some(StoreError::InvalidPlacement), None)
                .unwrap(),
            b"\x1b_Gi=7;EINVAL:invalid placement\x1b\\"
        );
    }

    #[test]
    fn placement_ack_respects_quiet_modes() {
        let quiet_ok = PlacementReply::for_command(b"\x1b_Ga=p,i=7,q=1\x1b\\", true).unwrap();
        assert!(quiet_ok.response(None, Some(7)).is_none());
        assert_eq!(
            quiet_ok
                .response(Some(StoreError::MissingImage), None)
                .unwrap(),
            b"\x1b_Gi=7;ENOENT:image not found\x1b\\"
        );
        let quiet_all = PlacementReply::for_command(b"\x1b_Ga=p,i=7,q=2\x1b\\", true).unwrap();
        assert!(quiet_all.response(None, Some(7)).is_none());
        assert!(
            quiet_all
                .response(Some(StoreError::MissingImage), None)
                .is_none()
        );
    }

    #[test]
    fn numbered_placement_ack_reports_resolved_id_or_missing_number() {
        let numbered = PlacementReply::for_command(b"\x1b_Ga=p,I=13,p=9\x1b\\", true).unwrap();
        assert_eq!(
            numbered.response(None, Some(42)).unwrap(),
            b"\x1b_Gi=42,I=13,p=9;OK\x1b\\"
        );
        assert_eq!(
            numbered
                .response(Some(StoreError::MissingImage), None)
                .unwrap(),
            b"\x1b_GI=13,p=9;ENOENT:image not found\x1b\\"
        );
        let ambiguous = PlacementReply::for_command(b"\x1b_Ga=p,i=7,I=13,p=9\x1b\\", true).unwrap();
        assert_eq!(
            ambiguous
                .response(Some(StoreError::UnsupportedIdentity), None)
                .unwrap(),
            b"\x1b_Gi=7,I=13,p=9;EINVAL:invalid placement\x1b\\"
        );
    }
}

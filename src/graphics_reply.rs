//! Child-facing replies for completed, supported Kitty direct-data commands.
//! Query replies do not insert or replace an image.

use crate::{
    graphics_store::{ImageFormat, StoreError, StoredImage},
    graphics_transfer::AssembledDirectTransfer,
};

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
    let format = match transfer.control(b'f') {
        None | Some(b"32") => Some(ImageFormat::Rgba),
        Some(b"24") => Some(ImageFormat::Rgb),
        Some(b"100") => Some(ImageFormat::Png),
        _ => None,
    };
    let supported_controls = transfer.supported_data_only_controls();
    let declared_width = transfer.control(b's').and_then(parse_nonzero);
    let declared_height = transfer.control(b'v').and_then(parse_nonzero);
    let valid = format.is_some_and(|format| {
        supported_controls
            && StoredImage {
                format,
                declared_width,
                declared_height,
                data: transfer.data,
            }
            .decode_rgba()
            .is_ok()
    });
    if valid && quiet == Some(b'1') {
        return None;
    }
    let message = if valid { "OK" } else { "EINVAL:invalid image" };
    Some(encode_reply(id, message))
}

/// Capture the identity of a data-only upload before the transfer is moved
/// into the image store. Only a completed transfer gets one reply.
#[derive(Clone, Copy)]
pub(crate) struct UploadReply {
    id: u32,
    quiet: u8,
}

impl UploadReply {
    pub(crate) fn for_transfer(
        transfer: &AssembledDirectTransfer,
        can_display: bool,
    ) -> Option<Self> {
        if !can_display || !matches!(transfer.control(b'a'), None | Some(b"t")) {
            return None;
        }
        Some(Self {
            id: parse_nonzero(transfer.control(b'i')?)?,
            quiet: transfer
                .control(b'q')
                .and_then(|value| value.first())
                .copied()
                .unwrap_or(b'0'),
        })
    }

    pub(crate) fn response(self, error: Option<StoreError>) -> Option<Vec<u8>> {
        if self.quiet == b'2' || (self.quiet == b'1' && error.is_none()) {
            return None;
        }
        let message = match error {
            None => "OK",
            Some(StoreError::TooLarge) => "E2BIG:image too large",
            Some(_) => "EINVAL:invalid image",
        };
        Some(encode_reply(self.id, message))
    }
}

fn encode_reply(id: u32, message: &str) -> Vec<u8> {
    let response = format!("\x1b_Gi={id};{message}\x1b\\").into_bytes();
    debug_assert!(response.len() <= crate::parser::MAX_REPLY_BYTES);
    response
}

fn parse_nonzero(bytes: &[u8]) -> Option<u32> {
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes)
        .ok()?
        .parse::<u32>()
        .ok()
        .filter(|&value| value != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics_transfer::DirectTransferAssembler;

    fn assembled(command: &[u8]) -> AssembledDirectTransfer {
        DirectTransferAssembler::new().accept(command).unwrap()
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
    fn unknown_query_control_must_not_get_false_success() {
        let query = b"\x1b_Gi=7,a=q,t=d,f=24,s=1,v=1,U=1;AQID\x1b\\";
        assert_eq!(
            direct_query_reply(assembled(query), true).unwrap(),
            b"\x1b_Gi=7;EINVAL:invalid image\x1b\\"
        );
    }

    #[test]
    fn upload_ack_uses_id_and_respects_quiet_modes() {
        let regular = assembled(b"\x1b_Ga=t,i=7,f=24,s=1,v=1;AQID\x1b\\");
        let ack = UploadReply::for_transfer(&regular, true).unwrap();
        assert_eq!(ack.response(None).unwrap(), b"\x1b_Gi=7;OK\x1b\\");
        assert!(UploadReply::for_transfer(&regular, false).is_none());
        let upload_and_place = assembled(b"\x1b_Ga=T,i=7,f=24,s=1,v=1;AQID\x1b\\");
        assert!(UploadReply::for_transfer(&upload_and_place, true).is_none());

        let quiet_ok = assembled(b"\x1b_Ga=t,i=7,f=24,s=1,v=1,q=1;AQID\x1b\\");
        let ack = UploadReply::for_transfer(&quiet_ok, true).unwrap();
        assert!(ack.response(None).is_none());
        assert_eq!(
            ack.response(Some(StoreError::InvalidData)).unwrap(),
            b"\x1b_Gi=7;EINVAL:invalid image\x1b\\"
        );

        let quiet_all = assembled(b"\x1b_Ga=t,i=7,f=24,s=1,v=1,q=2;AQID\x1b\\");
        let ack = UploadReply::for_transfer(&quiet_all, true).unwrap();
        assert!(ack.response(None).is_none());
        assert!(ack.response(Some(StoreError::InvalidData)).is_none());
    }
}

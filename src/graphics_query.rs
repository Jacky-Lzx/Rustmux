//! Child-facing replies for completed, direct-data Kitty graphics queries.
//! A query is never inserted into the pane's image store.

use crate::{
    graphics_store::{ImageFormat, StoredImage},
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
    let supported_controls = transfer.supported_query_controls();
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
    let response = format!("\x1b_Gi={id};{message}\x1b\\").into_bytes();
    debug_assert!(response.len() <= crate::parser::MAX_REPLY_BYTES);
    Some(response)
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
}

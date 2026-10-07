//! Small XTGETTCAP allowlist, independent of the host's terminal database.

pub(crate) fn reply(names: &[u8]) -> Vec<u8> {
    // The display parser retains at most 64 bytes, including the +q prefix.
    debug_assert!(names.len() <= 62);
    let mut response = b"\x1bP1+r".to_vec();
    let mut found = false;
    for encoded in names.split(|byte| *byte == b';') {
        let Some(value) = value(encoded) else {
            // XTerm stops the list at its first unknown or malformed name.
            break;
        };
        if found {
            response.push(b';');
        }
        response.extend_from_slice(encoded);
        response.push(b'=');
        response.extend_from_slice(value);
        found = true;
    }
    if !found {
        response[2] = b'0';
    }
    response.extend_from_slice(b"\x1b\\");
    // At most 12 shortest names fit; each adds at most seven value bytes.
    // This remains below the existing palette-query reply reservation.
    debug_assert!(response.len() <= crate::parser::MAX_REPLY_BYTES);
    response
}

fn value(encoded: &[u8]) -> Option<&'static [u8]> {
    if encoded.is_empty() || !encoded.len().is_multiple_of(2) || encoded.len() > 12 {
        return None;
    }
    let mut name = [0; 6];
    for (slot, pair) in name.iter_mut().zip(encoded.as_chunks::<2>().0) {
        *slot = hex(pair[0])? * 16 + hex(pair[1])?;
    }
    match &name[..encoded.len() / 2] {
        b"Co" | b"colors" => Some(b"323536"), // "256"
        b"RGB" => Some(b"38"),                // "8" bits per RGB component
        _ => None,
    }
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

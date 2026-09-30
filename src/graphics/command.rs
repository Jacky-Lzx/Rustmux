//! Shared lexical parsing for bounded Kitty data and control-only APCs.
//! Action, image format, and placement validation belong to their consumers.

use std::collections::BTreeMap;

use super::MAX_GRAPHICS_COMMAND_BYTES;

/// Kitten icat's largest observed encoded chunk; larger APCs remain bounded.
pub const MAX_ENCODED_CHUNK_BYTES: usize = 128 * 1024;

pub(crate) type Controls = BTreeMap<u8, Vec<u8>>;

/// Data commands require a semicolon, even when controls or payload are empty.
/// Keep the payload borrowed; its medium and Base64 validation happen later.
pub(crate) fn parse_command(command: &[u8]) -> Option<(Controls, &[u8])> {
    let body = command_body(command)?;
    let separator = body.iter().position(|&byte| byte == b';')?;
    let encoded = &body[separator + 1..];
    if encoded.len() > MAX_ENCODED_CHUNK_BYTES {
        return None;
    }
    Some((parse_controls(&body[..separator])?, encoded))
}

/// Control-only commands allow one trailing semicolon but no image payload.
/// Store mutation and child placement replies share quiet-mode validation.
pub(crate) fn parse_control_command(command: &[u8]) -> Option<Controls> {
    let body = command_body(command)?;
    let body = body.strip_suffix(b";").unwrap_or(body);
    if body.is_empty() || body.contains(&b';') {
        return None;
    }
    let controls = parse_controls(body)?;
    valid_quiet(&controls).then_some(controls)
}

pub(crate) fn valid_quiet(controls: &Controls) -> bool {
    matches!(
        controls.get(&b'q').map(Vec::as_slice),
        None | Some(b"0" | b"1" | b"2")
    )
}

fn command_body(command: &[u8]) -> Option<&[u8]> {
    if command.len() > MAX_GRAPHICS_COMMAND_BYTES {
        return None;
    }
    if let Some(bytes) = command.strip_prefix(b"\x1b_G") {
        bytes.strip_suffix(b"\x1b\\")
    } else {
        let bytes = command.strip_prefix(&[0x9f, b'G'])?;
        bytes
            .strip_suffix(&[0x9c])
            .or_else(|| bytes.strip_suffix(b"\x1b\\"))
    }
}

fn parse_controls(bytes: &[u8]) -> Option<Controls> {
    let mut controls = Controls::new();
    if !bytes.is_empty() {
        for pair in bytes.split(|&byte| byte == b',') {
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
    }
    Some(controls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_framing_preserves_controls_and_payload() {
        let framing: &[(&[u8], &[u8])] = &[
            (b"\x1b_G", b"\x1b\\"),
            (b"\x9fG", b"\x9c"),
            (b"\x9fG", b"\x1b\\"),
        ];
        for &(prefix, suffix) in framing {
            let command = [prefix, b"a=t,f=24,s=1,v=1;AQID", suffix].concat();
            let (controls, payload) = parse_command(&command).unwrap();
            assert_eq!(controls.get(&b'f').unwrap(), b"24");
            assert_eq!(payload, b"AQID");
            let command = [prefix, b"a=p,i=7,p=2;", suffix].concat();
            let controls = parse_control_command(&command).unwrap();
            assert_eq!(controls.get(&b'i').unwrap(), b"7");
            assert_eq!(controls.get(&b'p').unwrap(), b"2");
        }
        for command in [
            b"\x1b_Ga=d\x9c".as_slice(),
            b"\x1b_Ga=d".as_slice(),
            b"\x9fGa=d".as_slice(),
            b"\x1b_a=d\x1b\\".as_slice(),
            b"\x1b_Ga=d\x1b\\extra".as_slice(),
        ] {
            assert!(parse_command(command).is_none());
            assert!(parse_control_command(command).is_none());
        }
    }

    #[test]
    fn data_and_control_commands_keep_distinct_separator_rules() {
        for command in [b"\x1b_Ga=d\x1b\\".as_slice(), b"\x1b_Ga=d;\x1b\\"] {
            assert_eq!(
                parse_control_command(command).unwrap().get(&b'a').unwrap(),
                b"d"
            );
        }
        assert!(parse_command(b"\x1b_Ga=d\x1b\\").is_none());
        let (controls, payload) = parse_command(b"\x1b_G;AQID\x1b\\").unwrap();
        assert!(controls.is_empty());
        assert_eq!(payload, b"AQID");
        for command in [
            b"\x1b_G\x1b\\".as_slice(),
            b"\x1b_G;\x1b\\".as_slice(),
            b"\x1b_Ga=d;AQID\x1b\\".as_slice(),
            b"\x1b_Ga=d;;\x1b\\".as_slice(),
        ] {
            assert!(parse_control_command(command).is_none());
        }
    }

    #[test]
    fn malformed_or_duplicate_fields_fail_in_both_command_kinds() {
        for fields in [
            b"a=t,a=q".as_slice(),
            b"a=".as_slice(),
            b"ab=t".as_slice(),
            b"1=t".as_slice(),
            b"=t".as_slice(),
            b"a".as_slice(),
            b",a=t".as_slice(),
            b"a=t,".as_slice(),
            b"a= t".as_slice(),
            b"a=\n".as_slice(),
            b"a=\xff".as_slice(),
        ] {
            let control = [b"\x1b_G", fields, b"\x1b\\"].concat();
            let data = [b"\x1b_G", fields, b";AQID\x1b\\"].concat();
            assert!(parse_control_command(&control).is_none(), "{fields:?}");
            assert!(parse_command(&data).is_none(), "{fields:?}");
        }
    }

    #[test]
    fn data_consumers_validate_quiet_after_lexical_parsing() {
        for quiet in ["0", "1", "2", "3", "00"] {
            let command = format!("\x1b_Ga=t,q={quiet};AQID\x1b\\");
            let (controls, _) = parse_command(command.as_bytes()).unwrap();
            let supported = matches!(quiet, "0" | "1" | "2");
            assert_eq!(valid_quiet(&controls), supported);
            let command = format!("\x1b_Ga=p,i=7,q={quiet};\x1b\\");
            assert_eq!(
                parse_control_command(command.as_bytes()).is_some(),
                supported
            );
        }
    }

    #[test]
    fn payload_and_complete_command_limits_remain_independent() {
        let mut command = b"\x1b_G;".to_vec();
        command.extend(std::iter::repeat_n(b'A', MAX_ENCODED_CHUNK_BYTES));
        command.extend(b"\x1b\\");
        assert_eq!(
            parse_command(&command).unwrap().1.len(),
            MAX_ENCODED_CHUNK_BYTES
        );
        command.insert(command.len() - 2, b'A');
        assert!(command.len() < MAX_GRAPHICS_COMMAND_BYTES);
        assert!(parse_command(&command).is_none());

        let prefix = b"\x1b_Ga=";
        let suffix = b"\x1b\\";
        let mut control = prefix.to_vec();
        control.extend(std::iter::repeat_n(
            b'x',
            MAX_GRAPHICS_COMMAND_BYTES - prefix.len() - suffix.len(),
        ));
        control.extend(suffix);
        assert!(parse_control_command(&control).is_some());
        control.insert(control.len() - suffix.len(), b'x');
        assert!(parse_control_command(&control).is_none());
    }
}

//! Bounded OSC 21 operations over the existing pane color profile.
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use std::fmt::Write as _;

use crate::{parser::parse_color, screen::Screen};

#[derive(Clone, Copy)]
enum Key {
    Foreground,
    Background,
    Cursor,
    Palette(u8),
    Unknown,
}

fn key(name: &str) -> Key {
    match name {
        "foreground" => Key::Foreground,
        "background" => Key::Background,
        "cursor" => Key::Cursor,
        _ if name.bytes().all(|byte| byte.is_ascii_digit()) => {
            name.parse().map(Key::Palette).unwrap_or(Key::Unknown)
        }
        _ => Key::Unknown,
    }
}

pub(crate) fn apply(
    screen: &mut Screen,
    values: &[u8],
    bell_terminated: bool,
    reply: &mut impl FnMut(&[u8]),
) {
    // The parser caps the entire OSC payload at 64 bytes. Validate everything
    // before any state change or reply, as with the existing color commands.
    if !values.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
        return;
    }
    let values = std::str::from_utf8(values).expect("printable ASCII");
    for field in values.split(';') {
        let (name, value) = field
            .split_once('=')
            .map_or((field, None), |(name, value)| (name, Some(value)));
        if name.is_empty()
            || (!matches!(key(name), Key::Unknown)
                && value.is_some_and(|value| value != "?" && parse_color(value).is_none()))
        {
            return;
        }
    }

    let mut response = String::from("\x1b]21");
    for field in values.split(';') {
        let (name, value) = field
            .split_once('=')
            .map_or((field, None), |(name, value)| (name, Some(value)));
        let target = key(name);
        if matches!(target, Key::Unknown) {
            let _ = write!(response, ";unknown={}", STANDARD_NO_PAD.encode(name));
        } else if value == Some("?") {
            let (red, green, blue) = match target {
                Key::Foreground => screen.default_foreground(),
                Key::Background => screen.default_background(),
                Key::Cursor => screen.cursor_color(),
                Key::Palette(index) => screen.palette_color(index),
                Key::Unknown => unreachable!(),
            };
            let _ = write!(response, ";{name}=rgb:{red:02x}/{green:02x}/{blue:02x}");
        } else if let Some(value) = value {
            let color = parse_color(value).expect("validated color");
            match target {
                Key::Foreground => screen.set_default_foreground(color),
                Key::Background => screen.set_default_background(color),
                Key::Cursor => screen.set_cursor_color(color),
                Key::Palette(index) => screen.set_palette_color(index, color),
                Key::Unknown => unreachable!(),
            }
        } else {
            match target {
                Key::Foreground => screen.reset_default_foreground(),
                Key::Background => screen.reset_default_background(),
                Key::Cursor => screen.reset_cursor_color(),
                Key::Palette(index) => screen.reset_palette_color(index),
                Key::Unknown => unreachable!(),
            }
        }
    }
    if response.len() > 4 {
        response.push_str(if bell_terminated { "\x07" } else { "\x1b\\" });
        // A query field of at least four bytes expands by at most 15 bytes;
        // unknown fields of at least one byte expand to nine plus base64.
        // Even 31 one-byte unknown keys fit the existing 420-byte reply bound.
        debug_assert!(response.len() <= crate::parser::MAX_REPLY_BYTES);
        reply(response.as_bytes());
    }
}

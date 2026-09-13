use rustmux::{parser::Parser, render::render, screen::Screen};
use std::io::{self, Write};

fn screen(rows: usize, columns: usize, text: &str) -> Screen {
    let mut screen = Screen::new(rows, columns).unwrap();
    Parser::new().advance(&mut screen, text.as_bytes());
    screen
}

fn frame(screen: &Screen) -> Vec<u8> {
    let mut bytes = Vec::new();
    render(screen, &mut bytes).unwrap();
    bytes
}

#[test]
fn full_grid_has_explicit_rows_and_never_uses_newlines() {
    let screen = screen(2, 3, "abcdef");
    let before = screen.clone();
    assert_eq!(
        frame(&screen),
        b"\x1b[?25l\x1b[0m\x1b[?2004l\x1b[?1l\x1b>\x1b[1 q\x1b[?1004l\x1b[1;1Habc\x1b[2;1Hdef\x1b[0m\x1b[2;3H\x1b[?25h"
    );
    assert_eq!(screen, before);
    assert!(screen.wrap_pending());
}

#[test]
fn exact_style_encoding_resets_attributes_between_cells() {
    let screen = screen(1, 3, "\x1b[1;2;3;4;5;7;8;9;31;48;2;0;127;255mAB\x1b[0mC");
    assert_eq!(frame(&screen), b"\x1b[?25l\x1b[0m\x1b[?2004l\x1b[?1l\x1b>\x1b[1 q\x1b[?1004l\x1b[1;1H\x1b[0;1;2;3;4;5;7;8;9;38;5;1;48;2;0;127;255mAB\x1b[0mC\x1b[0m\x1b[1;3H\x1b[?25h");
}

#[test]
fn wide_leaders_and_suffixes_are_emitted_once() {
    let screen = screen(1, 4, "中e\u{301}X");
    assert_eq!(
        frame(&screen),
        "\x1b[?25l\x1b[0m\x1b[?2004l\x1b[?1l\x1b>\x1b[1 q\x1b[?1004l\x1b[1;1H中e\u{301}X\x1b[0m\x1b[1;4H\x1b[?25h"
            .as_bytes()
    );
}

fn redraw_matches(source: &Screen, target: &mut Screen) {
    // Replaying the emitted ANSI catches lost spaces, glyphs and style transitions.
    // Exact byte fixtures above provide checks independent of our parser.
    let output = frame(source);
    let mut parser = Parser::new();
    for byte in output {
        parser.advance(target, &[byte]);
    }
    for row in 0..source.dimensions().0 {
        assert_eq!(target.row(row), source.row(row));
    }
    assert_eq!(target.cursor(), source.cursor());
}

#[test]
fn redraw_clears_stale_text_and_handles_alternate_and_resize() {
    let mut source = screen(2, 6, "\x1b[31m中文\r\n\x1b[38;5;255mX");
    let mut target = screen(2, 6, "\x1b[1;44mxxxxxxxxxxxx");
    redraw_matches(&source, &mut target);
    Parser::new().advance(&mut source, b"\x1b[?1049h\x1b[H\x1b[0mALT");
    redraw_matches(&source, &mut target);
    source.leave_alternate();
    redraw_matches(&source, &mut target);
    source.resize(1, 3).unwrap();
    target.resize(1, 3).unwrap();
    redraw_matches(&source, &mut target);
    let one = screen(1, 1, "X");
    redraw_matches(&one, &mut Screen::new(1, 1).unwrap());
}

struct ShortWriter {
    bytes: Vec<u8>,
    interrupted: bool,
    limit: Option<usize>,
}
impl Write for ShortWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.limit.is_some_and(|limit| self.bytes.len() >= limit) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let remaining = self
            .limit
            .map_or(usize::MAX, |limit| limit - self.bytes.len());
        let count = bytes.len().min(2).min(remaining);
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("render must not flush")
    }
}

#[test]
fn partial_writes_retry_interruptions_and_propagate_failures() {
    let screen = screen(1, 4, "\x1b[31m中X");
    let expected = frame(&screen);
    let mut writer = ShortWriter {
        bytes: Vec::new(),
        interrupted: false,
        limit: None,
    };
    render(&screen, &mut writer).unwrap();
    assert_eq!(writer.bytes, expected);
    let mut writer = ShortWriter {
        bytes: Vec::new(),
        interrupted: false,
        limit: Some(20),
    };
    assert_eq!(
        render(&screen, &mut writer).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(writer.bytes, expected[..20]);
}

//! Full-frame ANSI output for a screen model. No event loop or output queue here.

use std::io::{self, Write};

use crate::screen::Screen;
use crate::style::{Color, Style};

/// Draw the active grid from the top-left corner onto an equally sized terminal.
///
/// The caller owns terminal setup/restoration and must use normal origin mode,
/// a full-screen scrolling region and compatible character-width rules. This
/// writes a full frame, including blank cells, without switching screen buffers.
/// On success attributes are reset; cursor position, visibility and supported
/// input modes match the model.
/// It does not flush. Errors may leave a partial frame; the caller must handle
/// cleanup or redraw. For nonblocking output, render into a buffer and queue it.
pub fn render(screen: &Screen, output: &mut impl Write) -> io::Result<()> {
    output.write_all(b"\x1b[?25l\x1b[0m")?;
    // The single active pane determines how the outer terminal encodes paste.
    // Input forwarding preserves the resulting start/end markers unchanged.
    output.write_all(if screen.bracketed_paste() {
        b"\x1b[?2004h"
    } else {
        b"\x1b[?2004l"
    })?;
    // Let the outer terminal encode cursor keys for the child; the input loop
    // forwards those bytes without translating CSI/SS3 or modified keys.
    output.write_all(if screen.application_cursor_keys() {
        b"\x1b[?1h"
    } else {
        b"\x1b[?1l"
    })?;
    let mut style = Style::default();
    for row in 0..screen.dimensions().0 {
        // Explicit CUP avoids newline-induced scrolling, including at bottom-right.
        write!(output, "\x1b[{};1H", row + 1)?;
        for cell in screen.row(row).expect("row is in bounds") {
            if cell.width == 0 {
                continue;
            }
            if cell.style != style {
                write_style(output, cell.style)?;
                style = cell.style;
            }
            let mut bytes = [0; 4];
            output.write_all(cell.character.encode_utf8(&mut bytes).as_bytes())?;
            for character in &cell.combining {
                output.write_all(character.encode_utf8(&mut bytes).as_bytes())?;
            }
        }
    }
    let (row, column) = screen.cursor();
    // CUP also cancels physical delayed wrap; logical pending wrap stays in Screen.
    write!(output, "\x1b[0m\x1b[{};{}H", row + 1, column + 1)?;
    output.write_all(if screen.cursor_visible() {
        b"\x1b[?25h"
    } else {
        b"\x1b[?25l"
    })
}

fn write_style(output: &mut impl Write, style: Style) -> io::Result<()> {
    // Start from reset so attributes absent from the next cell cannot leak.
    output.write_all(b"\x1b[0")?;
    for (enabled, code) in [
        (style.bold, 1),
        (style.dim, 2),
        (style.italic, 3),
        (style.underline, 4),
        (style.blink, 5),
        (style.inverse, 7),
        (style.hidden, 8),
        (style.strikethrough, 9),
    ] {
        if enabled {
            write!(output, ";{code}")?;
        }
    }
    write_color(output, style.foreground, 38)?;
    write_color(output, style.background, 48)?;
    output.write_all(b"m")
}

fn write_color(output: &mut impl Write, color: Color, selector: u8) -> io::Result<()> {
    match color {
        Color::Default => Ok(()), // The leading reset already selects default colors.
        Color::Indexed(index) => write!(output, ";{selector};5;{index}"),
        Color::Rgb(red, green, blue) => write!(output, ";{selector};2;{red};{green};{blue}"),
    }
}

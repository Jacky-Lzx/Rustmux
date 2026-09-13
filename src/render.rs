//! Full-frame and changed-row ANSI output. No event loop or output queue here.

use std::io::{self, Write};

use crate::screen::{MouseTracking, Screen};
use crate::style::{Cell, Color, Style};

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
    render_frame(screen, output, true, true, None)
}

/// Changed-row rendering for an ordered output stream. Each successful frame must
/// be delivered completely before the next; create a fresh Renderer after losing
/// or discarding output, or call invalidate. Avoid re-enabling focus reports on ordinary redraws,
/// since an outer terminal may report its current focus when enabled.
#[derive(Default)]
pub struct Renderer {
    focus_reporting: Option<bool>,
    mouse: Option<(MouseTracking, bool)>,
    dimensions: Option<(usize, usize)>,
    rows: Vec<Vec<Cell>>,
}

impl Renderer {
    /// Force a complete repaint after external damage or discarded queued output.
    pub fn invalidate(&mut self) {
        self.dimensions = None;
        self.focus_reporting = None;
        self.mouse = None;
    }

    pub fn render(&mut self, screen: &Screen, output: &mut impl Write) -> io::Result<()> {
        let synchronize = self.focus_reporting != Some(screen.focus_reporting());
        let mouse = (screen.mouse_tracking(), screen.sgr_mouse());
        let previous =
            (self.dimensions == Some(screen.dimensions())).then_some(self.rows.as_slice());
        if let Err(error) = render_frame(
            screen,
            output,
            synchronize,
            self.mouse != Some(mouse),
            previous,
        ) {
            // Partly written rows can no longer be compared against the old grid.
            self.invalidate();
            return Err(error);
        }
        if self.dimensions != Some(screen.dimensions()) {
            self.rows.clear();
        }
        self.rows.resize_with(screen.dimensions().0, Vec::new);
        for (index, cached) in self.rows.iter_mut().enumerate() {
            let cells = screen.row(index).expect("row is in bounds");
            if cached.as_slice() != cells {
                cached.clear();
                cached.extend_from_slice(cells);
            }
        }
        self.dimensions = Some(screen.dimensions());
        self.mouse = Some(mouse);
        self.focus_reporting = Some(screen.focus_reporting());
        Ok(())
    }
}

fn render_frame(
    screen: &Screen,
    output: &mut impl Write,
    synchronize_focus: bool,
    synchronize_mouse: bool,
    previous: Option<&[Vec<Cell>]>,
) -> io::Result<()> {
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
    // Numeric keypad mode is separate from application cursor keys.
    output.write_all(if screen.application_keypad() {
        b"\x1b="
    } else {
        b"\x1b>"
    })?;
    // Set shape while hidden; the frame ending restores requested visibility.
    write!(output, "\x1b[{} q", screen.cursor_shape() as u8)?;
    if synchronize_focus {
        output.write_all(if screen.focus_reporting() {
            b"\x1b[?1004h"
        } else {
            b"\x1b[?1004l"
        })?;
    }
    if synchronize_mouse {
        // Clear old tracking before selecting the new exclusive mode. Set the
        // encoding first so the first new event uses the requested format.
        output.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l")?;
        output.write_all(if screen.sgr_mouse() {
            b"\x1b[?1006h"
        } else {
            b"\x1b[?1006l"
        })?;
        if screen.mouse_tracking() != MouseTracking::Off {
            write!(output, "\x1b[?{}h", screen.mouse_tracking() as u16)?;
        }
    }
    let mut style = Style::default();
    for row in 0..screen.dimensions().0 {
        let cells = screen.row(row).expect("row is in bounds");
        if previous.is_some_and(|cached| cached[row].as_slice() == cells) {
            continue;
        }
        // Explicit CUP avoids newline-induced scrolling, including at bottom-right.
        write!(output, "\x1b[{};1H", row + 1)?;
        for cell in cells {
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

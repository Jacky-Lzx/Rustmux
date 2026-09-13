//! Full-frame and changed-cell ANSI output. No event loop or output queue here.

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

/// Changed-cell rendering for an ordered output stream. Each successful frame must
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
        if let Some(cached) = previous {
            let ranges = changed_ranges(&cached[row], cells);
            // A whole changed row needs no alternative plan or temporary output.
            if ranges.len() == 1 && ranges[0] == (0..cells.len()) {
                write_run(output, row, 0, cells, &mut style)?;
                continue;
            }
            let mut partial = Vec::new();
            let mut partial_style = style;
            let mut previous_end = None;
            for range in ranges {
                if let Some(end) = previous_end {
                    let gap = &cells[end..range.start];
                    if bridge_cost(gap, cells[range.start].style, partial_style)?
                        < restart_cost(row, range.start, cells[range.start].style, partial_style)?
                    {
                        write_cells(&mut partial, gap, &mut partial_style)?;
                        write_cells(&mut partial, &cells[range.clone()], &mut partial_style)?;
                        previous_end = Some(range.end);
                        continue;
                    }
                }
                previous_end = Some(range.end);
                write_run(
                    &mut partial,
                    row,
                    range.start,
                    &cells[range],
                    &mut partial_style,
                )?;
            }
            // Include positioning, SGR and UTF-8 bytes, not just changed-cell count.
            if partial.len() < row_cost(row, cells, style)? {
                output.write_all(&partial)?;
                style = partial_style;
                continue;
            }
        }
        write_run(output, row, 0, cells, &mut style)?;
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

// Expand both old and new wide glyphs before merging overlapping ranges. A
// replacement must erase the old trailing cell and never begin on a placeholder.
fn changed_ranges(old: &[Cell], cells: &[Cell]) -> Vec<std::ops::Range<usize>> {
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let mut column = 0;
    while column < cells.len() {
        if old[column] == cells[column] {
            column += 1;
            continue;
        }
        let mut start = column;
        while column < cells.len() && old[column] != cells[column] {
            column += 1;
        }
        while start > 0 && (old[start].width == 0 || cells[start].width == 0) {
            start -= 1;
        }
        while column < cells.len() && (old[column].width == 0 || cells[column].width == 0) {
            column += 1;
        }
        if let Some(last) = ranges.last_mut().filter(|last| last.end >= start) {
            last.end = column;
        } else {
            ranges.push(start..column);
        }
    }
    ranges
}

fn write_run(
    output: &mut impl Write,
    row: usize,
    column: usize,
    cells: &[Cell],
    style: &mut Style,
) -> io::Result<()> {
    // CUP cancels delayed wrap and positions each span independently.
    write!(output, "\x1b[{};{}H", row + 1, column + 1)?;
    write_cells(output, cells, style)
}

fn write_cells(output: &mut impl Write, cells: &[Cell], style: &mut Style) -> io::Result<()> {
    for cell in cells {
        if cell.width == 0 {
            continue;
        }
        if cell.style != *style {
            write_style(output, cell.style)?;
            *style = cell.style;
        }
        let mut bytes = [0; 4];
        output.write_all(cell.character.encode_utf8(&mut bytes).as_bytes())?;
        for character in &cell.combining {
            output.write_all(character.encode_utf8(&mut bytes).as_bytes())?;
        }
    }
    Ok(())
}

#[derive(Default)]
struct ByteCount(usize);
impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn row_cost(row: usize, cells: &[Cell], mut style: Style) -> io::Result<usize> {
    let mut count = ByteCount::default();
    write!(&mut count, "\x1b[{};1H", row + 1)?;
    count_cells(&mut count, cells, &mut style)?;
    Ok(count.0)
}

fn count_cells(count: &mut ByteCount, cells: &[Cell], style: &mut Style) -> io::Result<()> {
    for cell in cells {
        if cell.width == 0 {
            continue;
        }
        count_style(count, cell.style, style)?;
        count.0 += cell.character.len_utf8();
        count.0 += cell.combining.iter().map(|c| c.len_utf8()).sum::<usize>();
    }
    Ok(())
}

fn count_style(count: &mut ByteCount, next: Style, style: &mut Style) -> io::Result<()> {
    if next != *style {
        write_style(count, next)?;
        *style = next;
    }
    Ok(())
}

// Both alternatives finish at the next span's first style. Its glyph and the
// rest of the span therefore have identical costs and need not be counted.
// Wide-glyph boundaries have already been expanded by changed_ranges.
fn bridge_cost(gap: &[Cell], next: Style, mut style: Style) -> io::Result<usize> {
    let mut count = ByteCount::default();
    count_cells(&mut count, gap, &mut style)?;
    count_style(&mut count, next, &mut style)?;
    Ok(count.0)
}

fn restart_cost(row: usize, column: usize, next: Style, mut style: Style) -> io::Result<usize> {
    let mut count = ByteCount::default();
    write!(&mut count, "\x1b[{};{}H", row + 1, column + 1)?;
    count_style(&mut count, next, &mut style)?;
    Ok(count.0)
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

#[cfg(test)]
mod cost_tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    fn row_cost_matches_encoded_bytes_including_styles_and_unicode() {
        let mut screen = Screen::new(1, 20).unwrap();
        Parser::new().advance(
            &mut screen,
            "中e\u{301}\x1b[1;38;2;7;8;9mX\x1b[0mY".as_bytes(),
        );
        for initial in [
            Style::default(),
            Style {
                bold: true,
                ..Style::default()
            },
        ] {
            for row in [0, 9, 999] {
                let mut bytes = Vec::new();
                let mut style = initial;
                write_run(&mut bytes, row, 0, screen.row(0).unwrap(), &mut style).unwrap();
                assert_eq!(
                    row_cost(row, screen.row(0).unwrap(), initial).unwrap(),
                    bytes.len()
                );
            }
        }
    }
}

//! Small terminal picker used by `rustmux attach` when no name is supplied.

use std::fmt::Write as _;
use std::io;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, poll};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{SessionInfo, SessionName};
use crate::terminal_device::TerminalDevice;

const POLL_MILLIS: u16 = 30;
const ESCAPE_DELAY: Duration = Duration::from_millis(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Key {
    Up,
    Down,
    Tab,
    Enter,
    Escape,
    Interrupt,
    Backspace,
    Character(u8),
}

#[derive(Default)]
struct InputDecoder {
    escape: Vec<u8>,
    escape_at: Option<Instant>,
}

impl InputDecoder {
    fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<Key> {
        let mut keys = Vec::new();
        for &byte in bytes {
            if self.escape.is_empty() {
                match byte {
                    27 => {
                        self.escape.push(byte);
                        self.escape_at = Some(now);
                    }
                    3 => keys.push(Key::Interrupt),
                    8 | 127 => keys.push(Key::Backspace),
                    b'\t' => keys.push(Key::Tab),
                    b'\r' | b'\n' => keys.push(Key::Enter),
                    _ => keys.push(Key::Character(byte)),
                }
                continue;
            }

            self.escape.push(byte);
            match self.escape.as_slice() {
                [27, b'['] | [27, b'O'] => {}
                [27, b'[', b'A'] | [27, b'O', b'A'] => {
                    keys.push(Key::Up);
                    self.clear_escape();
                }
                [27, b'[', b'B'] | [27, b'O', b'B'] => {
                    keys.push(Key::Down);
                    self.clear_escape();
                }
                _ => {
                    keys.push(Key::Escape);
                    self.clear_escape();
                }
            }
        }
        keys
    }

    fn flush_due(&mut self, now: Instant) -> Option<Key> {
        if !self.escape.is_empty()
            && self
                .escape_at
                .is_some_and(|start| now.saturating_duration_since(start) >= ESCAPE_DELAY)
        {
            self.clear_escape();
            Some(Key::Escape)
        } else {
            None
        }
    }

    fn clear_escape(&mut self) {
        self.escape.clear();
        self.escape_at = None;
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Choice {
    Attach(SessionName),
    Create(SessionName),
    Kill(SessionName),
    Cancel,
}

pub(super) fn choose(
    sessions: &[SessionInfo],
    initially_selected: Option<&SessionName>,
) -> io::Result<Choice> {
    let file = TerminalDevice::open_controlling()?;
    let signals = PickerSignals::install()?;
    let mut terminal = TerminalDevice::enter(file)?;
    let result = run_picker(&mut terminal, &signals, sessions, initially_selected);
    let restored = terminal.restore();
    result.and_then(|selection| restored.map(|()| selection))
}

fn run_picker(
    terminal: &mut TerminalDevice,
    signals: &PickerSignals,
    sessions: &[SessionInfo],
    initially_selected: Option<&SessionName>,
) -> io::Result<Choice> {
    let mut selected = initially_selected
        .and_then(|name| sessions.iter().position(|session| &session.name == name))
        .unwrap_or(0);
    let mut create_input: Option<String> = None;
    let mut search_input: Option<String> = None;
    let mut delete_armed: Option<SessionName> = None;
    let mut decoder = InputDecoder::default();
    let mut input = [0; 256];
    let mut previous_size = None;
    let mut dirty = true;

    loop {
        if let Some(signal) = signals.pending() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                format!("session selection interrupted by signal {signal}"),
            ));
        }

        let size = terminal.size()?;
        let size = (size.ws_row, size.ws_col);
        if dirty || previous_size != Some(size) {
            let visible = filtered_sessions(sessions, search_input.as_deref());
            selected = selected.min(visible.len().saturating_sub(1));
            terminal.write_all(&render(
                &visible,
                selected,
                size,
                create_input.as_deref(),
                search_input.as_deref(),
                delete_armed.as_ref(),
            ))?;
            previous_size = Some(size);
            dirty = false;
        }

        let event = {
            let mut descriptors = [PollFd::new(terminal.file().as_fd(), PollFlags::POLLIN)];
            match poll(&mut descriptors, POLL_MILLIS) {
                Ok(_) | Err(Errno::EINTR) => {}
                Err(error) => return Err(error.into()),
            }
            descriptors[0].revents().unwrap_or_else(PollFlags::empty)
        };

        let mut keys = Vec::new();
        if event.contains(PollFlags::POLLIN) {
            match nix::unistd::read(terminal.file(), &mut input) {
                Ok(0) => return Ok(Choice::Cancel),
                Ok(count) => keys.extend(decoder.feed(&input[..count], Instant::now())),
                Err(Errno::EINTR | Errno::EAGAIN) => {}
                Err(error) => return Err(error.into()),
            }
        } else if event.intersects(PollFlags::POLLHUP | PollFlags::POLLERR) {
            return Ok(Choice::Cancel);
        }
        if let Some(key) = decoder.flush_due(Instant::now()) {
            keys.push(key);
        }

        for key in keys {
            if let Some(name) = &mut create_input {
                match key {
                    Key::Enter => {
                        if let Ok(name) = SessionName::new(name.clone()) {
                            return Ok(Choice::Create(name));
                        }
                    }
                    Key::Escape => create_input = None,
                    Key::Interrupt => return Ok(Choice::Cancel),
                    Key::Backspace => {
                        name.pop();
                    }
                    Key::Character(byte)
                        if (byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                            && name.len() < super::MAX_SESSION_NAME_BYTES =>
                    {
                        name.push(char::from(byte));
                    }
                    Key::Up | Key::Down | Key::Tab | Key::Character(_) => {}
                }
                dirty = true;
                continue;
            }

            if let Some(query) = &mut search_input {
                let visible = filtered_sessions(sessions, Some(query));
                selected = selected.min(visible.len().saturating_sub(1));
                if let Some(next) = search_navigation_target(selected, visible.len(), key) {
                    selected = next;
                    dirty = true;
                    continue;
                }
                match key {
                    Key::Enter if !visible.is_empty() => {
                        return Ok(Choice::Attach(visible[selected].name.clone()));
                    }
                    Key::Tab if !visible.is_empty() => {
                        *query = visible[selected].name.as_str().to_owned();
                        selected = 0;
                    }
                    Key::Escape => {
                        let selected_name =
                            visible.get(selected).map(|session| session.name.clone());
                        search_input = None;
                        selected = selected_name
                            .as_ref()
                            .and_then(|name| {
                                sessions.iter().position(|session| &session.name == name)
                            })
                            .unwrap_or(0);
                    }
                    Key::Interrupt => return Ok(Choice::Cancel),
                    Key::Backspace => {
                        query.pop();
                        selected = 0;
                    }
                    Key::Character(byte)
                        if byte.is_ascii_graphic()
                            && query.len() < super::MAX_SESSION_NAME_BYTES =>
                    {
                        query.push(char::from(byte));
                        selected = 0;
                    }
                    Key::Up | Key::Down | Key::Tab | Key::Enter | Key::Character(_) => {}
                }
                dirty = true;
                continue;
            }

            let armed = delete_armed.take();
            if let Some(next) = navigation_target(selected, sessions.len(), key) {
                selected = next;
                dirty = true;
                continue;
            }
            match key {
                Key::Enter if !sessions.is_empty() => {
                    return Ok(Choice::Attach(sessions[selected].name.clone()));
                }
                Key::Character(b'a') => create_input = Some(String::new()),
                Key::Character(b'/') => search_input = Some(String::new()),
                Key::Character(b'd') if !sessions.is_empty() => {
                    let name = sessions[selected].name.clone();
                    if armed.as_ref() == Some(&name) {
                        return Ok(Choice::Kill(name));
                    }
                    delete_armed = Some(name);
                }
                Key::Escape | Key::Interrupt | Key::Character(b'q') => {
                    return Ok(Choice::Cancel);
                }
                Key::Backspace
                | Key::Tab
                | Key::Character(_)
                | Key::Up
                | Key::Down
                | Key::Enter => {}
            }
            dirty = true;
        }
    }
}

fn filtered_sessions(sessions: &[SessionInfo], query: Option<&str>) -> Vec<SessionInfo> {
    let query = query.unwrap_or_default().to_ascii_lowercase();
    sessions
        .iter()
        .filter(|session| session.name.as_str().to_ascii_lowercase().contains(&query))
        .cloned()
        .collect()
}

fn navigation_target(selected: usize, session_count: usize, key: Key) -> Option<usize> {
    if session_count == 0 {
        return None;
    }
    match key {
        Key::Up | Key::Character(b'k') => {
            Some(selected.checked_sub(1).unwrap_or(session_count - 1))
        }
        Key::Down | Key::Character(b'j') => Some((selected + 1) % session_count),
        _ => None,
    }
}

fn search_navigation_target(selected: usize, session_count: usize, key: Key) -> Option<usize> {
    if session_count == 0 {
        return None;
    }
    match key {
        Key::Up => Some(selected.checked_sub(1).unwrap_or(session_count - 1)),
        Key::Down => Some((selected + 1) % session_count),
        _ => None,
    }
}

const BASE: (u8, u8, u8) = (30, 30, 46);
const SURFACE: (u8, u8, u8) = (49, 50, 68);
const TEXT: (u8, u8, u8) = (205, 214, 244);
const MUTED: (u8, u8, u8) = (166, 173, 200);
const BLUE: (u8, u8, u8) = (137, 180, 250);
const TEAL: (u8, u8, u8) = (148, 226, 213);
const PEACH: (u8, u8, u8) = (250, 179, 135);

struct PickerView<'a> {
    sessions: &'a [SessionInfo],
    selected: usize,
    create_input: Option<&'a str>,
    search_input: Option<&'a str>,
    delete_armed: Option<&'a SessionName>,
}

fn render(
    sessions: &[SessionInfo],
    selected: usize,
    size: (u16, u16),
    create_input: Option<&str>,
    search_input: Option<&str>,
    delete_armed: Option<&SessionName>,
) -> Vec<u8> {
    let (box_row, box_column, height, width) = picker_rect(size);
    // Clear with the terminal's default background so emulator transparency is
    // preserved outside the explicitly painted manager window.
    let mut frame = String::from("\x1b[0m\x1b[2J\x1b[H\x1b[?25l");
    for row in 0..height {
        write_field(
            &mut frame,
            box_row + row,
            box_column,
            "",
            width,
            TEXT,
            BASE,
            false,
        );
    }
    if width < 4 || height < 3 {
        return frame.into_bytes();
    }

    let inner_width = width - 2;
    write_at(&mut frame, box_row, box_column, "┌", BLUE, BASE, true);
    write_at(
        &mut frame,
        box_row,
        box_column + 1,
        &"─".repeat(inner_width),
        BLUE,
        BASE,
        true,
    );
    write_at(
        &mut frame,
        box_row,
        box_column + width - 1,
        "┐",
        BLUE,
        BASE,
        true,
    );
    for row in 1..height - 1 {
        write_at(
            &mut frame,
            box_row + row,
            box_column,
            "│",
            BLUE,
            BASE,
            false,
        );
        write_at(
            &mut frame,
            box_row + row,
            box_column + width - 1,
            "│",
            BLUE,
            BASE,
            false,
        );
    }
    write_at(
        &mut frame,
        box_row + height - 1,
        box_column,
        &format!("└{}┘", "─".repeat(inner_width)),
        BLUE,
        BASE,
        false,
    );

    let title = truncate("─ Session Manager ", inner_width);
    write_at(
        &mut frame,
        box_row,
        box_column + 1,
        &title,
        BLUE,
        BASE,
        true,
    );
    let count = format!(
        " {} SESSION{} ",
        sessions.len(),
        if sessions.len() == 1 { "" } else { "S" }
    );
    let count_width = UnicodeWidthStr::width(count.as_str());
    if count_width <= inner_width.saturating_sub(UnicodeWidthStr::width(title.as_str())) {
        write_at(
            &mut frame,
            box_row,
            box_column + width - 1 - count_width,
            &count,
            MUTED,
            BASE,
            true,
        );
    }

    if height < 8 {
        let view = PickerView {
            sessions,
            selected,
            create_input,
            search_input,
            delete_armed,
        };
        draw_compact_sessions(&mut frame, &view, (box_row, box_column, height, width));
        return frame.into_bytes();
    }

    let body_width = width - 4;
    let navigation = if let Some(name) = create_input {
        format!("New session: {name}_")
    } else if let Some(query) = search_input {
        format!("Search: {query}_")
    } else {
        "↑/↓/j/k Move  / Search  Esc/q Close".to_owned()
    };
    write_field(
        &mut frame,
        box_row + 1,
        box_column + 2,
        &navigation,
        body_width,
        if create_input.is_some() || search_input.is_some() {
            BLUE
        } else {
            MUTED
        },
        BASE,
        create_input.is_some() || search_input.is_some(),
    );
    write_at(
        &mut frame,
        box_row + 2,
        box_column + 1,
        &"─".repeat(inner_width),
        SURFACE,
        BASE,
        false,
    );

    let marker_width = 2;
    let status_width = if body_width >= 26 { 12 } else { 0 };
    let pid_width = if body_width >= 32 { 9 } else { 0 };
    let name_width = body_width.saturating_sub(marker_width + status_width + pid_width);
    let header_row = box_row + 3;
    let mut column = box_column + 2;
    write_field(
        &mut frame,
        header_row,
        column,
        "",
        marker_width,
        MUTED,
        BASE,
        false,
    );
    column += marker_width;
    write_field(
        &mut frame, header_row, column, "SESSION", name_width, MUTED, BASE, true,
    );
    column += name_width;
    if status_width > 0 {
        write_field(
            &mut frame,
            header_row,
            column,
            "STATUS",
            status_width,
            MUTED,
            BASE,
            true,
        );
        column += status_width;
    }
    if pid_width > 0 {
        write_field(
            &mut frame, header_row, column, "PID", pid_width, MUTED, BASE, true,
        );
    }

    let visible = (height - 6).min(sessions.len());
    let start = selected
        .saturating_sub(visible.saturating_sub(1))
        .min(sessions.len().saturating_sub(visible));
    for (offset, session) in sessions[start..start + visible].iter().enumerate() {
        let index = start + offset;
        let selected = index == selected;
        let background = if selected { SURFACE } else { BASE };
        let row = box_row + 4 + offset;
        let mut column = box_column + 2;
        write_field(
            &mut frame,
            row,
            column,
            if selected { "› " } else { "  " },
            marker_width,
            if selected { BLUE } else { MUTED },
            background,
            selected,
        );
        column += marker_width;
        write_field(
            &mut frame,
            row,
            column,
            session.name.as_str(),
            name_width,
            TEAL,
            background,
            true,
        );
        column += name_width;
        if status_width > 0 {
            write_field(
                &mut frame,
                row,
                column,
                if session.attached {
                    "[ATTACHED]"
                } else {
                    "[DETACHED]"
                },
                status_width,
                if session.attached { PEACH } else { MUTED },
                background,
                true,
            );
            column += status_width;
        }
        if pid_width > 0 {
            write_field(
                &mut frame,
                row,
                column,
                &session
                    .server_pid
                    .map_or_else(|| "—".to_owned(), |pid| pid.to_string()),
                pid_width,
                MUTED,
                background,
                false,
            );
        }
    }

    let footer = if create_input.is_some() {
        "<Enter> Create  <Esc> Cancel".to_owned()
    } else if search_input.is_some() {
        "<Enter> Attach  <Tab> Complete  <Esc> Clear".to_owned()
    } else if let Some(name) = delete_armed {
        format!("Press d again to kill '{name}'")
    } else {
        "<Enter> Attach  <a> New  <dd> Kill".to_owned()
    };
    write_field(
        &mut frame,
        box_row + height - 2,
        box_column + 2,
        &footer,
        body_width,
        if delete_armed.is_some() { PEACH } else { TEXT },
        BASE,
        delete_armed.is_some(),
    );
    frame.into_bytes()
}

fn draw_compact_sessions(
    frame: &mut String,
    view: &PickerView<'_>,
    (box_row, box_column, height, width): (usize, usize, usize, usize),
) {
    let visible = height.saturating_sub(3).min(view.sessions.len());
    let start = view
        .selected
        .saturating_sub(visible.saturating_sub(1))
        .min(view.sessions.len().saturating_sub(visible));
    for (offset, session) in view.sessions[start..start + visible].iter().enumerate() {
        let index = start + offset;
        let selected = index == view.selected;
        write_field(
            frame,
            box_row + 1 + offset,
            box_column + 1,
            &format!(
                "{} {}  {}",
                if selected { "›" } else { " " },
                session.name,
                if session.attached {
                    "[ATTACHED]"
                } else {
                    "[DETACHED]"
                }
            ),
            width.saturating_sub(2),
            if selected { BLUE } else { TEXT },
            if selected { SURFACE } else { BASE },
            selected,
        );
    }
    if height >= 3 {
        let footer = if let Some(name) = view.create_input {
            format!("New: {name}_  Enter create")
        } else if let Some(query) = view.search_input {
            format!("Search: {query}_  Enter attach")
        } else if let Some(name) = view.delete_armed {
            format!("d again: kill {name}")
        } else {
            "Enter attach  a new  dd kill".to_owned()
        };
        write_field(
            frame,
            box_row + height - 2,
            box_column + 1,
            &footer,
            width.saturating_sub(2),
            if view.delete_armed.is_some() {
                PEACH
            } else {
                MUTED
            },
            BASE,
            view.delete_armed.is_some(),
        );
    }
}

fn picker_rect((rows, columns): (u16, u16)) -> (usize, usize, usize, usize) {
    let rows = usize::from(rows.max(1));
    let columns = usize::from(columns.max(1));
    let width = (columns / 2).max(40).min(columns);
    let height = (rows / 2).max(8).min(rows);
    (
        rows.saturating_sub(height) / 2 + 1,
        columns.saturating_sub(width) / 2 + 1,
        height,
        width,
    )
}

fn write_at(
    frame: &mut String,
    row: usize,
    column: usize,
    text: &str,
    foreground: (u8, u8, u8),
    background: (u8, u8, u8),
    bold: bool,
) {
    let _ = write!(frame, "\x1b[{row};{column}H");
    set_style(frame, foreground, background, bold);
    frame.push_str(text);
}

#[allow(clippy::too_many_arguments)]
fn write_field(
    frame: &mut String,
    row: usize,
    column: usize,
    text: &str,
    width: usize,
    foreground: (u8, u8, u8),
    background: (u8, u8, u8),
    bold: bool,
) {
    let text = truncate(text, width);
    let used = UnicodeWidthStr::width(text.as_str());
    write_at(frame, row, column, &text, foreground, background, bold);
    frame.push_str(&" ".repeat(width.saturating_sub(used)));
}

fn truncate(text: &str, width: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|character| {
            let next = used + UnicodeWidthChar::width(*character).unwrap_or(0);
            if next > width {
                false
            } else {
                used = next;
                true
            }
        })
        .collect()
}

fn set_style(
    frame: &mut String,
    (red, green, blue): (u8, u8, u8),
    (background_red, background_green, background_blue): (u8, u8, u8),
    bold: bool,
) {
    let bold = if bold { 1 } else { 22 };
    let _ = write!(
        frame,
        "\x1b[{bold};38;2;{red};{green};{blue};48;2;{background_red};{background_green};{background_blue}m"
    );
}

struct PickerSignals {
    pending: Arc<AtomicUsize>,
    ids: Vec<signal_hook::SigId>,
}

impl PickerSignals {
    fn install() -> io::Result<Self> {
        let mut signals = Self {
            pending: Arc::new(AtomicUsize::new(0)),
            ids: Vec::new(),
        };
        for signal in [SIGHUP, SIGTERM, SIGINT, SIGQUIT] {
            signals.ids.push(signal_hook::flag::register_usize(
                signal,
                signals.pending.clone(),
                signal as usize,
            )?);
        }
        Ok(signals)
    }

    fn pending(&self) -> Option<usize> {
        match self.pending.load(Ordering::Relaxed) {
            0 => None,
            signal => Some(signal),
        }
    }
}

impl Drop for PickerSignals {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_handles_navigation_choice_and_delayed_escape() {
        let now = Instant::now();
        let mut decoder = InputDecoder::default();
        assert_eq!(
            decoder.feed(b"\x1b[A\x1bOBj\t\r", now),
            [
                Key::Up,
                Key::Down,
                Key::Character(b'j'),
                Key::Tab,
                Key::Enter,
            ]
        );
        assert!(decoder.feed(b"\x1b", now).is_empty());
        assert_eq!(decoder.flush_due(now + ESCAPE_DELAY), Some(Key::Escape));
    }

    #[test]
    fn character_and_arrow_navigation_wrap_at_both_ends() {
        assert_eq!(navigation_target(0, 3, Key::Character(b'j')), Some(1));
        assert_eq!(navigation_target(2, 3, Key::Down), Some(0));
        assert_eq!(navigation_target(2, 3, Key::Character(b'k')), Some(1));
        assert_eq!(navigation_target(0, 3, Key::Up), Some(2));
        assert_eq!(navigation_target(0, 0, Key::Character(b'j')), None);
        assert_eq!(navigation_target(0, 3, Key::Character(b'x')), None);
        assert_eq!(search_navigation_target(0, 3, Key::Down), Some(1));
        assert_eq!(search_navigation_target(0, 3, Key::Up), Some(2));
        assert_eq!(search_navigation_target(0, 3, Key::Character(b'j')), None);
        assert_eq!(search_navigation_target(0, 3, Key::Character(b'k')), None);
    }

    #[test]
    fn rendering_keeps_selected_session_in_a_small_viewport() {
        let sessions: Vec<_> = ["one", "two", "three", "four"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| SessionInfo {
                name: SessionName::new(name).unwrap(),
                attached: index == 1,
                server_pid: Some(100 + index as i32),
            })
            .collect();
        let frame = String::from_utf8(render(&sessions, 3, (5, 20), None, None, None)).unwrap();
        assert!(frame.contains("› four  [DETACHED]"));
        assert!(!frame.contains("one"));
        assert!(frame.contains("Session Manager"));
    }

    #[test]
    fn full_picker_is_centered_and_shows_status_and_server_pid() {
        let sessions = [
            SessionInfo {
                name: SessionName::new("active").unwrap(),
                attached: true,
                server_pid: Some(4321),
            },
            SessionInfo {
                name: SessionName::new("idle").unwrap(),
                attached: false,
                server_pid: Some(9876),
            },
        ];
        let frame = String::from_utf8(render(&sessions, 1, (24, 80), None, None, None)).unwrap();
        assert!(frame.starts_with("\x1b[0m\x1b[2J"));
        assert!(!frame.contains("48;2;24;24;37"));
        assert!(frame.contains("\x1b[7;21H"));
        assert!(frame.contains("2 SESSIONS"));
        assert!(frame.contains("[ATTACHED]"));
        assert!(frame.contains("[DETACHED]"));
        assert!(frame.contains("4321"));
        assert!(frame.contains("9876"));
        assert!(frame.contains("<Enter> Attach  <a> New  <dd> Kill"));
        let create =
            String::from_utf8(render(&sessions, 1, (24, 80), Some("work"), None, None)).unwrap();
        assert!(create.contains("New session: work_"));
        assert!(create.contains("<Enter> Create  <Esc> Cancel"));
        let delete = String::from_utf8(render(
            &sessions,
            1,
            (24, 80),
            None,
            None,
            Some(&sessions[1].name),
        ))
        .unwrap();
        assert!(delete.contains("Press d again to kill 'idle'"));
    }

    #[test]
    fn search_filters_case_insensitively_and_renders_its_editor() {
        let sessions: Vec<_> = ["alpha", "Jupiter", "jump-start"]
            .into_iter()
            .map(|name| SessionInfo {
                name: SessionName::new(name).unwrap(),
                attached: false,
                server_pid: None,
            })
            .collect();
        let filtered = filtered_sessions(&sessions, Some("JU"));
        assert_eq!(
            filtered
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            ["Jupiter", "jump-start"]
        );

        let frame =
            String::from_utf8(render(&filtered, 0, (24, 80), None, Some("JU"), None)).unwrap();
        assert!(frame.contains("Search: JU_"));
        assert!(frame.contains("<Tab> Complete"));
        assert!(!frame.contains("alpha"));
    }
}

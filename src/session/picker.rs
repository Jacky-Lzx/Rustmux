//! Small terminal picker used by `rustmux attach` when no name is supplied.

use std::fmt::Write as _;
use std::io;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, poll};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{SessionInfo, SessionName};
use crate::terminal_device::TerminalDevice;

const POLL_MILLIS: u16 = 30;
const ESCAPE_DELAY: Duration = Duration::from_millis(30);

mod worker;
use crate::config::manager::{Action, Bindings, Key};
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
                    3 => keys.push(Key::Byte(3)),
                    8 | 127 => keys.push(Key::Byte(127)),
                    b'\t' => keys.push(Key::Byte(9)),
                    b'\r' | b'\n' => keys.push(Key::Byte(13)),
                    _ => keys.push(Key::byte(byte)),
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
                    keys.push(Key::Byte(27));
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
            Some(Key::Byte(27))
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
    config_path: Option<&std::path::Path>,
) -> io::Result<Choice> {
    let config = crate::config::load_with_path(config_path).map_err(io::Error::other)?;
    let mut reload =
        crate::config::reload::Reload::new(&config)?.expect("loaded configuration source");
    let worker = worker::Worker::new(initially_selected.cloned())?;
    let file = TerminalDevice::open_controlling()?;
    let signals = PickerSignals::install()?;
    let mut terminal = TerminalDevice::enter(file)?;
    let result = run_picker(
        &mut terminal,
        &signals,
        sessions,
        initially_selected,
        &mut reload,
        &worker,
    );
    worker.shutdown();
    reload.shutdown();
    let restored = terminal.restore();
    result.and_then(|selection| restored.map(|()| selection))
}

fn run_picker(
    terminal: &mut TerminalDevice,
    signals: &PickerSignals,
    sessions: &[SessionInfo],
    initially_selected: Option<&SessionName>,
    reload: &mut crate::config::reload::Reload,
    worker: &worker::Worker,
) -> io::Result<Choice> {
    let mut sessions = sessions.to_vec();
    let mut bindings = reload.current().manager().clone();
    let mut status = None;
    let mut list_error = None;
    let mut reload_error = None;
    let mut saving = false;
    let mut deleting = false;
    let mut selected = initially_selected
        .and_then(|name| sessions.iter().position(|session| &session.name == name))
        .unwrap_or(0);
    let mut create_input: Option<String> = None;
    let mut search_input: Option<String> = None;
    let mut delete_armed: Option<(SessionName, bool)> = None;
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

        let updates = worker.poll();
        let operation_completed = updates.save.is_some() || updates.delete.is_some();
        if let Some(list) = updates.list {
            match list {
                Ok(next) => {
                    if list_error.take().is_some() {
                        dirty = true;
                    }
                    if next != sessions {
                        let previous = filtered_sessions(&sessions, search_input.as_deref());
                        let name = previous.get(selected).map(|s| &s.name);
                        let visible = filtered_sessions(&next, search_input.as_deref());
                        selected = name
                            .and_then(|name| visible.iter().position(|s| &s.name == name))
                            .unwrap_or(selected)
                            .min(visible.len().saturating_sub(1));
                        sessions = next;
                        delete_armed = None;
                        dirty = true;
                    }
                }
                Err(error) => {
                    list_error = Some(format!("Refresh failed: {error}"));
                    dirty = true;
                }
            }
        }
        if let Some((name, result)) = updates.save {
            saving = false;
            status = Some(match result {
                Ok(()) => format!("Saved {name}"),
                Err(error) => format!("Save failed: {error}"),
            });
            dirty = true;
        }
        if let Some((name, result)) = updates.delete {
            deleting = false;
            status = Some(match result {
                Ok(()) => format!("Deleted {name}"),
                Err(error) => format!("Delete failed: {error}"),
            });
            dirty = true;
        }
        reload.poll_manager();
        let error = reload
            .error()
            .map(|error| format!("Config reload failed: {error}"));
        if error != reload_error {
            reload_error = error;
            if !saving && !deleting && !operation_completed {
                status = None;
            }
            dirty = true;
        }
        if decoder.escape.is_empty()
            && let Some(config) = reload.take()
        {
            bindings = config.manager().clone();
            reload.commit(config);
            delete_armed = None;
            dirty = true;
        }
        let size = terminal.size()?;
        let size = (size.ws_row, size.ws_col);
        if dirty || previous_size != Some(size) {
            let visible = filtered_sessions(&sessions, search_input.as_deref());
            selected = selected.min(visible.len().saturating_sub(1));
            terminal.write_all(&render_view(
                &PickerView {
                    sessions: &visible,
                    selected,
                    create_input: create_input.as_deref(),
                    search_input: search_input.as_deref(),
                    delete_armed: delete_armed.as_ref().map(|(name, _)| name),
                    current: initially_selected,
                    bindings: &bindings,
                    status: status
                        .as_deref()
                        .or(reload_error.as_deref())
                        .or(list_error.as_deref()),
                },
                size,
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
            if key == Key::Byte(3) {
                return Ok(Choice::Cancel);
            }
            let editing = create_input.is_some() || search_input.is_some();
            let action = bindings.action(key, editing);
            let armed = delete_armed.take();
            if action != Some(Action::Save) && !saving && !deleting {
                status = None;
            }
            if action == Some(Action::Save) {
                if !saving && !deleting {
                    let target = save_target(
                        &sessions,
                        initially_selected,
                        selected,
                        search_input.as_deref(),
                    );
                    status = Some(if let Some(name) = target {
                        if worker.save(name.clone()) {
                            saving = true;
                            format!("Saving {name}…")
                        } else {
                            "Save failed: worker unavailable".into()
                        }
                    } else {
                        "Save failed: no running session to save".into()
                    });
                }
                dirty = true;
                continue;
            }
            if let Some(name) = &mut create_input {
                match action {
                    Some(Action::Open) => {
                        if let Ok(name) = SessionName::new(name.clone()) {
                            return Ok(Choice::Create(name));
                        }
                    }
                    Some(Action::Cancel) => create_input = None,
                    Some(Action::Backspace) => {
                        name.pop();
                    }
                    _ => {
                        if let Key::Byte(byte) = key
                            && (byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                            && name.len() < super::MAX_SESSION_NAME_BYTES
                        {
                            name.push(char::from(byte));
                        }
                    }
                }
                dirty = true;
                continue;
            }
            let visible = filtered_sessions(&sessions, search_input.as_deref());
            selected = selected.min(visible.len().saturating_sub(1));
            if let Some(next) = action_navigation(selected, visible.len(), action) {
                selected = next;
                dirty = true;
                continue;
            }
            if let Some(query) = &mut search_input {
                match action {
                    Some(Action::Open) if !visible.is_empty() => {
                        return Ok(Choice::Attach(visible[selected].name.clone()));
                    }
                    Some(Action::Complete) if !visible.is_empty() => {
                        *query = visible[selected].name.as_str().into();
                        selected = 0;
                    }
                    Some(Action::Cancel) => {
                        let name = visible.get(selected).map(|s| &s.name);
                        selected = name
                            .and_then(|n| sessions.iter().position(|s| &s.name == n))
                            .unwrap_or(0);
                        search_input = None;
                    }
                    Some(Action::Backspace) => {
                        query.pop();
                        selected = 0;
                    }
                    _ => {
                        if let Key::Byte(byte) = key
                            && byte.is_ascii_graphic()
                            && query.len() < super::MAX_SESSION_NAME_BYTES
                        {
                            query.push(char::from(byte));
                            selected = 0;
                        }
                    }
                }
            } else {
                match action {
                    Some(Action::Open) if !visible.is_empty() => {
                        return Ok(Choice::Attach(visible[selected].name.clone()));
                    }
                    Some(Action::Create) => create_input = Some(String::new()),
                    Some(Action::Search) => search_input = Some(String::new()),
                    Some(Action::Delete) if !visible.is_empty() && !saving && !deleting => {
                        let name = visible[selected].name.clone();
                        if armed.as_ref() == Some(&(name.clone(), visible[selected].saved)) {
                            if !visible[selected].saved {
                                return Ok(Choice::Kill(name));
                            }
                            status = Some(if worker.delete(name.clone()) {
                                deleting = true;
                                format!("Deleting {name}…")
                            } else {
                                "Delete failed: worker unavailable".into()
                            });
                        } else {
                            delete_armed = Some((name, visible[selected].saved));
                        }
                    }
                    Some(Action::Cancel) => return Ok(Choice::Cancel),
                    _ => {}
                }
            }
            dirty = true;
        }
    }
}

fn save_target(
    sessions: &[SessionInfo],
    current: Option<&SessionName>,
    selected: usize,
    query: Option<&str>,
) -> Option<SessionName> {
    if let Some(name) = current {
        return sessions
            .iter()
            .find(|s| &s.name == name && !s.saved)
            .map(|s| s.name.clone());
    }
    filtered_sessions(sessions, query)
        .get(selected)
        .filter(|s| !s.saved)
        .map(|s| s.name.clone())
}
fn action_navigation(selected: usize, count: usize, action: Option<Action>) -> Option<usize> {
    if count == 0 {
        return None;
    }
    match action {
        Some(Action::Up) => Some(selected.checked_sub(1).unwrap_or(count - 1)),
        Some(Action::Down) => Some((selected + 1) % count),
        _ => None,
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

#[cfg(test)]
fn navigation_target(selected: usize, count: usize, key: Key) -> Option<usize> {
    action_navigation(selected, count, Bindings::default().action(key, false))
}
#[cfg(test)]
fn search_navigation_target(selected: usize, count: usize, key: Key) -> Option<usize> {
    action_navigation(selected, count, Bindings::default().action(key, true))
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
    current: Option<&'a SessionName>,
    bindings: &'a Bindings,
    status: Option<&'a str>,
}

#[cfg(test)]
fn render(
    sessions: &[SessionInfo],
    selected: usize,
    size: (u16, u16),
    create_input: Option<&str>,
    search_input: Option<&str>,
    delete_armed: Option<&SessionName>,
    current: Option<&SessionName>,
) -> Vec<u8> {
    render_view(
        &PickerView {
            sessions,
            selected,
            create_input,
            search_input,
            delete_armed,
            current,
            bindings: &Bindings::default(),
            status: None,
        },
        size,
    )
}
fn render_view(view: &PickerView<'_>, size: (u16, u16)) -> Vec<u8> {
    let PickerView {
        sessions,
        selected,
        create_input,
        search_input,
        delete_armed,
        current,
        bindings,
        status: _,
    } = *view;
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
        draw_compact_sessions(&mut frame, view, (box_row, box_column, height, width));
        return frame.into_bytes();
    }

    let body_width = width - 4;
    let navigation = if let Some(name) = create_input {
        format!("New session: {name}_")
    } else if let Some(query) = search_input {
        format!("Search: {query}_")
    } else {
        [
            bindings.hint(Action::Save, "Save"),
            bindings.hint(Action::Up, "Up"),
            bindings.hint(Action::Down, "Down"),
            bindings.hint(Action::Search, "Search"),
            bindings.hint(Action::Cancel, "Close"),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("  ")
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
    let last_connected_width = if body_width >= 54 { 15 } else { 0 };
    let name_width =
        body_width.saturating_sub(marker_width + status_width + last_connected_width + pid_width);
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
    if last_connected_width > 0 {
        write_field(
            &mut frame,
            header_row,
            column,
            "LAST CONNECTED",
            last_connected_width,
            MUTED,
            BASE,
            true,
        );
        column += last_connected_width;
    }
    if pid_width > 0 {
        write_field(
            &mut frame, header_row, column, "PID", pid_width, MUTED, BASE, true,
        );
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
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
                &format!("[{}]", session.status(current)),
                status_width,
                if current == Some(&session.name) || session.attached {
                    PEACH
                } else {
                    MUTED
                },
                background,
                true,
            );
            column += status_width;
        }
        if last_connected_width > 0 {
            write_field(
                &mut frame,
                row,
                column,
                &session.last_connected_label(current, now),
                last_connected_width,
                MUTED,
                background,
                false,
            );
            column += last_connected_width;
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

    let footer = picker_footer(view, false);
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
                "{} {}  [{}]",
                if selected { "›" } else { " " },
                session.name,
                session.status(view.current)
            ),
            width.saturating_sub(2),
            if selected { BLUE } else { TEXT },
            if selected { SURFACE } else { BASE },
            selected,
        );
    }
    if height >= 3 {
        let footer = picker_footer(view, true);
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

fn picker_footer(view: &PickerView<'_>, compact: bool) -> String {
    if let Some(status) = view.status {
        return status
            .chars()
            .take(256)
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
    }
    let b = view.bindings;
    let saved = view.sessions.get(view.selected).is_some_and(|s| s.saved);
    if let Some(name) = view.delete_armed {
        return format!(
            "Press {} again to {} '{}'",
            delete_label(b),
            if saved { "delete saved" } else { "kill" },
            name
        );
    }
    let open = if view.create_input.is_some() {
        "Create"
    } else if saved {
        "Restore"
    } else {
        "Attach"
    };
    let editing = view.create_input.is_some() || view.search_input.is_some();
    let mut hints = vec![b.hint_in(Action::Open, open, editing)];
    if view.create_input.is_some() {
        hints.push(b.hint_in(Action::Cancel, "Cancel", true));
    } else if view.search_input.is_some() {
        hints.extend([
            b.hint_in(Action::Complete, "Complete", true),
            b.hint_in(Action::Cancel, "Clear", true),
        ]);
    } else {
        hints.push(b.hint(Action::Create, "New"));
        let label = repeat_delete_label(b);
        if !label.is_empty() {
            hints.push(format!(
                "<{label}> {}",
                if saved { "Delete" } else { "Kill" }
            ));
        }
    }
    if !saved || view.current.is_some() {
        hints.push(b.hint_in(Action::Save, "Save", editing));
    }
    if compact && let Some(name) = view.create_input {
        hints.insert(0, format!("New: {name}_"));
    }
    if compact && let Some(query) = view.search_input {
        hints.insert(0, format!("Search: {query}_"));
    }
    hints
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("  ")
}

fn delete_label(bindings: &Bindings) -> String {
    bindings
        .hint(Action::Delete, "")
        .trim()
        .trim_matches(['<', '>'])
        .to_owned()
}
fn repeat_delete_label(bindings: &Bindings) -> String {
    let key = delete_label(bindings);
    if key.len() == 1 {
        format!("{key}{key}")
    } else if key.is_empty() {
        key
    } else {
        format!("{key} twice")
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
    fn save_target_uses_current_or_selected_live_session_without_fallback() {
        let current = SessionName::new("current").unwrap();
        let other = SessionName::new("other").unwrap();
        let mut sessions = vec![
            SessionInfo {
                name: current.clone(),
                saved: false,
                attached: false,
                server_pid: Some(10),
                last_connected_at: None,
            },
            SessionInfo {
                name: other.clone(),
                saved: false,
                attached: false,
                server_pid: Some(11),
                last_connected_at: None,
            },
        ];
        assert_eq!(
            save_target(&sessions, Some(&current), 1, None),
            Some(current.clone())
        );
        assert_eq!(save_target(&sessions, None, 1, None), Some(other));
        sessions[0].saved = true;
        assert_eq!(save_target(&sessions, Some(&current), 1, None), None);
        assert_eq!(save_target(&sessions, None, 0, None), None);
        assert_eq!(save_target(&sessions, None, 0, Some("missing")), None);
    }

    #[test]
    fn decoder_handles_navigation_choice_and_delayed_escape() {
        let now = Instant::now();
        let mut decoder = InputDecoder::default();
        assert_eq!(
            decoder.feed(b"\x1b[A\x1bOBj\t\r", now),
            [
                Key::Up,
                Key::Down,
                Key::Byte(b'j'),
                Key::Byte(9),
                Key::Byte(13),
            ]
        );
        assert!(decoder.feed(b"\x1b", now).is_empty());
        assert_eq!(decoder.flush_due(now + ESCAPE_DELAY), Some(Key::Byte(27)));
    }

    #[test]
    fn character_and_arrow_navigation_wrap_at_both_ends() {
        assert_eq!(navigation_target(0, 3, Key::Byte(b'j')), Some(1));
        assert_eq!(navigation_target(2, 3, Key::Down), Some(0));
        assert_eq!(navigation_target(2, 3, Key::Byte(b'k')), Some(1));
        assert_eq!(navigation_target(0, 3, Key::Up), Some(2));
        assert_eq!(navigation_target(0, 0, Key::Byte(b'j')), None);
        assert_eq!(navigation_target(0, 3, Key::Byte(b'x')), None);
        assert_eq!(search_navigation_target(0, 3, Key::Down), Some(1));
        assert_eq!(search_navigation_target(0, 3, Key::Up), Some(2));
        assert_eq!(search_navigation_target(0, 3, Key::Byte(b'j')), None);
        assert_eq!(search_navigation_target(0, 3, Key::Byte(b'k')), None);
    }

    #[test]
    fn rendering_keeps_selected_session_in_a_small_viewport() {
        let sessions: Vec<_> = ["one", "two", "three", "four"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| SessionInfo {
                saved: false,
                name: SessionName::new(name).unwrap(),
                attached: index == 1,
                server_pid: Some(100 + index as i32),
                last_connected_at: None,
            })
            .collect();
        let frame =
            String::from_utf8(render(&sessions, 3, (5, 20), None, None, None, None)).unwrap();
        assert!(frame.contains("› four  [DETACHED]"));
        assert!(!frame.contains("one"));
        assert!(frame.contains("Session Manager"));
    }

    #[test]
    fn full_picker_is_centered_and_shows_status_and_server_pid() {
        let sessions = [
            SessionInfo {
                saved: false,
                name: SessionName::new("active").unwrap(),
                attached: true,
                server_pid: Some(4321),
                last_connected_at: Some(900),
            },
            SessionInfo {
                saved: false,
                name: SessionName::new("idle").unwrap(),
                attached: false,
                server_pid: Some(9876),
                last_connected_at: Some(500),
            },
        ];
        let frame =
            String::from_utf8(render(&sessions, 1, (24, 80), None, None, None, None)).unwrap();
        assert!(frame.starts_with("\x1b[0m\x1b[2J"));
        assert!(!frame.contains("48;2;24;24;37"));
        assert!(frame.contains("\x1b[7;21H"));
        assert!(frame.contains("2 SESSIONS"));
        assert!(frame.contains("[ATTACHED]"));
        assert!(frame.contains("[DETACHED]"));
        assert!(frame.contains("4321"));
        assert!(frame.contains("9876"));
        assert!(frame.contains("<Enter> Attach  <a> New"));
        let wide =
            String::from_utf8(render(&sessions, 1, (24, 160), None, None, None, None)).unwrap();
        assert!(wide.contains("LAST CONNECTED"));
        assert!(wide.contains("Now"));
        let create = String::from_utf8(render(
            &sessions,
            1,
            (24, 80),
            Some("work"),
            None,
            None,
            None,
        ))
        .unwrap();
        assert!(create.contains("New session: work_"));
        assert!(create.contains("<Enter> Create  <Esc> Cancel"));
        let delete = String::from_utf8(render(
            &sessions,
            1,
            (24, 80),
            None,
            None,
            Some(&sessions[1].name),
            None,
        ))
        .unwrap();
        assert!(delete.contains("Press d again to kill 'idle'"));
    }

    #[test]
    fn search_filters_case_insensitively_and_renders_its_editor() {
        let sessions: Vec<_> = ["alpha", "Jupiter", "jump-start"]
            .into_iter()
            .map(|name| SessionInfo {
                saved: false,
                name: SessionName::new(name).unwrap(),
                attached: false,
                server_pid: None,
                last_connected_at: None,
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

        let frame = String::from_utf8(render(&filtered, 0, (24, 80), None, Some("JU"), None, None))
            .unwrap();
        assert!(frame.contains("Search: JU_"));
        assert!(frame.contains("<Tab> Complete"));
        assert!(!frame.contains("alpha"));
    }

    #[test]
    fn saved_session_shows_restore_action_in_full_compact_and_search_views() {
        let sessions = [SessionInfo {
            name: SessionName::new("offline").unwrap(),
            saved: true,
            attached: false,
            server_pid: None,
            last_connected_at: None,
        }];
        for (size, query, action) in [
            ((24, 160), None, "<Enter> Restore"),
            ((6, 40), None, "<Enter> Restore"),
            ((24, 100), Some("off"), "<Enter> Restore"),
        ] {
            let frame =
                String::from_utf8(render(&sessions, 0, size, None, query, None, None)).unwrap();
            assert!(frame.contains("[SAVED]"));
            assert!(frame.contains(action));
            if query.is_none() && size.1 >= 160 {
                assert!(frame.contains("<dd> Delete"));
            }
            assert!(!frame.contains("[DETACHED]"));
        }
    }

    #[test]
    fn current_session_has_a_distinct_status() {
        let sessions = [SessionInfo {
            saved: false,
            name: SessionName::new("work").unwrap(),
            attached: false,
            server_pid: Some(4321),
            last_connected_at: Some(900),
        }];
        let frame = String::from_utf8(render(
            &sessions,
            0,
            (24, 80),
            None,
            None,
            None,
            Some(&sessions[0].name),
        ))
        .unwrap();
        assert!(frame.contains("[CURRENT]"));
        assert!(!frame.contains("[DETACHED]"));
    }

    #[test]
    fn last_connected_labels_use_compact_elapsed_units() {
        let mut session = SessionInfo {
            saved: false,
            name: SessionName::new("work").unwrap(),
            attached: false,
            server_pid: None,
            last_connected_at: None,
        };
        assert_eq!(session.last_connected_label(None, 100_000_000), "—");
        session.last_connected_at = Some(99_955_000);
        assert_eq!(session.last_connected_label(None, 100_000_000), "45s ago");
        session.last_connected_at = Some(96_400_000);
        assert_eq!(session.last_connected_label(None, 100_000_000), "1h ago");
        session.attached = true;
        assert_eq!(session.last_connected_label(None, 100_000_000), "Now");
    }
}

//! Execute validated requests on the session event loop, never from a worker.
use super::*;
use crate::control::{MAX_INPUT, MAX_RESPONSE, Request};
use serde::Serialize;
use std::io;

#[derive(Serialize)]
struct PaneInfo {
    id: u64,
    pid: u32,
    window: usize,
    window_name: String,
    active: bool,
    selected: bool,
    directory: Option<String>,
    title: String,
    exited: bool,
    output_complete: bool,
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
}
#[derive(Serialize)]
struct PaneList {
    panes: Vec<PaneInfo>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn target(windows: &Windows<PaneSet<Pane>>, id: Option<u64>) -> io::Result<(WindowId, PaneId)> {
    if let Some(id) = id {
        for window in windows.iter() {
            if let Some((pane, _)) = window
                .content()
                .iter()
                .find(|(_, pane)| pane.control_id() == id)
            {
                return Ok((window.id(), pane));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "unknown runtime pane ID",
        ))
    } else {
        let window = windows.active().ok_or_else(|| invalid("no active pane"))?;
        Ok((window.id(), window.content().layout().active()))
    }
}
fn window_target(windows: &Windows<PaneSet<Pane>>, number: Option<u16>) -> io::Result<WindowId> {
    match number {
        Some(number) => usize::from(number)
            .checked_sub(1)
            .and_then(|position| windows.iter().nth(position))
            .map(|window| window.id())
            .ok_or_else(|| invalid("unknown window number")),
        None => windows
            .active()
            .map(|window| window.id())
            .ok_or_else(|| invalid("no active window")),
    }
}

fn name(name: Option<&str>) -> io::Result<()> {
    if name.is_some_and(|s| s.is_empty() || s.len() > 128 || s.chars().any(char::is_control)) {
        return Err(invalid(
            "window name must contain 1–128 bytes without controls",
        ));
    }
    Ok(())
}

fn startup(command: Option<&str>, cwd: Option<&Path>) -> io::Result<()> {
    crate::project::validate_command(command)?;
    if cwd.is_some_and(|path| !path.is_absolute() || !path.is_dir()) {
        return Err(invalid(
            "startup cwd must be an absolute existing directory",
        ));
    }
    Ok(())
}

pub(super) fn handle(
    request: Request,
    windows: &mut Windows<PaneSet<Pane>>,
    context: SessionContext<'_>,
    rows: u16,
    remain_on_exit: bool,
    reload: Option<&crate::config::reload::Reload>,
) -> io::Result<String> {
    match request {
        Request::RenameSession { .. } => Err(invalid("live rename is unavailable for this server")),
        Request::DisconnectSession { .. } => {
            Err(invalid("client disconnect is unavailable for this server"))
        }
        Request::ShowConfig => reload
            .ok_or_else(|| invalid("configuration reload is unavailable"))
            .and_then(|reload| reload.status()),
        Request::SelectPane { pane } => {
            let (window, id) = target(windows, Some(pane))?;
            let set = windows.get_mut(window).unwrap().content_mut();
            set.select(id)?;
            windows.select(window)?;
            Ok(String::new())
        }
        Request::SelectWindow { window } => {
            let id = window_target(windows, Some(window))?;
            windows.select(id)?;
            Ok(String::new())
        }
        Request::RenameWindow {
            window,
            name: window_name,
        } => {
            name(Some(&window_name))?;
            let id = window_target(windows, window)?;
            windows.rename(id, window_name)?;
            Ok(String::new())
        }
        Request::ResizePane {
            pane,
            direction,
            cells,
        } => {
            if cells == 0 {
                return Err(invalid("resize cells must be positive"));
            }
            let (window, id) = target(windows, pane)?;
            let set = windows.get_mut(window).unwrap().content_mut();
            if set.layout().is_zoomed() {
                return Err(invalid("cannot resize a zoomed pane layout"));
            }
            if !set.resize_pane(id, direction.into(), cells)? {
                return Err(invalid("pane separator cannot move in that direction"));
            }
            // Both event loops synchronize windows after accepted mutations.
            // An I/O failure must end the runtime, since a
            // partially committed screen/PTY resize cannot safely continue.
            Ok(String::new())
        }
        Request::ClosePane { pane } => {
            let (window, id) = target(windows, pane)?;
            let set = windows.get_mut(window).unwrap().content_mut();
            if set.iter().len() > 1 {
                // Transfer ownership out of the layout before dropping the
                // process; scripted closure does not enter close-undo storage.
                drop(set.close(id)?);
            } else {
                if windows.iter().len() == 1 {
                    return Err(invalid(
                        "cannot close the final session pane; use kill SESSION",
                    ));
                }
                drop(windows.close(window)?);
            }
            // Event loops synchronize every surviving set and route the
            // resulting focus transition through their normal refresh path.
            Ok(String::new())
        }
        Request::ListPanes { toml: as_toml } => {
            use std::os::unix::process::ExitStatusExt;
            let active = windows.active().map(|window| window.id());
            let mut panes = Vec::new();
            for (index, window) in windows.iter().enumerate() {
                for (id, pane) in window.content().iter() {
                    panes.push(PaneInfo {
                        id: pane.control_id(),
                        pid: pane.shell().id(),
                        window: index + 1,
                        window_name: window.name().to_owned(),
                        active: active == Some(window.id())
                            && id == window.content().layout().active(),
                        selected: id == window.content().layout().active(),
                        directory: pane
                            .inherited_directory()
                            .map(|p| p.to_string_lossy().into_owned()),
                        title: pane.terminal_title().to_owned(),
                        exited: pane.io().status.is_some(),
                        output_complete: pane.io().eof,
                        exit_code: pane.io().status.and_then(|s| s.code()),
                        exit_signal: pane.io().status.and_then(|s| s.signal()),
                    });
                }
            }
            if as_toml {
                toml::to_string(&PaneList { panes }).map_err(io::Error::other)
            } else {
                Ok(panes
                    .into_iter()
                    .map(|pane| {
                        format!(
                            "{}\t{}\t{}\t{}\n",
                            pane.id,
                            pane.window,
                            pane.title
                                .chars()
                                .map(|c| if c.is_control() { ' ' } else { c })
                                .collect::<String>(),
                            pane.directory
                                .unwrap_or_default()
                                .chars()
                                .map(|c| if c.is_control() { ' ' } else { c })
                                .collect::<String>()
                        )
                    })
                    .collect())
            }
        }
        Request::ReadPaneOutput {
            pane,
            after,
            require_retained,
        } => {
            let (window, id) = target(windows, pane)?;
            let pane = windows.get(window).unwrap().content().get(id).unwrap();
            if require_retained && !pane.retain_after_exit(remain_on_exit) {
                return Err(invalid(
                    "output subscriptions require remain_on_exit = true",
                ));
            }
            let chunk = pane.read_output(after)?;
            toml::to_string(&chunk).map_err(io::Error::other)
        }
        Request::CapturePane { pane, history } => {
            let (window, id) = target(windows, pane)?;
            capture(
                windows
                    .get(window)
                    .unwrap()
                    .content()
                    .get(id)
                    .unwrap()
                    .screen(),
                history,
            )
        }
        Request::RespawnPane { pane, command, cwd } => {
            crate::project::validate_command(command.as_deref())?;
            if cwd
                .as_ref()
                .is_some_and(|path| !path.is_absolute() || !path.is_dir())
            {
                return Err(invalid(
                    "respawn cwd must be an absolute existing directory",
                ));
            }
            let (window, id) = target(windows, pane)?;
            let pane = windows
                .get_mut(window)
                .unwrap()
                .content_mut()
                .get_mut(id)
                .unwrap();
            pane.respawn(
                context.shell_path,
                command.as_deref(),
                cwd.as_deref(),
                context.notifications,
                context.scrollback_lines,
            )?;
            Ok(format!("{}\n", pane.control_id()))
        }
        Request::SendKeys { pane, bytes } => {
            if bytes.len() > MAX_INPUT {
                return Err(invalid("input exceeds 4096 bytes"));
            }
            let (window, id) = target(windows, pane)?;
            let pane = windows
                .get_mut(window)
                .unwrap()
                .content_mut()
                .get_mut(id)
                .unwrap();
            if pane.io().eof || pane.io().status.is_some() {
                return Err(invalid("pane has exited"));
            }
            if pane.io().to_shell.len() + bytes.len() > crate::pane::INPUT_LIMIT {
                return Err(invalid("pane input queue is full"));
            }
            if bytes.iter().any(|&byte| byte == b'\r' || byte == b'\n') {
                pane.command_submitted();
            }
            pane.parts_mut().3.to_shell.extend(bytes);
            Ok(String::new())
        }
        Request::NewWindow {
            name: window_name,
            command,
            cwd,
        } => {
            name(window_name.as_deref())?;
            startup(command.as_deref(), cwd.as_deref())?;
            if windows.iter().len() >= MAX_WINDOWS {
                return Err(invalid("window limit reached"));
            }
            let columns = windows.active().unwrap().content().layout().dimensions().1;
            let rows = pane_rows(rows);
            let (content_rows, content_columns) = pane_content_dimensions(rows, columns);
            let directory = cwd.or_else(|| active_directory(windows));
            let set = PaneSet::new(
                rows,
                columns,
                Pane::spawn_with_startup(
                    context.shell_path,
                    directory.as_deref(),
                    content_rows,
                    content_columns,
                    context.notifications,
                    context.scrollback_lines,
                    command.as_deref(),
                )?,
            )?;
            let id = set.active().control_id();
            windows.create(window_name.unwrap_or_else(|| "shell".into()), set)?;
            Ok(format!("{id}\n"))
        }
        Request::SplitPane {
            pane,
            down,
            command,
            cwd,
        } => {
            startup(command.as_deref(), cwd.as_deref())?;
            let (window, id) = target(windows, pane)?;
            let set = windows.get_mut(window).unwrap().content_mut();
            let previous = set.layout().active();
            let directory = cwd.or_else(|| set.get(id).unwrap().inherited_directory());
            if set.get(id).unwrap().is_temporary() {
                return Err(invalid("temporary editor panes cannot be split by script"));
            }
            set.select(id)?;
            let result = set.split_with(
                if down {
                    SplitAxis::Rows
                } else {
                    SplitAxis::Columns
                },
                |_, rect| {
                    Pane::spawn_with_startup(
                        context.shell_path,
                        directory.as_deref(),
                        rect.rows,
                        rect.columns,
                        context.notifications,
                        context.scrollback_lines,
                        command.as_deref(),
                    )
                },
            );
            let new = match result {
                Ok(new) => new,
                Err(error) => {
                    set.select(previous)?;
                    return Err(error);
                }
            };
            let id = set.get(new).unwrap().control_id();
            set.synchronize_sizes()?;
            windows.select(window)?;
            Ok(format!("{id}\n"))
        }
        Request::JoinPane {
            pane,
            to_pane,
            down,
        } => {
            let (source, id) = target(windows, pane)?;
            let (destination, target_id) = target(windows, Some(to_pane))?;
            if source == destination {
                return Err(invalid("join-pane requires panes in different windows"));
            }
            if windows
                .get(source)
                .unwrap()
                .content()
                .get(id)
                .unwrap()
                .is_temporary()
                || windows
                    .get(destination)
                    .unwrap()
                    .content()
                    .get(target_id)
                    .unwrap()
                    .is_temporary()
            {
                return Err(invalid("temporary editor panes cannot be joined"));
            }
            let control_id = windows
                .get(source)
                .unwrap()
                .content()
                .get(id)
                .unwrap()
                .control_id();
            let source_focus = windows.get(source).unwrap().content().layout().active();
            let destination_focus = windows
                .get(destination)
                .unwrap()
                .content()
                .layout()
                .active();
            windows.get_mut(source).unwrap().content_mut().select(id)?;
            windows
                .get_mut(destination)
                .unwrap()
                .content_mut()
                .select(target_id)?;
            if let Err(error) = windows.join_pane_from(
                source,
                destination,
                if down {
                    SplitAxis::Rows
                } else {
                    SplitAxis::Columns
                },
            ) {
                windows
                    .get_mut(source)
                    .unwrap()
                    .content_mut()
                    .select(source_focus)?;
                windows
                    .get_mut(destination)
                    .unwrap()
                    .content_mut()
                    .select(destination_focus)?;
                return Err(error);
            }
            if let Some(window) = windows.get_mut(source) {
                window.content_mut().synchronize_sizes()?;
            }
            windows
                .get_mut(destination)
                .unwrap()
                .content_mut()
                .synchronize_sizes()?;
            Ok(format!("{control_id}\n"))
        }
        Request::BreakPane {
            pane,
            name: window_name,
        } => {
            name(window_name.as_deref())?;
            let (source, id) = target(windows, pane)?;
            let set = windows.get(source).unwrap().content();
            if set.get(id).unwrap().is_temporary() {
                return Err(invalid(
                    "temporary editor panes cannot be broken into windows",
                ));
            }
            let control_id = set.get(id).unwrap().control_id();
            if set.iter().len() == 1 {
                return Ok(format!("{control_id}\n"));
            }
            if windows.iter().len() >= MAX_WINDOWS {
                return Err(invalid("window limit reached"));
            }
            windows.get_mut(source).unwrap().content_mut().select(id)?;
            windows.select(source)?;
            if let Some(destination) = windows.break_active_pane()? {
                if let Some(name) = window_name {
                    windows.rename(destination, name)?;
                }
                windows
                    .get_mut(source)
                    .unwrap()
                    .content_mut()
                    .synchronize_sizes()?;
                windows
                    .get_mut(destination)
                    .unwrap()
                    .content_mut()
                    .synchronize_sizes()?;
            }
            Ok(format!("{control_id}\n"))
        }
    }
}

fn capture(screen: &crate::screen::Screen, history: bool) -> io::Result<String> {
    let mut text = String::new();
    // TOML quoting can expand a byte to six bytes. Reserve space for response metadata.
    let limit = (MAX_RESPONSE - 1024) / 6;
    let mut write_row = |row: &[crate::style::Cell], continued: bool| -> io::Result<()> {
        if continued && text.ends_with('\n') {
            text.pop();
        }
        for cell in row {
            if cell.width == 0 {
                continue;
            }
            if text.len()
                + cell.character.len_utf8()
                + cell.combining.iter().map(|c| c.len_utf8()).sum::<usize>()
                + 1
                > limit
            {
                return Err(invalid("capture exceeds bounded response size"));
            }
            text.push(cell.character);
            text.extend(&cell.combining);
        }
        if text.len() + 1 > limit {
            return Err(invalid("capture exceeds bounded response size"));
        }
        text.push('\n');
        Ok(())
    };
    if history && !screen.is_alternate() {
        for index in 0..screen.history_len() {
            let row = screen.history_row(index).unwrap();
            write_row(
                &row[..screen.history_row_used_columns(index).unwrap()],
                screen.history_row_continued(index).unwrap(),
            )?;
        }
    }
    let (rows, _) = screen.dimensions();
    for index in 0..rows {
        let row = screen.row(index).unwrap();
        write_row(
            &row[..screen.row_used_columns(index).unwrap()],
            screen.row_continued(index).unwrap(),
        )?;
    }
    Ok(text)
}

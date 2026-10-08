//! Bounded script requests, independent of the interactive session lease.

use base64::{Engine, engine::general_purpose::STANDARD};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::session::SessionName;
use serde::{Deserialize, Serialize};

mod cli;
pub use cli::{
    Command, PaneCommand, PaneTarget, ResizeDirection, Target, WindowCommand, WindowMoveDirection,
};

const MAX_REQUEST: usize = 64 * 1024;
pub(crate) const MAX_RESPONSE: usize = 16 * 1024 * 1024;
pub(crate) const MAX_INPUT: usize = 4096;
const MAX_CLIENTS: usize = 4;
const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Request {
    DisconnectSession {
        server_pid: i32,
    },
    RenameSession {
        source: String,
        name: String,
        server_pid: i32,
    },
    ShowConfig,
    ListPanes {
        toml: bool,
    },
    SelectPane {
        pane: u64,
    },
    SelectPaneDirection {
        pane: Option<u64>,
        direction: ResizeDirection,
    },
    SelectWindow {
        window: u16,
    },
    RenameWindow {
        window: Option<u16>,
        name: String,
    },
    CloseWindow {
        window: Option<u16>,
    },
    MoveWindow {
        window: Option<u16>,
        direction: WindowMoveDirection,
    },
    ResizePane {
        pane: Option<u64>,
        direction: ResizeDirection,
        cells: u16,
    },
    ZoomPane {
        pane: Option<u64>,
        zoom: Option<bool>,
    },
    SwapPane {
        pane: Option<u64>,
        to_pane: u64,
    },
    MovePane {
        pane: Option<u64>,
        direction: ResizeDirection,
    },
    NewWindow {
        name: Option<String>,
        command: Option<String>,
        cwd: Option<PathBuf>,
    },
    SplitPane {
        pane: Option<u64>,
        down: bool,
        command: Option<String>,
        cwd: Option<PathBuf>,
    },
    ClosePane {
        pane: Option<u64>,
    },
    SendKeys {
        pane: Option<u64>,
        bytes: Vec<u8>,
    },
    CapturePane {
        pane: Option<u64>,
        history: bool,
    },
    ReadPaneOutput {
        pane: Option<u64>,
        after: Option<u64>,
        #[serde(default)]
        require_retained: bool,
    },
    RespawnPane {
        pane: Option<u64>,
        command: Option<String>,
        cwd: Option<PathBuf>,
    },
    JoinPane {
        pane: Option<u64>,
        to_pane: u64,
        down: bool,
    },
    BreakPane {
        pane: Option<u64>,
        name: Option<String>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Response {
    ok: bool,
    output: String,
}

impl Command {
    pub fn run(self) -> io::Result<String> {
        match self {
            Self::Pane(command) => command.run(),
            Self::Window(command) => command.run(),
        }
    }
}

/// Inspect a running session's applied configuration and reload status.
pub fn show_config(session: &SessionName) -> io::Result<String> {
    request_session(session, &Request::ShowConfig)
}

impl PaneCommand {
    fn run(self) -> io::Result<String> {
        let (target, request) = match self {
            Self::Subscribe { target, after } => {
                let chunk = output_chunk(&target, after)?;
                follow_output(&target, chunk, &mut io::stdout().lock())?;
                return Ok(String::new());
            }
            Self::Log {
                target,
                output,
                after,
            } => {
                let chunk = output_chunk(&target, after)?;
                // Exclusive creation prevents overwriting files and following symlinks.
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(nix::libc::O_NOFOLLOW)
                    .open(output)?;
                follow_output(&target, chunk, &mut file)?;
                return Ok(String::new());
            }
            Self::ReadOutput { target, after } => (
                target.target,
                Request::ReadPaneOutput {
                    pane: target.pane,
                    after,
                    require_retained: false,
                },
            ),
            Self::List { target, toml } => (target, Request::ListPanes { toml }),
            Self::Select {
                target,
                pane,
                direction,
            } => (
                target,
                match direction {
                    Some(direction) => Request::SelectPaneDirection { pane, direction },
                    None => Request::SelectPane {
                        pane: pane
                            .ok_or_else(|| invalid("pane select requires --pane or --direction"))?,
                    },
                },
            ),
            Self::Resize {
                target,
                direction,
                cells,
            } => (
                target.target,
                Request::ResizePane {
                    pane: target.pane,
                    direction,
                    cells,
                },
            ),
            Self::Zoom { target, on, off } => (
                target.target,
                Request::ZoomPane {
                    pane: target.pane,
                    zoom: if on {
                        Some(true)
                    } else if off {
                        Some(false)
                    } else {
                        None
                    },
                },
            ),
            Self::Split {
                target,
                down,
                command,
                cwd,
            } => (
                target.target,
                Request::SplitPane {
                    pane: target.pane,
                    down,
                    command,
                    cwd,
                },
            ),
            Self::Close { target } => (target.target, Request::ClosePane { pane: target.pane }),
            Self::Swap { target, to_pane } => (
                target.target,
                Request::SwapPane {
                    pane: target.pane,
                    to_pane,
                },
            ),
            Self::Move { target, direction } => (
                target.target,
                Request::MovePane {
                    pane: target.pane,
                    direction,
                },
            ),
            Self::Capture { target, history } => (
                target.target,
                Request::CapturePane {
                    pane: target.pane,
                    history,
                },
            ),
            Self::Respawn {
                target,
                command,
                cwd,
            } => (
                target.target,
                Request::RespawnPane {
                    pane: target.pane,
                    command,
                    cwd,
                },
            ),
            Self::Join {
                target,
                to_pane,
                down,
            } => (
                target.target,
                Request::JoinPane {
                    pane: target.pane,
                    to_pane,
                    down,
                },
            ),
            Self::Break { target, name } => (
                target.target,
                Request::BreakPane {
                    pane: target.pane,
                    name,
                },
            ),
            Self::SendKeys {
                target,
                literal,
                enter,
                keys,
            } => {
                let bytes = input_bytes(&keys, literal, enter)?;
                (
                    target.target,
                    Request::SendKeys {
                        pane: target.pane,
                        bytes,
                    },
                )
            }
        };
        request_session(&target.session, &request)
    }
}

impl WindowCommand {
    fn run(self) -> io::Result<String> {
        let (target, request) = match self {
            Self::Select { target, window } => (target, Request::SelectWindow { window }),
            Self::Rename {
                target,
                window,
                name,
            } => (target, Request::RenameWindow { window, name }),
            Self::Close { target, window } => (target, Request::CloseWindow { window }),
            Self::Move {
                target,
                window,
                direction,
            } => (target, Request::MoveWindow { window, direction }),
            Self::New {
                target,
                name,
                command,
                cwd,
            } => (target, Request::NewWindow { name, command, cwd }),
        };
        request_session(&target.session, &request)
    }
}

fn output_chunk(target: &PaneTarget, after: Option<u64>) -> io::Result<crate::pane_output::Chunk> {
    let text = request_session(
        &target.target.session,
        &Request::ReadPaneOutput {
            pane: target.pane,
            after,
            require_retained: true,
        },
    )?;
    toml::from_str(&text).map_err(io::Error::other)
}

fn write_chunk(chunk: &crate::pane_output::Chunk, output: &mut impl Write) -> io::Result<()> {
    if chunk.dropped != 0 {
        return Err(io::Error::other(format!(
            "pane output lost {} bytes; next available cursor is {}",
            chunk.dropped, chunk.start
        )));
    }
    let bytes = STANDARD
        .decode(&chunk.bytes_base64)
        .map_err(io::Error::other)?;
    output.write_all(&bytes)?;
    output.flush()
}

fn follow_output(
    target: &PaneTarget,
    mut chunk: crate::pane_output::Chunk,
    output: &mut impl Write,
) -> io::Result<()> {
    // Resolve active pane exactly once so focus changes cannot redirect a subscriber.
    let pinned = PaneTarget {
        target: target.target.clone(),
        pane: Some(chunk.pane),
    };
    let generation = chunk.generation;
    let server_pid = chunk.server_pid;
    loop {
        if chunk.server_pid != server_pid {
            return Err(io::Error::other(
                "session server changed; start a new output subscription",
            ));
        }
        if chunk.generation != generation {
            return Err(io::Error::other(
                "pane was respawned; start a new output subscription",
            ));
        }
        write_chunk(&chunk, output)?;
        if chunk.complete {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
        chunk = output_chunk(&pinned, Some(chunk.next))?;
    }
}

fn input_bytes(keys: &[String], literal: bool, enter: bool) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if literal {
        // Bound before joining, including UTF-8 bytes and separator spaces.
        let length = keys
            .iter()
            .try_fold(keys.len().saturating_sub(1), |n, key| {
                n.checked_add(key.len())
            })
            .ok_or_else(|| invalid("input exceeds 4096 bytes"))?;
        if length > MAX_INPUT {
            return Err(invalid("input exceeds 4096 bytes"));
        }
        bytes.extend(keys.join(" ").as_bytes());
    } else {
        for key in keys {
            match key.to_ascii_lowercase().as_str() {
                "enter" => bytes.push(b'\r'),
                "tab" => bytes.push(b'\t'),
                "esc" | "escape" => bytes.push(27),
                "space" => bytes.push(b' '),
                "backspace" => bytes.push(127),
                "delete" => bytes.extend(b"\x1b[3~"),
                "up" => bytes.extend(b"\x1b[A"),
                "down" => bytes.extend(b"\x1b[B"),
                "right" => bytes.extend(b"\x1b[C"),
                "left" => bytes.extend(b"\x1b[D"),
                "home" => bytes.extend(b"\x1b[H"),
                "end" => bytes.extend(b"\x1b[F"),
                "pageup" | "page-up" => bytes.extend(b"\x1b[5~"),
                "pagedown" | "page-down" => bytes.extend(b"\x1b[6~"),
                name if name.starts_with("ctrl ") && name.len() == 6 => {
                    let letter = name.as_bytes()[5];
                    if !(b'@'..=b'_').contains(&letter.to_ascii_uppercase()) {
                        return Err(invalid(format!("unknown key: {key}")));
                    }
                    bytes.push(letter.to_ascii_uppercase() & 31);
                }
                _ if key.chars().count() == 1 => bytes.extend(key.as_bytes()),
                _ => {
                    return Err(invalid(format!(
                        "unknown key: {key}; use --literal for text"
                    )));
                }
            }
            if bytes.len() > MAX_INPUT {
                return Err(invalid("input exceeds 4096 bytes"));
            }
        }
    }
    if enter {
        bytes.push(b'\r');
    }
    if bytes.len() > MAX_INPUT {
        return Err(invalid("input exceeds 4096 bytes"));
    }
    Ok(bytes)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
fn frame(contents: &str) -> Vec<u8> {
    let mut bytes = (contents.len() as u32).to_be_bytes().to_vec();
    bytes.extend(contents.as_bytes());
    bytes
}

pub(crate) fn rename_session(
    old: &SessionName,
    new: &SessionName,
    server_pid: i32,
) -> io::Result<()> {
    request_session(
        old,
        &Request::RenameSession {
            source: old.as_str().into(),
            name: new.as_str().into(),
            server_pid,
        },
    )
    .map(|_| ())
}

pub(crate) fn disconnect_session(name: &SessionName, server_pid: i32) -> io::Result<()> {
    // Keep the alias stable and prevent a new client from acquiring this lease
    // until the original displayed client has released it. Never signal a PID.
    let deadline = Instant::now() + Duration::from_secs(5);
    let _workspace = crate::session::acquire_workspace(name)?;
    let unchanged = || {
        if crate::session::live_server_pid(name)?.as_raw() != server_pid {
            return Err(io::Error::other(
                "session server changed; refresh and retry",
            ));
        }
        Ok(())
    };
    unchanged()?;
    request_session(name, &Request::DisconnectSession { server_pid })?;
    crate::session::wait_for_client_release(name, deadline)?;
    unchanged()
}

fn request_session(name: &SessionName, request: &Request) -> io::Result<String> {
    crate::session::live_server_pid(name)?;
    crate::session::ensure_private_directory(&crate::session::session_directory())?;
    let path = crate::session::session_socket_path(name).with_extension("control");
    let metadata = fs::symlink_metadata(&path)?;
    if !private_socket(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe control endpoint",
        ));
    }
    let text = toml::to_string(request).map_err(io::Error::other)?;
    if text.len() > MAX_REQUEST {
        return Err(invalid("control request exceeds 64 KiB"));
    }
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(&frame(&text))?;
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_RESPONSE {
        return Err(invalid("control response exceeds 16 MiB"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let response: Response = toml::from_str(text).map_err(io::Error::other)?;
    if response.ok {
        Ok(response.output)
    } else {
        Err(io::Error::other(response.output))
    }
}

fn private_socket(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_socket()
        && metadata.uid() == nix::unistd::geteuid().as_raw()
        && metadata.mode() & 0o077 == 0
}

struct Client {
    stream: UnixStream,
    bytes: Vec<u8>,
    reply: Option<Vec<u8>>,
    offset: usize,
    deadline: Instant,
}
pub(crate) struct Service {
    listener: UnixListener,
    socket: PathBuf,
    identity: (u64, u64),
    clients: Vec<Client>,
    session_identity: Option<crate::session::rename::Identity>,
}
impl Service {
    pub(crate) fn bind(name: &SessionName) -> io::Result<Self> {
        let socket = crate::session::session_socket_path(name).with_extension("control");
        match fs::symlink_metadata(&socket) {
            Ok(metadata) if private_socket(&metadata) => fs::remove_file(&socket)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unsafe control endpoint",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let metadata = fs::symlink_metadata(&socket)?;
        Ok(Self {
            listener,
            socket,
            identity: (metadata.dev(), metadata.ino()),
            clients: Vec::new(),
            session_identity: None,
        })
    }

    pub(crate) fn track_identity(&mut self, identity: crate::session::rename::Identity) {
        self.session_identity = Some(identity);
    }

    /// Return true after a successful request that needs a frontend refresh.
    pub(crate) fn tick(&mut self, mut handle: impl FnMut(Request) -> io::Result<String>) -> bool {
        for _ in 0..MAX_CLIENTS {
            let Ok((stream, _)) = self.listener.accept() else {
                break;
            };
            if self.clients.len() < MAX_CLIENTS && stream.set_nonblocking(true).is_ok() {
                self.clients.push(Client {
                    stream,
                    bytes: Vec::new(),
                    reply: None,
                    offset: 0,
                    deadline: Instant::now() + TIMEOUT,
                });
            }
        }
        let mut handled = false;
        let mut reply_budget = MAX_RESPONSE;
        for client in &self.clients {
            reply_budget = reply_budget.saturating_sub(client.reply.as_ref().map_or(0, Vec::len));
        }
        self.clients.retain_mut(|client| {
            if Instant::now() >= client.deadline {
                return false;
            }
            if client.reply.is_none() {
                // One bounded read per client per tick prevents stalled or flooding
                // controllers from starving the PTY loop.
                let mut chunk = [0; 8192];
                match client.stream.read(&mut chunk) {
                    Ok(0) => return false,
                    Ok(n) => client.bytes.extend_from_slice(&chunk[..n]),
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return false,
                }
                if client.bytes.len() >= 4 {
                    let length = u32::from_be_bytes(client.bytes[..4].try_into().unwrap()) as usize;
                    if length > MAX_REQUEST || client.bytes.len() > length + 4 {
                        return false;
                    }
                    if client.bytes.len() == length + 4 {
                        let result = std::str::from_utf8(&client.bytes[4..])
                            .map_err(io::Error::other)
                            .and_then(|text| toml::from_str(text).map_err(io::Error::other))
                            .and_then(|request| {
                                let refresh = matches!(
                                    &request,
                                    Request::NewWindow { .. }
                                        | Request::SelectPane { .. }
                                        | Request::SelectPaneDirection { .. }
                                        | Request::SelectWindow { .. }
                                        | Request::RenameWindow { .. }
                                        | Request::CloseWindow { .. }
                                        | Request::MoveWindow { .. }
                                        | Request::ResizePane { .. }
                                        | Request::ZoomPane { .. }
                                        | Request::SwapPane { .. }
                                        | Request::MovePane { .. }
                                        | Request::SplitPane { .. }
                                        | Request::ClosePane { .. }
                                        | Request::JoinPane { .. }
                                        | Request::BreakPane { .. }
                                        | Request::RespawnPane { .. }
                                );
                                let result = handle(request);
                                handled |= refresh && result.is_ok();
                                result
                            });
                        let response = match result {
                            Ok(output) => Response { ok: true, output },
                            Err(error) => Response {
                                ok: false,
                                output: error.to_string().chars().take(512).collect(),
                            },
                        };
                        let text = toml::to_string(&response).unwrap_or_default();
                        let bytes = if text.len() + 4 <= reply_budget {
                            frame(&text)
                        } else {
                            frame(
                                &toml::to_string(&Response {
                                    ok: false,
                                    output: "control response exceeds available 16 MiB budget"
                                        .into(),
                                })
                                .unwrap(),
                            )
                        };
                        if bytes.len() > reply_budget {
                            return false;
                        }
                        reply_budget -= bytes.len();
                        client.reply = Some(bytes);
                        client.bytes.clear();
                        client.deadline = Instant::now() + TIMEOUT;
                    }
                }
            }
            if let Some(bytes) = &client.reply {
                match client.stream.write(&bytes[client.offset..]) {
                    Ok(0) => return false,
                    Ok(n) => {
                        client.offset += n;
                        if client.offset == bytes.len() {
                            return false;
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return false,
                }
            }
            true
        });
        handled
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let socket = self
            .session_identity
            .as_ref()
            .map(|identity| identity.path().with_extension("control"))
            .unwrap_or_else(|| self.socket.clone());
        if fs::symlink_metadata(&socket).is_ok_and(|m| (m.dev(), m.ino()) == self.identity) {
            let _ = fs::remove_file(&socket);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn select_pane_accepts_direction_with_active_or_explicit_origin() {
        for (name, direction) in [
            ("left", ResizeDirection::Left),
            ("right", ResizeDirection::Right),
            ("up", ResizeDirection::Up),
            ("down", ResizeDirection::Down),
        ] {
            for pane in [None, Some(0)] {
                let mut arguments = vec!["rustmux", "pane", "select", "--direction", name];
                if pane.is_some() {
                    arguments.extend(["-s", "work", "-p", "0"]);
                }
                let cli = crate::cli::Cli::try_parse_from(arguments).unwrap();
                let Some(crate::cli::Command::Control(Command::Pane(PaneCommand::Select {
                    target,
                    pane: parsed_pane,
                    direction: parsed,
                }))) = cli.command
                else {
                    panic!("expected select-pane");
                };
                assert_eq!((parsed_pane, parsed), (pane, Some(direction)));
                assert_eq!(
                    target.session.as_str(),
                    if pane.is_some() { "work" } else { "default" }
                );
            }
        }
        for arguments in [
            vec!["rustmux", "pane", "select", "--direction"],
            vec!["rustmux", "pane", "select", "--direction", "next"],
            vec![
                "rustmux",
                "pane",
                "select",
                "-p",
                "invalid",
                "--direction",
                "left",
            ],
        ] {
            assert!(crate::cli::Cli::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn move_pane_accepts_active_or_explicit_source_and_requires_cardinal_direction() {
        for (name, direction) in [
            ("left", ResizeDirection::Left),
            ("right", ResizeDirection::Right),
            ("up", ResizeDirection::Up),
            ("down", ResizeDirection::Down),
        ] {
            for pane in [None, Some(0)] {
                let mut arguments = vec!["rustmux", "pane", "move", "--direction", name];
                if pane.is_some() {
                    arguments.extend(["-s", "work", "-p", "0"]);
                }
                let cli = crate::cli::Cli::try_parse_from(arguments).unwrap();
                let Some(crate::cli::Command::Control(Command::Pane(PaneCommand::Move {
                    target,
                    direction: parsed,
                }))) = cli.command
                else {
                    panic!("expected move-pane");
                };
                assert_eq!((target.pane, parsed), (pane, direction));
                assert_eq!(
                    target.target.session.as_str(),
                    if pane.is_some() { "work" } else { "default" }
                );
            }
        }
        for arguments in [
            vec!["rustmux", "pane", "move"],
            vec!["rustmux", "pane", "move", "--direction", "next"],
            vec![
                "rustmux",
                "pane",
                "move",
                "-p",
                "invalid",
                "--direction",
                "left",
            ],
        ] {
            assert!(crate::cli::Cli::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn swap_pane_accepts_active_or_explicit_source_and_requires_destination() {
        for (arguments, pane, to_pane) in [
            (vec!["rustmux", "pane", "swap", "--to-pane", "0"], None, 0),
            (
                vec![
                    "rustmux",
                    "pane",
                    "swap",
                    "-s",
                    "work",
                    "-p",
                    "2",
                    "--to-pane",
                    "3",
                ],
                Some(2),
                3,
            ),
        ] {
            let cli = crate::cli::Cli::try_parse_from(arguments).unwrap();
            let Some(crate::cli::Command::Control(Command::Pane(PaneCommand::Swap {
                target,
                to_pane: parsed,
            }))) = cli.command
            else {
                panic!("expected swap-pane");
            };
            assert_eq!((target.pane, parsed), (pane, to_pane));
            assert_eq!(
                target.target.session.as_str(),
                if pane.is_some() { "work" } else { "default" }
            );
        }
        for arguments in [
            vec!["rustmux", "pane", "swap"],
            vec!["rustmux", "pane", "swap", "--to-pane", "invalid"],
            vec!["rustmux", "pane", "swap", "-p", "invalid", "--to-pane", "0"],
        ] {
            assert!(crate::cli::Cli::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn move_window_accepts_default_or_positive_target_and_horizontal_direction() {
        for (arguments, window, direction) in [
            (
                vec!["rustmux", "window", "move", "--direction", "left"],
                None,
                WindowMoveDirection::Left,
            ),
            (
                vec![
                    "rustmux",
                    "window",
                    "move",
                    "-s",
                    "work",
                    "-w",
                    "2",
                    "--direction",
                    "right",
                ],
                Some(2),
                WindowMoveDirection::Right,
            ),
        ] {
            let cli = crate::cli::Cli::try_parse_from(arguments).unwrap();
            let Some(crate::cli::Command::Control(Command::Window(WindowCommand::Move {
                target,
                window: parsed,
                direction: parsed_direction,
            }))) = cli.command
            else {
                panic!("expected move-window");
            };
            assert_eq!((parsed, parsed_direction), (window, direction));
            assert_eq!(
                target.session.as_str(),
                if window.is_some() { "work" } else { "default" }
            );
        }
        for arguments in [
            vec!["rustmux", "window", "move"],
            vec!["rustmux", "window", "move", "--direction", "up"],
            vec![
                "rustmux",
                "window",
                "move",
                "-w",
                "0",
                "--direction",
                "right",
            ],
        ] {
            assert!(crate::cli::Cli::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn zoom_pane_accepts_default_toggle_or_exclusive_explicit_state() {
        for (arguments, pane, on, off) in [
            (vec!["rustmux", "pane", "zoom"], None, false, false),
            (
                vec!["rustmux", "pane", "zoom", "-s", "work", "-p", "0", "--on"],
                Some(0),
                true,
                false,
            ),
            (vec!["rustmux", "pane", "zoom", "--off"], None, false, true),
        ] {
            let cli = crate::cli::Cli::try_parse_from(arguments).unwrap();
            let Some(crate::cli::Command::Control(Command::Pane(PaneCommand::Zoom {
                target,
                on: parsed_on,
                off: parsed_off,
            }))) = cli.command
            else {
                panic!("expected zoom-pane");
            };
            assert_eq!((target.pane, parsed_on, parsed_off), (pane, on, off));
            assert_eq!(
                target.target.session.as_str(),
                if pane.is_some() { "work" } else { "default" }
            );
        }
        assert!(
            crate::cli::Cli::try_parse_from(["rustmux", "pane", "zoom", "--on", "--off"]).is_err()
        );
        assert!(
            crate::cli::Cli::try_parse_from(["rustmux", "pane", "zoom", "-p", "invalid"]).is_err()
        );
    }

    #[test]
    fn close_window_accepts_active_default_or_positive_window_number() {
        for (arguments, window) in [
            (vec!["rustmux", "window", "close"], None),
            (
                vec!["rustmux", "window", "close", "-s", "work", "-w", "2"],
                Some(2),
            ),
        ] {
            let cli = crate::cli::Cli::try_parse_from(arguments).unwrap();
            let Some(crate::cli::Command::Control(Command::Window(WindowCommand::Close {
                target,
                window: parsed,
            }))) = cli.command
            else {
                panic!("expected close-window");
            };
            assert_eq!(parsed, window);
            assert_eq!(
                target.session.as_str(),
                if window.is_some() { "work" } else { "default" }
            );
        }
        for invalid in ["0", "-1", "65536", "invalid"] {
            assert!(
                crate::cli::Cli::try_parse_from(["rustmux", "window", "close", "-w", invalid])
                    .is_err()
            );
        }
    }

    #[test]
    fn close_pane_accepts_active_default_or_explicit_runtime_id() {
        for (arguments, pane) in [
            (vec!["rustmux", "pane", "close"], None),
            (
                vec!["rustmux", "pane", "close", "-s", "work", "-p", "42"],
                Some(42),
            ),
        ] {
            let cli = crate::cli::Cli::try_parse_from(arguments).unwrap();
            let Some(crate::cli::Command::Control(Command::Pane(PaneCommand::Close { target }))) =
                cli.command
            else {
                panic!("expected close-pane");
            };
            assert_eq!(target.pane, pane);
            assert_eq!(
                target.target.session.as_str(),
                if pane.is_some() { "work" } else { "default" }
            );
        }
        assert!(
            crate::cli::Cli::try_parse_from(["rustmux", "pane", "close", "-p", "invalid"]).is_err()
        );
    }

    #[test]
    fn creation_arguments_preserve_startup_command_and_directory() {
        for (group, action) in [("window", "new"), ("pane", "split")] {
            let cli = crate::cli::Cli::try_parse_from([
                "rustmux",
                group,
                action,
                "--cwd",
                "/tmp/project with spaces",
                "--command",
                "printf 'hello world'; exit 7",
            ])
            .unwrap();
            let Some(crate::cli::Command::Control(command)) = cli.command else {
                panic!("expected control command")
            };
            let (startup, cwd) = match command {
                Command::Window(WindowCommand::New { command, cwd, .. })
                | Command::Pane(PaneCommand::Split { command, cwd, .. }) => (command, cwd),
                _ => panic!("expected pane creation"),
            };
            assert_eq!(startup.as_deref(), Some("printf 'hello world'; exit 7"));
            assert_eq!(cwd, Some(PathBuf::from("/tmp/project with spaces")));
            let cli = crate::cli::Cli::try_parse_from(["rustmux", group, action]).unwrap();
            assert!(matches!(
                cli.command,
                Some(crate::cli::Command::Control(
                    Command::Window(WindowCommand::New {
                        command: None,
                        cwd: None,
                        ..
                    }) | Command::Pane(PaneCommand::Split {
                        command: None,
                        cwd: None,
                        ..
                    })
                ))
            ));
        }
    }

    #[test]
    fn legacy_creation_requests_omit_optional_startup_fields() {
        for body in ["action='new-window'", "action='split-pane'\ndown=true"] {
            let request = toml::from_str::<Request>(body).unwrap();
            assert!(matches!(
                request,
                Request::NewWindow {
                    command: None,
                    cwd: None,
                    ..
                } | Request::SplitPane {
                    command: None,
                    cwd: None,
                    ..
                }
            ));
        }
    }

    #[test]
    fn resize_arguments_require_direction_and_positive_bounded_cells() {
        let cli =
            crate::cli::Cli::try_parse_from(["rustmux", "pane", "resize", "--direction", "right"])
                .unwrap();
        assert_eq!(
            cli.command,
            Some(crate::cli::Command::Control(Command::Pane(
                PaneCommand::Resize {
                    target: PaneTarget {
                        target: Target {
                            session: SessionName::new("default").unwrap()
                        },
                        pane: None,
                    },
                    direction: ResizeDirection::Right,
                    cells: 1,
                }
            )))
        );
        for arguments in [
            &["rustmux", "pane", "resize"][..],
            &["rustmux", "pane", "resize", "--direction", "diagonal"][..],
            &[
                "rustmux",
                "pane",
                "resize",
                "--direction",
                "left",
                "--cells",
                "0",
            ][..],
            &[
                "rustmux",
                "pane",
                "resize",
                "--direction",
                "up",
                "--cells",
                "65536",
            ][..],
            &[
                "rustmux",
                "pane",
                "resize",
                "--direction",
                "down",
                "--cells",
                "-1",
            ][..],
        ] {
            assert!(crate::cli::Cli::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn rename_window_uses_current_or_one_based_explicit_target() {
        for (arguments, window) in [
            (vec!["rustmux", "window", "rename", "工作区"], None),
            (
                vec!["rustmux", "window", "rename", "-w", "2", "工作区"],
                Some(2),
            ),
        ] {
            let cli = crate::cli::Cli::try_parse_from(arguments).unwrap();
            assert_eq!(
                cli.command,
                Some(crate::cli::Command::Control(Command::Window(
                    WindowCommand::Rename {
                        target: Target {
                            session: SessionName::new("default").unwrap()
                        },
                        window,
                        name: "工作区".into(),
                    }
                )))
            );
        }
        for arguments in [
            &["rustmux", "window", "rename"][..],
            &["rustmux", "window", "rename", "-w", "0", "name"][..],
            &["rustmux", "window", "rename", "-w", "65536", "name"][..],
            &["rustmux", "window", "rename", "-w", "-1", "name"][..],
        ] {
            assert!(crate::cli::Cli::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn focus_commands_require_explicit_pane_ids_and_one_based_window_numbers() {
        let cli =
            crate::cli::Cli::try_parse_from(["rustmux", "pane", "select", "-s", "work", "-p", "0"])
                .unwrap();
        assert_eq!(
            cli.command,
            Some(crate::cli::Command::Control(Command::Pane(
                PaneCommand::Select {
                    target: Target {
                        session: SessionName::new("work").unwrap(),
                    },
                    pane: Some(0),
                    direction: None,
                }
            )))
        );
        let cli = crate::cli::Cli::try_parse_from(["rustmux", "window", "select", "--window", "2"])
            .unwrap();
        assert_eq!(
            cli.command,
            Some(crate::cli::Command::Control(Command::Window(
                WindowCommand::Select {
                    target: Target {
                        session: SessionName::new("default").unwrap(),
                    },
                    window: 2,
                }
            )))
        );
        for arguments in [
            &["rustmux", "pane", "select"][..],
            &["rustmux", "pane", "select", "-p", "-1"][..],
            &["rustmux", "window", "select"][..],
            &["rustmux", "window", "select", "-w", "0"][..],
            &["rustmux", "window", "select", "-w", "-1"][..],
        ] {
            assert!(crate::cli::Cli::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn input_validates_names_and_exact_byte_limits_before_sending() {
        assert_eq!(
            input_bytes(
                &["Ctrl c".into(), "上".into(), "enter".into()],
                false,
                false
            )
            .unwrap(),
            "\u{3}上\r".as_bytes()
        );
        assert!(input_bytes(&["unknown".into()], false, false).is_err());
        assert!(input_bytes(&["x".repeat(MAX_INPUT)], true, true).is_err());
        assert_eq!(
            input_bytes(&["x".repeat(MAX_INPUT)], true, false)
                .unwrap()
                .len(),
            MAX_INPUT
        );
    }
    #[test]
    fn unknown_wire_fields_and_actions_are_rejected() {
        assert!(toml::from_str::<Request>("action='list-panes'\ntoml=true\nextra=1").is_err());
        assert!(toml::from_str::<Request>("action='kill'").is_err());
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;
    use crate::pane_output::{Chunk, Output};

    #[test]
    fn subscriber_rejects_gaps_before_writing() {
        let mut output = Output::default();
        output.append(&vec![b'x'; crate::pane_output::LIMIT + 1]);
        let chunk = output.read(1, Some(0), false).unwrap();
        let mut written = Vec::new();
        assert!(write_chunk(&chunk, &mut written).is_err());
        assert!(written.is_empty());
    }

    #[test]
    fn write_errors_are_returned_and_raw_bytes_preserved() {
        let mut output = Output::default();
        output.append(b"\xff\x1b[31m");
        let chunk = output.read(1, Some(0), false).unwrap();
        let wire = toml::to_string(&chunk).unwrap();
        let decoded: Chunk = toml::from_str(&wire).unwrap();
        let mut written = Vec::new();
        write_chunk(&decoded, &mut written).unwrap();
        assert_eq!(written, b"\xff\x1b[31m");
        let mut full = &mut [0u8; 1][..];
        assert_eq!(
            write_chunk(&decoded, &mut full).unwrap_err().kind(),
            io::ErrorKind::WriteZero
        );
    }
}

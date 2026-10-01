//! Bounded script requests, independent of the interactive session lease.

use base64::{Engine, engine::general_purpose::STANDARD};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::session::SessionName;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

const MAX_REQUEST: usize = 64 * 1024;
pub(crate) const MAX_RESPONSE: usize = 16 * 1024 * 1024;
pub(crate) const MAX_INPUT: usize = 4096;
const MAX_CLIENTS: usize = 4;
const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct Target {
    #[arg(short = 's', long, default_value = "default")]
    pub session: SessionName,
}
#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct PaneTarget {
    #[command(flatten)]
    pub target: Target,
    #[arg(short = 'p', long)]
    pub pane: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResizeDirection {
    Left,
    Right,
    Up,
    Down,
}

impl From<ResizeDirection> for crate::layout::Direction {
    fn from(direction: ResizeDirection) -> Self {
        match direction {
            ResizeDirection::Left => Self::Left,
            ResizeDirection::Right => Self::Right,
            ResizeDirection::Up => Self::Up,
            ResizeDirection::Down => Self::Down,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    /// Show the server's active settings and configuration reload status as TOML.
    ShowConfig {
        #[command(flatten)]
        target: Target,
    },
    /// List runtime pane IDs, windows, focus and working directories.
    ListPanes {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        toml: bool,
    },
    /// Focus a runtime pane ID, including its window, without restarting it.
    SelectPane {
        #[command(flatten)]
        target: Target,
        #[arg(short = 'p', long)]
        pane: u64,
    },
    /// Focus a one-based window number, preserving that window's selected pane.
    SelectWindow {
        #[command(flatten)]
        target: Target,
        #[arg(short = 'w', long, value_parser = clap::value_parser!(u16).range(1..))]
        window: u16,
    },
    /// Rename a window without changing focus; defaults to the active window.
    RenameWindow {
        #[command(flatten)]
        target: Target,
        #[arg(short = 'w', long, value_parser = clap::value_parser!(u16).range(1..))]
        window: Option<u16>,
        name: String,
    },
    /// Move a pane's nearest separator, preserving focus; defaults to the active pane.
    ResizePane {
        #[command(flatten)]
        target: PaneTarget,
        /// Separator movement, rather than growth of the target pane.
        #[arg(long, value_enum)]
        direction: ResizeDirection,
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..))]
        cells: u16,
    },
    /// Create a window, focus it, and print the new pane ID.
    NewWindow {
        #[command(flatten)]
        target: Target,
        #[arg(short = 'n', long)]
        name: Option<String>,
        /// Run COMMAND through the configured server shell instead of opening an interactive shell.
        #[arg(long)]
        command: Option<String>,
        /// Start in an absolute existing directory; otherwise inherit the active pane's directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Split a pane to the right, or below with --down.
    SplitPane {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(long)]
        down: bool,
        /// Run COMMAND through the configured server shell instead of opening an interactive shell.
        #[arg(long)]
        command: Option<String>,
        /// Start in an absolute existing directory; otherwise inherit the target pane's directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Close a pane and stop its process; the final session pane cannot be closed.
    ClosePane {
        #[command(flatten)]
        target: PaneTarget,
    },
    /// Send named keys or --literal text; optionally append Enter.
    SendKeys {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(short = 'l', long)]
        literal: bool,
        #[arg(long)]
        enter: bool,
        #[arg(required=true, num_args=1..)]
        keys: Vec<String>,
    },
    /// Capture plain text from the visible pane, or retained text with --history.
    CapturePane {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(long)]
        history: bool,
    },
    /// Read a bounded raw PTY tail as TOML with base64 bytes and a byte cursor.
    ReadPaneOutput {
        #[command(flatten)]
        target: PaneTarget,
        /// Omit to obtain the current cursor without replaying output.
        #[arg(long)]
        after: Option<u64>,
    },
    /// Stream future raw PTY bytes to stdout; requires remain_on_exit = true.
    SubscribePane {
        #[command(flatten)]
        target: PaneTarget,
        /// Resume from a byte cursor rather than starting at the current tail.
        #[arg(long)]
        after: Option<u64>,
    },
    /// Log raw PTY bytes into a new file until EOF; requires remain_on_exit = true.
    LogPane {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        after: Option<u64>,
    },
    /// Restart an exited pane in place, preserving its runtime ID.
    RespawnPane {
        #[command(flatten)]
        target: PaneTarget,
        /// Override the recorded startup command (run through the configured shell).
        #[arg(long)]
        command: Option<String>,
        /// Override the startup directory; must be an absolute existing directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Move a pane beside a pane in another window, preserving its runtime ID.
    JoinPane {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(long)]
        to_pane: u64,
        #[arg(long)]
        down: bool,
    },
    /// Move a pane into a new window, preserving its process and runtime ID.
    BreakPane {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(short = 'n', long)]
        name: Option<String>,
    },
}

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
    SelectWindow {
        window: u16,
    },
    RenameWindow {
        window: Option<u16>,
        name: String,
    },
    ResizePane {
        pane: Option<u64>,
        direction: ResizeDirection,
        cells: u16,
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
        let (target, request) = match self {
            Self::ShowConfig { target } => (target, Request::ShowConfig),
            Self::SubscribePane { target, after } => {
                let chunk = output_chunk(&target, after)?;
                follow_output(&target, chunk, &mut io::stdout().lock())?;
                return Ok(String::new());
            }
            Self::LogPane {
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
            Self::ReadPaneOutput { target, after } => (
                target.target,
                Request::ReadPaneOutput {
                    pane: target.pane,
                    after,
                    require_retained: false,
                },
            ),
            Self::ListPanes { target, toml } => (target, Request::ListPanes { toml }),
            Self::SelectPane { target, pane } => (target, Request::SelectPane { pane }),
            Self::SelectWindow { target, window } => (target, Request::SelectWindow { window }),
            Self::RenameWindow {
                target,
                window,
                name,
            } => (target, Request::RenameWindow { window, name }),
            Self::ResizePane {
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
            Self::NewWindow {
                target,
                name,
                command,
                cwd,
            } => (target, Request::NewWindow { name, command, cwd }),
            Self::SplitPane {
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
            Self::ClosePane { target } => (target.target, Request::ClosePane { pane: target.pane }),
            Self::CapturePane { target, history } => (
                target.target,
                Request::CapturePane {
                    pane: target.pane,
                    history,
                },
            ),
            Self::RespawnPane {
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
            Self::JoinPane {
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
            Self::BreakPane { target, name } => (
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
                                        | Request::SelectWindow { .. }
                                        | Request::RenameWindow { .. }
                                        | Request::ResizePane { .. }
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
    fn close_pane_accepts_active_default_or_explicit_runtime_id() {
        for (arguments, pane) in [
            (vec!["rustmux", "close-pane"], None),
            (
                vec!["rustmux", "close-pane", "-s", "work", "-p", "42"],
                Some(42),
            ),
        ] {
            let cli = crate::cli::Cli::try_parse_from(arguments).unwrap();
            let Some(crate::cli::Command::Control(Command::ClosePane { target })) = cli.command
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
            crate::cli::Cli::try_parse_from(["rustmux", "close-pane", "-p", "invalid"]).is_err()
        );
    }

    #[test]
    fn creation_arguments_preserve_startup_command_and_directory() {
        for action in ["new-window", "split-pane"] {
            let cli = crate::cli::Cli::try_parse_from([
                "rustmux",
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
                Command::NewWindow { command, cwd, .. }
                | Command::SplitPane { command, cwd, .. } => (command, cwd),
                _ => panic!("expected pane creation"),
            };
            assert_eq!(startup.as_deref(), Some("printf 'hello world'; exit 7"));
            assert_eq!(cwd, Some(PathBuf::from("/tmp/project with spaces")));
            let cli = crate::cli::Cli::try_parse_from(["rustmux", action]).unwrap();
            assert!(matches!(
                cli.command,
                Some(crate::cli::Command::Control(
                    Command::NewWindow {
                        command: None,
                        cwd: None,
                        ..
                    } | Command::SplitPane {
                        command: None,
                        cwd: None,
                        ..
                    }
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
            crate::cli::Cli::try_parse_from(["rustmux", "resize-pane", "--direction", "right"])
                .unwrap();
        assert_eq!(
            cli.command,
            Some(crate::cli::Command::Control(Command::ResizePane {
                target: PaneTarget {
                    target: Target {
                        session: SessionName::new("default").unwrap()
                    },
                    pane: None,
                },
                direction: ResizeDirection::Right,
                cells: 1,
            }))
        );
        for arguments in [
            &["rustmux", "resize-pane"][..],
            &["rustmux", "resize-pane", "--direction", "diagonal"][..],
            &[
                "rustmux",
                "resize-pane",
                "--direction",
                "left",
                "--cells",
                "0",
            ][..],
            &[
                "rustmux",
                "resize-pane",
                "--direction",
                "up",
                "--cells",
                "65536",
            ][..],
            &[
                "rustmux",
                "resize-pane",
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
            (vec!["rustmux", "rename-window", "工作区"], None),
            (
                vec!["rustmux", "rename-window", "-w", "2", "工作区"],
                Some(2),
            ),
        ] {
            let cli = crate::cli::Cli::try_parse_from(arguments).unwrap();
            assert_eq!(
                cli.command,
                Some(crate::cli::Command::Control(Command::RenameWindow {
                    target: Target {
                        session: SessionName::new("default").unwrap()
                    },
                    window,
                    name: "工作区".into(),
                }))
            );
        }
        for arguments in [
            &["rustmux", "rename-window"][..],
            &["rustmux", "rename-window", "-w", "0", "name"][..],
            &["rustmux", "rename-window", "-w", "65536", "name"][..],
            &["rustmux", "rename-window", "-w", "-1", "name"][..],
        ] {
            assert!(crate::cli::Cli::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn focus_commands_require_explicit_pane_ids_and_one_based_window_numbers() {
        let cli =
            crate::cli::Cli::try_parse_from(["rustmux", "select-pane", "-s", "work", "-p", "0"])
                .unwrap();
        assert_eq!(
            cli.command,
            Some(crate::cli::Command::Control(Command::SelectPane {
                target: Target {
                    session: SessionName::new("work").unwrap(),
                },
                pane: 0,
            }))
        );
        let cli =
            crate::cli::Cli::try_parse_from(["rustmux", "select-window", "--window", "2"]).unwrap();
        assert_eq!(
            cli.command,
            Some(crate::cli::Command::Control(Command::SelectWindow {
                target: Target {
                    session: SessionName::new("default").unwrap(),
                },
                window: 2,
            }))
        );
        for arguments in [
            &["rustmux", "select-pane"][..],
            &["rustmux", "select-pane", "-p", "-1"][..],
            &["rustmux", "select-window"][..],
            &["rustmux", "select-window", "-w", "0"][..],
            &["rustmux", "select-window", "-w", "-1"][..],
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

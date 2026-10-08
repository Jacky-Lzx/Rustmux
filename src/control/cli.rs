//! Grouped command-line definitions for script control.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::session::SessionName;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowMoveDirection {
    Left,
    Right,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    /// Inspect and control panes in a running session.
    #[command(subcommand)]
    Pane(PaneCommand),
    /// Create and control windows in a running session.
    #[command(subcommand)]
    Window(WindowCommand),
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum PaneCommand {
    /// List runtime pane IDs, windows, focus and working directories.
    #[command(visible_alias = "ls")]
    List {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        toml: bool,
    },
    /// Focus a runtime pane, or its directional neighbor, including its window.
    Select {
        #[command(flatten)]
        target: Target,
        /// Target pane, or origin for --direction; otherwise use the active pane.
        #[arg(short = 'p', long, required_unless_present = "direction")]
        pane: Option<u64>,
        #[arg(long, value_enum)]
        direction: Option<ResizeDirection>,
    },
    /// Move a pane's nearest separator, preserving focus; defaults to the active pane.
    Resize {
        #[command(flatten)]
        target: PaneTarget,
        /// Separator movement, rather than growth of the target pane.
        #[arg(long, value_enum)]
        direction: ResizeDirection,
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..))]
        cells: u16,
    },
    /// Focus a pane and toggle its full-window view, or set it with --on/--off.
    Zoom {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(long, conflicts_with = "off")]
        on: bool,
        #[arg(long, conflicts_with = "on")]
        off: bool,
    },
    /// Exchange two pane positions in one window without changing focus.
    Swap {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(long)]
        to_pane: u64,
    },
    /// Exchange a pane with its nearest directional neighbor without changing focus.
    Move {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(long, value_enum)]
        direction: ResizeDirection,
    },
    /// Split a pane to the right, or below with --down.
    Split {
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
    Close {
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
    Capture {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(long)]
        history: bool,
    },
    /// Read a bounded raw PTY tail as TOML with base64 bytes and a byte cursor.
    ReadOutput {
        #[command(flatten)]
        target: PaneTarget,
        /// Omit to obtain the current cursor without replaying output.
        #[arg(long)]
        after: Option<u64>,
    },
    /// Stream future raw PTY bytes to stdout; requires remain_on_exit = true.
    Subscribe {
        #[command(flatten)]
        target: PaneTarget,
        /// Resume from a byte cursor rather than starting at the current tail.
        #[arg(long)]
        after: Option<u64>,
    },
    /// Log raw PTY bytes into a new file until EOF; requires remain_on_exit = true.
    Log {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        after: Option<u64>,
    },
    /// Restart an exited pane in place, preserving its runtime ID.
    Respawn {
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
    Join {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(long)]
        to_pane: u64,
        #[arg(long)]
        down: bool,
    },
    /// Move a pane into a new window, preserving its process and runtime ID.
    Break {
        #[command(flatten)]
        target: PaneTarget,
        #[arg(short = 'n', long)]
        name: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum WindowCommand {
    /// Create a window, focus it, and print the new pane ID.
    New {
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
    /// Focus a one-based window number, preserving that window's selected pane.
    Select {
        #[command(flatten)]
        target: Target,
        #[arg(short = 'w', long, value_parser = clap::value_parser!(u16).range(1..))]
        window: u16,
    },
    /// Rename a window without changing focus; defaults to the active window.
    Rename {
        #[command(flatten)]
        target: Target,
        #[arg(short = 'w', long, value_parser = clap::value_parser!(u16).range(1..))]
        window: Option<u16>,
        name: String,
    },
    /// Close a window and all its panes; the final session window cannot be closed.
    Close {
        #[command(flatten)]
        target: Target,
        #[arg(short = 'w', long, value_parser = clap::value_parser!(u16).range(1..))]
        window: Option<u16>,
    },
    /// Move a window one position in the bar, wrapping at edges and preserving focus.
    Move {
        #[command(flatten)]
        target: Target,
        #[arg(short = 'w', long, value_parser = clap::value_parser!(u16).range(1..))]
        window: Option<u16>,
        #[arg(long, value_enum)]
        direction: WindowMoveDirection,
    },
}

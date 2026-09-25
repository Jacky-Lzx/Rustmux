//! Building blocks for the human-reviewed Rustmux implementation.

mod chrome;
pub mod cli;
mod closed_pane;
pub mod config;
pub mod graphics;
pub mod graphics_capability;
pub mod graphics_composite;
pub mod graphics_decode;
pub mod graphics_output;
mod graphics_reply;
pub mod graphics_snapshot;
pub mod graphics_store;
pub mod graphics_transfer;
mod history_view;
pub mod layout;
pub mod pane;
pub mod pane_set;
pub mod pane_view;
pub mod parser;
mod prompt;
pub mod pty;
pub mod render;
pub mod screen;
mod scrollback;
mod semantic;
pub mod session;
mod shortcut_help;
pub mod style;
pub mod terminal;
mod terminal_device;
mod theme;

pub mod window;

/// Marks processes started inside a Rustmux pane.
#[doc(hidden)]
pub const RUSTMUX_ENV: &str = "RUSTMUX";

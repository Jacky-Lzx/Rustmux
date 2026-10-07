//! Building blocks for the human-reviewed Rustmux implementation.

mod capability;
mod chrome;
pub mod cli;
mod clipboard;
mod closed_pane;
pub mod config;
pub mod control;
pub mod graphics;
pub use graphics::{
    capability as graphics_capability, composite as graphics_composite, decode as graphics_decode,
    output as graphics_output, placeholder as graphics_placeholder, snapshot as graphics_snapshot,
    store as graphics_store, transfer as graphics_transfer,
};
pub(crate) use graphics::{
    reply as graphics_reply, shared_memory_output as graphics_shared_memory_output,
};
mod history_view;
mod hyperlink;
pub use hyperlink::Hyperlink;
pub mod layout;
mod notification;
pub mod pane;
mod pane_output;
pub mod pane_set;
pub mod pane_view;
pub mod parser;
mod persistence;
mod pointer;
mod project;
mod prompt;
pub mod pty;
pub mod render;
pub mod screen;
mod scrollback;
mod semantic;
pub mod session;
mod shortcut_help;
mod structured_colors;
pub mod style;
pub mod terminal;
mod terminal_colors;
mod terminal_device;
mod theme;

pub mod window;

/// Marks processes started inside a Rustmux pane.
#[doc(hidden)]
pub const RUSTMUX_ENV: &str = "RUSTMUX";

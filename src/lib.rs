//! Building blocks for the human-reviewed Rustmux implementation.

mod chrome;
mod closed_pane;
pub mod config;
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
pub mod style;
pub mod terminal;
mod terminal_device;

pub mod window;

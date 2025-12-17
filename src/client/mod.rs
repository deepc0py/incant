//! Client module for the llmcmd CLI.
//!
//! The client is a lightweight process that:
//! - Renders a minimal TUI input prompt
//! - Gathers terminal context
//! - Sends queries to the daemon via Unix socket
//! - Outputs the generated command to stdout

pub mod socket;
pub mod tui;

pub use socket::send_query;
pub use tui::run_tui;

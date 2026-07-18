//! Daemon module for the incant daemon.
//!
//! The daemon is a long-running process that:
//! - Holds the LLM connection (Ollama or API)
//! - Listens on the platform-native same-user IPC endpoint
//! - Pre-caches the system prompt
//! - Handles inference requests

pub mod llm;
pub mod server;

pub use server::DaemonServer;

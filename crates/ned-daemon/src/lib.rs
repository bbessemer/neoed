//! The per-workspace daemon that keeps language servers warm, and its client
//! (command-language spec §1.1). Unix-only.
#![cfg(unix)]

pub mod client;
pub mod lsp;
pub mod paths;
pub mod protocol;
pub mod server;
pub mod servers;

pub use client::Client;
pub use paths::Paths;

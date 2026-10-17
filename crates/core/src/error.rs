//! Error type shared by services, the MCP server, and the UI.
//!
//! The TS app surfaces raw `Error` messages to users and MCP clients
//! ("Title required", "Board not found", ...), so the Rust port keeps a
//! free-form message string to preserve them verbatim.

/// Kandrium error — a user-facing message string.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct KandriumError(pub String);

impl KandriumError {
    pub fn new(msg: impl Into<String>) -> Self {
        KandriumError(msg.into())
    }
}

impl From<rusqlite::Error> for KandriumError {
    fn from(e: rusqlite::Error) -> Self {
        KandriumError(e.to_string())
    }
}

impl From<std::io::Error> for KandriumError {
    fn from(e: std::io::Error) -> Self {
        KandriumError(e.to_string())
    }
}

impl From<serde_json::Error> for KandriumError {
    fn from(e: serde_json::Error) -> Self {
        KandriumError(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, KandriumError>;

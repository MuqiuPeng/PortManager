//! Errors that cross process boundaries.
//!
//! The daemon serialises these to the CLI and MCP, so the variants are part of
//! the public protocol and each carries enough context to act on.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RuntimeError {
    #[error("{kind} not found: {id}")]
    NotFound { kind: String, id: String },

    #[error("{0}")]
    AlreadyExists(String),

    #[error("port {port} is in use by {holder}")]
    PortConflict { port: u16, holder: String },

    #[error("no free port found in range {from}-{to}")]
    NoPortAvailable { from: u16, to: u16 },

    #[error("service '{service}' is already running (pid {pid})")]
    AlreadyRunning { service: String, pid: u32 },

    #[error("service '{service}' is not running")]
    NotRunning { service: String },

    #[error("refusing to terminate pid {pid}: {reason}")]
    NotPermitted { pid: u32, reason: String },

    #[error("{0}")]
    InvalidInput(String),

    #[error("not supported on this platform: {0}")]
    Unsupported(String),

    #[error("{0}")]
    Io(String),

    #[error("{0}")]
    Internal(String),
}

impl RuntimeError {
    pub fn not_found(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self::NotFound {
            kind: kind.into(),
            id: id.into(),
        }
    }

    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }
}

impl From<std::io::Error> for RuntimeError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

pub type Result<T, E = RuntimeError> = std::result::Result<T, E>;

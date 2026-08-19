//! Errors that cross process boundaries.
//!
//! The daemon serialises these to the CLI, the desktop app and MCP, so the
//! variants are part of the public protocol and each carries enough context to
//! act on.
//!
//! Every variant uses **named fields**, never a newtype. Serde cannot place an
//! internal tag on a newtype variant wrapping a string, so `Io(String)` would
//! serialise to an error at runtime — and since that happens while writing the
//! response, the daemon would drop the connection instead of reporting
//! anything. `serialises_every_variant` below guards against reintroducing one.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RuntimeError {
    #[error("{kind} not found: {id}")]
    NotFound { kind: String, id: String },

    #[error("{message}")]
    AlreadyExists { message: String },

    #[error("port {port} is in use by {holder}")]
    PortConflict { port: u16, holder: String },

    #[error("no free port found in range {from}-{to}")]
    NoPortAvailable { from: u16, to: u16 },

    #[error("service '{service}' is already running (pid {pid})")]
    AlreadyRunning { service: String, pid: u32 },

    #[error("service '{service}' is not running")]
    NotRunning { service: String },

    #[error("'{service}' exited immediately{}: {detail}", .exit_code.map(|code| format!(" (code {code})")).unwrap_or_default())]
    StartFailed {
        service: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// The last thing it printed, which is normally the reason.
        detail: String,
    },

    #[error("refusing to terminate pid {pid}: {reason}")]
    NotPermitted { pid: u32, reason: String },

    #[error("{message}")]
    InvalidInput { message: String },

    #[error("not supported on this platform: {message}")]
    Unsupported { message: String },

    #[error("{message}")]
    Io { message: String },

    #[error("{message}")]
    Internal { message: String },
}

impl RuntimeError {
    pub fn not_found(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self::NotFound {
            kind: kind.into(),
            id: id.into(),
        }
    }

    pub fn already_exists(message: impl Into<String>) -> Self {
        Self::AlreadyExists {
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported {
            message: message.into(),
        }
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::Io {
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }
}

impl From<std::io::Error> for RuntimeError {
    fn from(value: std::io::Error) -> Self {
        Self::io(value.to_string())
    }
}

pub type Result<T, E = RuntimeError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant must survive a JSON round trip.
    ///
    /// A newtype variant compiles fine and only fails when serialised, which in
    /// production means a dropped connection carrying no diagnosis at all.
    #[test]
    fn serialises_every_variant() {
        let variants = [
            RuntimeError::not_found("service", "web"),
            RuntimeError::already_exists("a daemon is already listening"),
            RuntimeError::PortConflict {
                port: 3000,
                holder: "dossh/main/web".to_string(),
            },
            RuntimeError::NoPortAvailable {
                from: 3000,
                to: 3100,
            },
            RuntimeError::AlreadyRunning {
                service: "web".to_string(),
                pid: 42,
            },
            RuntimeError::NotRunning {
                service: "web".to_string(),
            },
            RuntimeError::StartFailed {
                service: "web".to_string(),
                exit_code: Some(1),
                detail: "EADDRINUSE".to_string(),
            },
            RuntimeError::NotPermitted {
                pid: 42,
                reason: "not started by the runtime".to_string(),
            },
            RuntimeError::invalid("bad selector"),
            RuntimeError::unsupported("edge panels"),
            RuntimeError::io("connection refused"),
            RuntimeError::internal("lock poisoned"),
        ];

        for error in variants {
            let encoded = serde_json::to_string(&error)
                .unwrap_or_else(|err| panic!("{error:?} cannot be serialised: {err}"));
            let decoded: RuntimeError = serde_json::from_str(&encoded)
                .unwrap_or_else(|err| panic!("{encoded} cannot be decoded: {err}"));
            assert_eq!(decoded, error);
            // The rendered message is what a human or an agent actually reads.
            assert!(!error.to_string().is_empty());
        }
    }
}

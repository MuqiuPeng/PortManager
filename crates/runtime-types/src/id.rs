//! Typed identifiers.
//!
//! Every entity id is a UUID v4 rendered as a plain string on the wire, so the
//! same identifiers travel unchanged through SQLite, IPC, the CLI and MCP.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

define_id!(
    /// Identifies a `Project` — one repository root as the user thinks of it.
    ProjectId
);
define_id!(
    /// Identifies a `Workspace` — one checkout (main clone or git worktree).
    WorkspaceId
);
define_id!(
    /// Identifies a `Service` — a declared runnable unit inside a workspace.
    ServiceId
);
define_id!(
    /// Identifies one `RuntimeInstance` — a single start of a service.
    InstanceId
);
define_id!(
    /// Identifies an `AgentSession` — one coding agent connected to the daemon.
    SessionId
);
define_id!(
    /// Identifies a `Stack` — a named sequence of steps in a workspace.
    StackId
);

//! The wire protocol.
//!
//! Newline-delimited JSON in both directions. Requests carry an id and are
//! answered by exactly one result or error frame; event frames arrive
//! unsolicited on the same connection after `Subscribe`.
//!
//! The protocol deliberately exposes *semantic* operations only. There is no
//! `exec`, no `kill_pid`, no `run_command` — a caller can restart a service but
//! cannot ask the daemon to run arbitrary code, which is what makes it safe to
//! put an MCP server in front of it.

use std::path::PathBuf;

use runtime_core::events::RuntimeEvent;
use runtime_types::{
    AgentSession, DaemonInfo, HealthReport, LogLine, PortOwner, PortReservation, PortStatus,
    ProjectView, RuntimeError, ServiceView, StartOutcome, Workspace,
};
use serde::{Deserialize, Serialize};

/// Bumped when a change would break an older client.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Ping,
    DaemonInfo,
    /// Ask the daemon to stop. Running services are stopped first.
    Shutdown,

    ListProjects,
    /// `selector` is an id, a name, or a path inside the project.
    GetProject {
        selector: String,
    },
    AddProject {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    RemoveProject {
        selector: String,
    },

    ListWorktrees {
        selector: String,
    },
    RegisterWorktree {
        selector: String,
        path: PathBuf,
    },

    /// All services, or only those of one project.
    ListServices {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    GetService {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        service: String,
    },

    StartService {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        service: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_conflict: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    StopService {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        service: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_seconds: Option<u64>,
    },
    RestartService {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        service: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },

    GetHealth {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        service: String,
    },
    WaitUntilHealthy {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        service: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_seconds: Option<u64>,
    },

    CheckPort {
        port: u16,
    },
    ListPorts,
    ReservePort {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        service: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_conflict: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_by: Option<String>,
    },
    ReleasePort {
        port: u16,
    },

    GetLogs {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        service: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_lines: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_seq: Option<u64>,
    },

    ListSessions,
    RegisterSession {
        provider: String,
        client: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
    },

    /// Turn this connection into an event stream. Further requests are still
    /// answered; events are interleaved.
    Subscribe,
}

/// An internally tagged enum cannot hold a sequence in a newtype variant —
/// serde has nowhere to put the tag — so every collection is carried in a
/// named `items` field rather than positionally.
//
// The variants differ in size by a few hundred bytes. Boxing them would add
// indirection to every match arm in the daemon and the CLI to save one memcpy
// per IPC call, which is not a trade worth making on a request/response type.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseBody {
    Pong { protocol_version: u32 },
    Info(DaemonInfo),
    Projects { items: Vec<ProjectView> },
    Project(ProjectView),
    Workspaces { items: Vec<Workspace> },
    Workspace(Workspace),
    Services { items: Vec<ServiceView> },
    Service(ServiceView),
    Started(StartOutcome),
    Health(HealthReport),
    Port(PortStatus),
    Ports { items: Vec<PortOwner> },
    Reservation(PortReservation),
    Logs { items: Vec<LogLine> },
    Sessions { items: Vec<AgentSession> },
    Session(AgentSession),
    Done { ok: bool },
}

/// One frame on the wire.
///
/// Explicitly tagged rather than untagged: an ambiguous frame that silently
/// deserialises as the wrong variant is far worse to debug than a slightly
/// more verbose envelope.
//
// See `ResponseBody` for why the size difference is left alone.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Frame {
    Request {
        id: u64,
        request: Request,
    },
    Response {
        id: u64,
        result: ResponseBody,
    },
    Error {
        id: u64,
        error: RuntimeError,
        /// The error rendered through its `Display` impl.
        ///
        /// Carried alongside the structured form so a client in another
        /// language does not have to reimplement the wording of every variant
        /// to show a usable message.
        message: String,
    },
    Event {
        event: RuntimeEvent,
    },
}

impl Frame {
    pub fn error(id: u64, error: RuntimeError) -> Self {
        Self::Error {
            id,
            message: error.to_string(),
            error,
        }
    }
}

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

use runtime_core::discover::Discovery;
use runtime_core::events::RuntimeEvent;
use runtime_types::{
    AdoptOutcome, AgentSession, DaemonInfo, Failure, Finding, HealthReport,
    LaunchObservation,
    LogLine,
    PortOwner, SupervisedView, StackView,
    PortReservation, PortStatus,
    ContainerView, ProjectConfig, ProjectView, RuntimeError, ServiceConfig, ServicePatch,
    ServiceView, StartOutcome, Workspace,
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
    /// Find projects without being told where they are.
    DiscoverProjects {
        /// Extra directory trees to walk. Discovery from running processes
        /// happens regardless.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<PathBuf>,
        /// Register everything found rather than only reporting it.
        #[serde(default)]
        adopt: bool,
    },
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
    /// Stop tracking a checkout. The directory is left alone.
    RemoveWorktree {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// The checkout's path, or its branch.
        checkout: String,
    },
    ListServices {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    GetService {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        service: String,
    },

    /// Correct a declared service. Detection is inference and gets it wrong.
    UpdateService {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        service: String,
        patch: ServicePatch,
    },
    /// Declare a service detection did not find.
    AddService {
        selector: String,
        name: String,
        #[serde(flatten)]
        config: ServiceConfig,
    },
    RemoveService {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        service: String,
    },
    /// The project's services as a committable `.runtime.json`.
    ExportConfig {
        selector: String,
    },

    /// Note a launch that is about to happen somewhere else.
    ///
    /// The command is not touched: this says what is about to run, so the
    /// runtime can recognise it when a port appears and, later, start it again
    /// from the command that actually ran rather than one inferred from it.
    RecordLaunch {
        command: String,
        cwd: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// Launches recorded recently, newest first.
    ListLaunches,

    /// Everything wrong with what is declared, looked for rather than waited on.
    Diagnose,

    /// Services that are not working, each with the part of its output that
    /// says why — without having to know which service to ask about.
    ListFailures {
        /// Lines of explanation to carry per service.
        #[serde(default)]
        detail_lines: usize,
    },

    /// Stacks declared in a checkout.
    ListStacks { selector: String },
    /// Declare or replace one.
    SetStack {
        selector: String,
        name: String,
        members: Vec<String>,
    },
    RemoveStack { selector: String, name: String },
    /// Bring up every step in order, each with its own dependencies.
    RunStack {
        selector: String,
        name: String,
        /// Clear whatever holds a port this stack declared, and carry on.
        ///
        /// Said by somebody who has been shown who holds it: a run stops at
        /// the conflict and reports the holder, and this is what the answer to
        /// that report sets. Nothing automatic sets it.
        #[serde(default)]
        free_ports: bool,
    },
    /// Stop everything it started, in the reverse of the order it started.
    StopStack { selector: String, name: String },

    /// Switch an entry another supervisor keeps.
    ///
    /// Only the reversible verbs: `start`, `stop`, `restart`. Deleting an
    /// entry is what stops it coming back at boot, and is not offered.
    ControlSupervised {
        name: String,
        action: String,
    },

    /// Declare whatever is on a port, so it can be started again later.
    ///
    /// Takes the command from the process, not from the project's scripts.
    AdoptPort {
        port: u16,
        /// Declare it even though another supervisor is keeping it alive.
        #[serde(default)]
        force: bool,
        /// Which stack to put it in. Its own, named after it, when unsaid.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stack: Option<String>,
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
    /// Stop what something else started and start it here instead.
    TakeOverService {
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

    /// Switch a container on or off.
    ///
    /// Named rather than by pid: a container id is stable, and `docker stop` is
    /// a graceful operation on a restartable object, which is why this is
    /// offered for containers the runtime did not create.
    ControlContainer {
        name: String,
        /// start | stop | restart
        action: String,
    },
    GetContainerLogs {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_lines: Option<usize>,
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

    /// Read a stored setting. Values are opaque to the daemon.
    GetSetting {
        key: String,
    },
    SetSetting {
        key: String,
        value: String,
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
    Discoveries { items: Vec<Discovery> },
    Project(ProjectView),
    Workspaces { items: Vec<Workspace> },
    Workspace(Workspace),
    Services { items: Vec<ServiceView> },
    Config(ProjectConfig),
    Container(ContainerView),
    Service(ServiceView),
    Started(StartOutcome),
    Health(HealthReport),
    Port(PortStatus),
    Ports { items: Vec<PortOwner> },
    Reservation(PortReservation),
    Logs { items: Vec<LogLine> },
    Setting { value: Option<String> },
    Launches { items: Vec<LaunchObservation> },
    Findings { items: Vec<Finding> },
    Failures { items: Vec<Failure> },
    Stacks { items: Vec<StackView> },
    StackRun { done: Vec<String> },
    Supervised(SupervisedView),
    Adopted(AdoptOutcome),
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

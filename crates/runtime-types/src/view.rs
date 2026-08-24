//! Read models.
//!
//! Every entry point (GUI, CLI, MCP) renders these aggregates, so all three
//! agree on what "the state of the machine" means without re-joining rows.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{ProjectId, ServiceId, WorkspaceId};
use crate::model::{
    ConflictPolicy, PortLeaseStatus, Project, Protocol, RuntimeInstance, Service, ServiceStatus,
    Stack, StartedBy, Workspace,
};

/// A service together with whatever is currently running for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceView {
    #[serde(flatten)]
    pub service: Service,
    pub status: ServiceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<RuntimeInstance>,
    /// The port actually bound, which may differ from `preferred_port`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// True when the runtime started what is running.
    ///
    /// A service found already listening on its port is reported as running —
    /// claiming otherwise while the port table shows it up is the kind of
    /// contradiction this tool exists to remove — but it cannot be stopped or
    /// restarted from here, because the runtime does not own it.
    #[serde(default)]
    pub managed: bool,
    /// Another supervisor keeping this alive, when one is.
    ///
    /// Only ever set for a service the runtime did not start. It is the answer
    /// to "why can I not stop this?" — a stop issued here would be undone in a
    /// second by whatever is watching, so the honest thing is to say who.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<String>,
    /// That supervisor's own name for this service.
    ///
    /// The link that makes the row actionable: knowing PM2 holds the port says
    /// only why the runtime cannot stop it, where knowing *which entry* is
    /// enough to stop it through PM2 — which is a stop that works, rather than
    /// one undone a second later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor_entry: Option<String>,
}

/// Something listening inside a project that no declared service accounts for.
///
/// Reported rather than guessed at: a process in Loom's directory on `:3001`
/// is certainly part of Loom, but deciding *which* declared service it is
/// would be a guess, and a wrong one is worse than an honest "unaccounted for".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalService {
    pub port: u16,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_line: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Another supervisor already keeping this alive, if one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<String>,
}

/// A container belonging to a project, running or not.
///
/// Kept apart from `ServiceView` because the runtime did not create it and does
/// not own its definition — compose does. What it can do is show it in the same
/// picture and switch it on or off, which is a named, graceful operation on a
/// restartable object rather than a signal aimed at a pid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerView {
    pub name: String,
    /// Compose service name, absent for a container started with `docker run`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    pub image: String,
    /// `running`, `exited`, `paused`, …
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl ContainerView {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceView {
    #[serde(flatten)]
    pub workspace: Workspace,
    pub services: Vec<ServiceView>,
    /// Ports live in this checkout that no declared service explains.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external: Vec<ExternalService>,
    /// Containers compose defines for this checkout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<ContainerView>,
    /// Entries another supervisor keeps in this checkout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supervised: Vec<SupervisedView>,
    /// Groups declared over these services. A member listed here is still
    /// present in `services`; a surface that shows groups is expected to show
    /// each service once, under its group.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stacks: Vec<StackView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectView {
    #[serde(flatten)]
    pub project: Project,
    pub workspaces: Vec<WorkspaceView>,
    /// Declared services that are up, however they were started.
    pub running_services: usize,
    pub total_services: usize,
    /// Live ports in this project that no declared service explains.
    #[serde(default)]
    pub external_services: usize,
}

/// What is actually listening on a port right now, resolved back to a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortOwner {
    pub port: u16,
    /// Defaulted so a client built against the TCP-only protocol still parses.
    #[serde(default)]
    pub protocol: Protocol,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_line: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// Another supervisor already keeping this alive: `pm2`, `systemd`.
    ///
    /// Reported, never acted on. Something that restarts a service on its own
    /// will undo a stop issued here, and taking it over means removing it from
    /// wherever it is declared — which usually changes what starts at boot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<ServiceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by: Option<StartedBy>,
    /// Container publishing this port, when it is not a plain process.
    ///
    /// Every container on a machine publishes through one Docker process, so
    /// the pid alone identifies nothing; this is what separates them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// True when this process was launched by the runtime itself. Only these
    /// are ever eligible for automatic termination.
    pub managed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortStatus {
    pub port: u16,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<PortOwner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_status: Option<PortLeaseStatus>,
    /// Populated when the port is taken: the next port the runtime would use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_port: Option<u16>,
}

/// Outcome of asking the runtime for a port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortReservation {
    pub port: u16,
    pub preferred_port: Option<u16>,
    /// True when `port != preferred_port`.
    pub reallocated: bool,
    pub policy: ConflictPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<PortOwner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthReport {
    pub service_id: ServiceId,
    pub status: ServiceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_port: Option<u16>,
}

/// Result of `start_service`, which may adopt an already-running instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartOutcome {
    pub service: ServiceView,
    /// True when an existing healthy instance was reused instead of started.
    pub reused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation: Option<PortReservation>,
    /// Something about this start the caller would have wanted to know first.
    ///
    /// Not an error and not a refusal: running a development server is a normal
    /// thing to want. But when it rewrites a build another service is serving
    /// from, nothing fails at the time — the failure arrives at that service's
    /// next restart, hours later, pointing at the wrong thing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub version: String,
    pub pid: u32,
    pub socket_path: PathBuf,
    pub database_path: PathBuf,
    pub platform: String,
    pub uptime_seconds: u64,
}

/// A service launch the runtime was told about before it happened.
///
/// The runtime records these rather than intercepting them: the command runs
/// exactly as it was typed, and this is only the note that it was going to.
/// What makes the note worth keeping is that it holds the one thing a running
/// process cannot be asked for — the command line that would start it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchObservation {
    pub id: String,
    /// Exactly as given. Never a script name inferred from it: a project whose
    /// `dev` and `start` write to the same build directory is broken by
    /// restarting it under the wrong one.
    pub command: String,
    pub cwd: PathBuf,
    pub source: StartedBy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub state: LaunchState,
    /// Set once a port turned up that this launch explains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<ServiceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchState {
    /// Recorded; nothing has been seen to listen yet.
    Pending,
    /// A port appeared that this launch explains.
    Bound,
}

/// What adopting a port produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptOutcome {
    pub service: ServiceView,
    /// Where the command came from. Never the project's scripts.
    pub command_source: CommandSource,
    /// False when the service was already declared and nothing changed.
    pub declared: bool,
    /// The command that was written down before, when adopting replaced it.
    ///
    /// Worth saying out loud: a definition silently rewritten underneath
    /// somebody is worse than one that was wrong openly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_command: Option<String>,
    /// Set when something else is still keeping it alive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<String>,
    /// The stack the adopted service was put in.
    ///
    /// Adopting exists so a running thing can be started again later, and a
    /// service in no stack cannot be started by name — so adopting one and
    /// leaving it outside every stack would undo its own purpose one step
    /// after it succeeded. Running `adopt` is somebody saying they want this
    /// managed, which is the declaration the rule asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSource {
    /// A launch recorded before it ran — what somebody actually asked for.
    Recorded,
    /// The process's own argv: what the shell and the package manager turned
    /// that request into.
    ProcessArgv,
    /// The supervisor that runs it, which holds what it will run next time —
    /// and which is the only source that survives a process renaming itself.
    Supervisor,
}

/// A service another supervisor keeps, that the runtime can switch.
///
/// Reported separately from declared services for the same reason containers
/// are: the runtime did not decide what this is and does not decide whether it
/// comes back after a reboot. What it can do is start and stop it, using the
/// named operations the supervisor offers itself, which leaves that
/// supervisor's registry exactly as it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisedView {
    /// The supervisor's own name for it.
    pub name: String,
    /// Which supervisor: `pm2`, `systemd`.
    pub supervisor: String,
    /// `online`, `stopped`, `errored`, …
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub command: String,
    pub restarts: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Set when restarting this would fail, with the reason.
    ///
    /// A production entry whose build has been replaced by a dev server keeps
    /// serving until something restarts it, and then cannot start at all. The
    /// runtime knows both halves — that it runs in production mode, and that
    /// the build directory has no production build in it — so it says so
    /// before the restart rather than after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_warning: Option<String>,
}

/// Something wrong with what is declared, found without being asked.
///
/// Every one of these is a problem that stays quiet until the moment it is
/// expensive: a dependency naming a service that does not exist fails halfway
/// through a start, having already brought up everything before it; a build
/// two services share breaks the one that is not looking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Where it is, as a person would say it: `Loom/api`.
    pub subject: String,
    pub message: String,
    /// True when it will fail rather than merely might.
    pub certain: bool,
}

/// A service that is not working, with the part of its output that says why.
///
/// The unit a person debugging actually wants. Finding this out otherwise means
/// knowing which service broke, then reading its whole log to find the few
/// lines that matter — two steps that both assume you already know something
/// about a failure you have not seen yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub service_id: ServiceId,
    /// `Loom/api`, as a person would say it.
    pub subject: String,
    pub status: ServiceStatus,
    /// When it stopped, or when it was last seen trying.
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The last thing it said, preferring what it said on stderr.
    ///
    /// A service that fails on startup usually explains itself in its final
    /// lines and then says nothing else, so the tail is the message — but only
    /// if the quieter stream is not drowned out by an ordinary access log.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
}

/// A stack with what its members are actually doing.
///
/// The point of declaring one is that a database, an API and a front end are
/// one thing to the person using them. Listing the three as peers, each with
/// its own button, makes the reader reassemble that every time they look — and
/// leaves them to remember the order. This is the group as a unit: one state,
/// one thing to start, one thing to stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackView {
    #[serde(flatten)]
    pub stack: Stack,
    /// Its members, in the order they start.
    pub services: Vec<ServiceView>,
    /// How many of them are up.
    pub running: usize,
    /// Steps naming a service that no longer exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// The group as a graph: what waits for what, and what can go at once.
    ///
    /// Derived from the members' own `depends_on` rather than stored beside
    /// it. A service needing a database is a fact about the service, true in
    /// every group it appears in; keeping a second copy per group is how the
    /// two come to disagree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flow: Vec<FlowNode>,
}

/// One service in a group, placed by what it waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowNode {
    pub name: String,
    /// Absent for a step naming a service that is no longer declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<ServiceId>,
    /// Members it waits for. Dependencies outside the group are not shown:
    /// they are brought up too, but they are not part of what was declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
    /// How many waits deep it is. Everything on one level can start at once.
    pub level: usize,
    pub status: ServiceStatus,
    #[serde(default)]
    pub one_shot: bool,
}

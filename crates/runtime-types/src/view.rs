//! Read models.
//!
//! Every entry point (GUI, CLI, MCP) renders these aggregates, so all three
//! agree on what "the state of the machine" means without re-joining rows.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{ProjectId, ServiceId, WorkspaceId};
use crate::model::{
    ConflictPolicy, PortLeaseStatus, Project, RuntimeInstance, Service, ServiceStatus, StartedBy,
    Workspace,
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

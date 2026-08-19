//! Read models.
//!
//! Every entry point (GUI, CLI, MCP) renders these aggregates, so all three
//! agree on what "the state of the machine" means without re-joining rows.

use std::path::PathBuf;

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceView {
    #[serde(flatten)]
    pub workspace: Workspace,
    pub services: Vec<ServiceView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectView {
    #[serde(flatten)]
    pub project: Project,
    pub workspaces: Vec<WorkspaceView>,
    pub running_services: usize,
    pub total_services: usize,
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

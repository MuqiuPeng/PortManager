//! Domain model.
//!
//! The object graph is `Project -> Workspace -> Service -> RuntimeInstance`.
//! A port is never a top-level object: it is a lease held by a service.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{InstanceId, ProjectId, ServiceId, SessionId, WorkspaceId};

/// A repository as the user thinks of it, independent of which checkout is open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub root_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One checkout of a project: the primary clone or a git worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub project_id: ProjectId,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    /// True when this checkout is a linked worktree rather than the main one.
    pub worktree: bool,
    /// Stable slot index used to derive worktree port offsets (main == 0).
    pub port_offset: u16,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    Web,
    Api,
    Worker,
    Database,
    Cache,
    Container,
    #[default]
    Custom,
}

/// A declared runnable unit. Declaration is durable; running state is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    pub id: ServiceId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    #[serde(default)]
    pub service_type: ServiceType,
    pub command: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheck>,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default)]
    pub conflict_policy: ConflictPolicy,
}

/// Who asked for a service to start. Drives ownership display and kill safety.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StartedBy {
    Manual,
    Desktop,
    Cli,
    ClaudeCode,
    Codex,
    Cursor,
    #[default]
    Unknown,
}

impl StartedBy {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "manual" => Self::Manual,
            "desktop" => Self::Desktop,
            "cli" => Self::Cli,
            "claude-code" | "claude" => Self::ClaudeCode,
            "codex" => Self::Codex,
            "cursor" => Self::Cursor,
            _ => Self::Unknown,
        }
    }
}

/// Lifecycle state. Deliberately finer-grained than running/stopped so that
/// "the process exists" and "the service answers" stay distinguishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceStatus {
    Starting,
    Healthy,
    Unhealthy,
    Stopping,
    Stopped,
    Failed,
    Unknown,
}

impl ServiceStatus {
    /// True when the runtime believes a process should currently exist.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Healthy | Self::Unhealthy | Self::Stopping
        )
    }
}

/// One start of a service. Identity is `(pid, process_start_time)` so a
/// recycled pid can never be mistaken for the process we launched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeInstance {
    pub id: InstanceId,
    pub service_id: ServiceId,
    pub pid: u32,
    /// Process start time in milliseconds since the Unix epoch.
    pub process_start_time: i64,
    pub status: ServiceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub started_by: StartedBy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session: Option<SessionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortLeaseStatus {
    Reserved,
    Active,
    Released,
    Conflicted,
}

/// A claim on a port held by a service, not merely an observation of a socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortLease {
    pub port: u16,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub service_id: ServiceId,
    /// True when this is the port the service actually asked for.
    pub preferred: bool,
    pub status: PortLeaseStatus,
    #[serde(default)]
    pub owner: StartedBy,
    pub created_at: DateTime<Utc>,
    /// Reservations expire so a crashed agent cannot hold a port forever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// What to do when the preferred port is already taken.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictPolicy {
    /// Adopt the existing instance if it is the same service.
    Reuse,
    /// Scan upward for the next free port.
    #[default]
    AllocateNext,
    /// Refuse to start.
    Fail,
    /// Return the conflict and let the caller decide.
    Ask,
    /// Terminate the current holder. Only ever honoured for owned processes.
    KillExisting,
}

impl ConflictPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "reuse" => Some(Self::Reuse),
            "allocate-next" | "next" => Some(Self::AllocateNext),
            "fail" => Some(Self::Fail),
            "ask" => Some(Self::Ask),
            "kill-existing" | "kill" => Some(Self::KillExisting),
            _ => None,
        }
    }
}

/// Changes to a declared service.
///
/// Every field optional, because inference gets things wrong in ones and twos —
/// a port, a command — and correcting one should not mean restating the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServicePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_type: Option<ServiceType>,
    /// `Some(None)` clears the port; `None` leaves it alone.
    #[serde(
        default,
        deserialize_with = "present_as_some",
        skip_serializing_if = "Option::is_none"
    )]
    pub preferred_port: Option<Option<u16>>,
    #[serde(
        default,
        deserialize_with = "present_as_some",
        skip_serializing_if = "Option::is_none"
    )]
    pub health_check: Option<Option<HealthCheck>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_start: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_policy: Option<ConflictPolicy>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// Distinguish an absent field from one explicitly set to `null`.
///
/// Serde folds both into `None` by default, which silently turns "clear this
/// port" into "leave it alone" — an option that appears to work and does
/// nothing. Running the inner deserializer only when the key is present makes
/// `null` mean what it says.
fn present_as_some<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

impl ServicePatch {
    /// Apply the changes, leaving untouched fields as they were.
    pub fn apply(self, service: &mut Service) {
        if let Some(name) = self.name {
            service.name = name;
        }
        if let Some(command) = self.command {
            service.command = command;
        }
        if let Some(cwd) = self.cwd {
            service.cwd = cwd;
        }
        if let Some(service_type) = self.service_type {
            service.service_type = service_type;
        }
        if let Some(port) = self.preferred_port {
            service.preferred_port = port;
        }
        if let Some(health) = self.health_check {
            service.health_check = health;
        }
        if let Some(auto_start) = self.auto_start {
            service.auto_start = auto_start;
        }
        if let Some(policy) = self.conflict_policy {
            service.conflict_policy = policy;
        }
        // Merged rather than replaced: setting one variable should not drop
        // the others.
        service.env.extend(self.env);
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum HealthCheck {
    /// A TCP connect to the service's port succeeds.
    Tcp {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
    },
    /// An HTTP GET returns one of `expect_status`.
    Http {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(default = "default_expect_status")]
        expect_status: Vec<u16>,
    },
    /// The process is alive. The weakest signal, and the default.
    Process,
}

fn default_expect_status() -> Vec<u16> {
    vec![200]
}

/// A connected coding agent. One session per MCP client connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: SessionId,
    /// e.g. `anthropic`, `openai`.
    pub provider: String,
    /// e.g. `claude-code`, `codex`, `cursor`.
    pub client: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    pub started_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogStream {
    Stdout,
    Stderr,
    /// Emitted by the runtime itself (start, exit, health transitions).
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    /// Monotonic per-service sequence number, usable as a cursor.
    pub seq: u64,
    pub service_id: ServiceId,
    pub stream: LogStream,
    pub timestamp: DateTime<Utc>,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absent, null and a value must mean three different things.
    ///
    /// Serde folds an explicit `null` into the same `None` as an absent key,
    /// which turns "clear this port" into a silent no-op — an option that
    /// appears to work and does nothing.
    #[test]
    fn a_patch_distinguishes_absent_from_null() {
        let absent: ServicePatch = serde_json::from_str(r#"{"command":"pnpm dev"}"#).unwrap();
        assert_eq!(absent.preferred_port, None, "absent means leave it alone");

        let cleared: ServicePatch = serde_json::from_str(r#"{"preferred_port":null}"#).unwrap();
        assert_eq!(cleared.preferred_port, Some(None), "null means clear it");

        let set: ServicePatch = serde_json::from_str(r#"{"preferred_port":3007}"#).unwrap();
        assert_eq!(set.preferred_port, Some(Some(3007)));
    }

    #[test]
    fn applying_a_patch_leaves_untouched_fields_alone() {
        let mut service = Service {
            id: ServiceId::from("svc"),
            workspace_id: WorkspaceId::from("ws"),
            name: "web".to_string(),
            service_type: ServiceType::Web,
            command: "pnpm dev".to_string(),
            cwd: PathBuf::from("/repo"),
            env: BTreeMap::from([("A".to_string(), "1".to_string())]),
            preferred_port: Some(3000),
            health_check: None,
            auto_start: false,
            conflict_policy: ConflictPolicy::AllocateNext,
        };

        ServicePatch {
            preferred_port: Some(Some(3007)),
            env: BTreeMap::from([("B".to_string(), "2".to_string())]),
            ..Default::default()
        }
        .apply(&mut service);

        assert_eq!(service.preferred_port, Some(3007));
        assert_eq!(service.command, "pnpm dev");
        // Environment is merged: setting one variable must not drop the others.
        assert_eq!(service.env.get("A").map(String::as_str), Some("1"));
        assert_eq!(service.env.get("B").map(String::as_str), Some("2"));
    }
}

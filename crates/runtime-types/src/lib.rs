//! Shared domain types for the local runtime manager.
//!
//! This crate has no OS dependencies and no I/O: it is the vocabulary that the
//! core, the daemon, the CLI, the desktop app and the MCP server all speak.

pub mod config;
pub mod error;
pub mod id;
pub mod model;
pub mod view;

pub use config::{ProjectConfig, ServiceConfig, CONFIG_FILE_NAME};
pub use error::{Result, RuntimeError};
pub use id::{InstanceId, ProjectId, ServiceId, SessionId, TaskId, WorkspaceId};
pub use model::{
    AgentSession, ConflictPolicy, HealthCheck, LogLine, LogStream, PortLease, PortLeaseStatus, Project, RuntimeInstance, Service, ServicePatch, ServiceStatus, ServiceType, StartedBy, Task, Workspace,
};
pub use view::{
    AdoptOutcome, CommandSource, ContainerView, DaemonInfo, ExternalService, Failure, Finding, HealthReport, LaunchObservation, LaunchState, PortOwner, PortReservation, PortStatus, ProjectView, ServiceView, StartOutcome, SupervisedView, WorkspaceView,
};

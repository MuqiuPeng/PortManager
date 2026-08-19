//! The runtime event bus.
//!
//! One broadcast channel that every entry point subscribes to, so a service
//! started from the CLI shows up in the GUI without polling.

use runtime_types::{LogLine, ProjectId, ServiceId, ServiceStatus};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RuntimeEvent {
    ProjectAdded {
        project_id: ProjectId,
        name: String,
    },
    ProjectRemoved {
        project_id: ProjectId,
    },
    ServiceStatusChanged {
        service_id: ServiceId,
        status: ServiceStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
    },
    ServiceExited {
        service_id: ServiceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    PortLeaseChanged {
        port: u16,
        service_id: ServiceId,
    },
    Log(LogLine),
}

/// Capacity is generous because log lines share the channel; slow subscribers
/// lag rather than block the writer.
const CHANNEL_CAPACITY: usize = 1024;

#[derive(Debug, Clone)]
pub struct EventBus {
    sender: broadcast::Sender<RuntimeEvent>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.sender.subscribe()
    }

    /// Publishing never fails: with no subscribers the event is simply dropped.
    pub fn publish(&self, event: RuntimeEvent) {
        let _ = self.sender.send(event);
    }
}

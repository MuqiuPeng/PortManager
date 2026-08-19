//! Tracking of processes this runtime started.
//!
//! The daemon holds a handle for every service it launched so it can stop the
//! log pumps and the exit waiter deterministically on shutdown, instead of
//! leaving tasks reading from pipes nobody will drain.

use std::collections::HashMap;
use std::sync::Mutex;

use runtime_adapter::ProcessIdentity;
use runtime_types::{InstanceId, Result, RuntimeError, ServiceId};
use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct RunningProcess {
    pub instance_id: InstanceId,
    pub identity: ProcessIdentity,
    pub port: Option<u16>,
    /// Log pumps and the exit waiter.
    pub tasks: Vec<JoinHandle<()>>,
}

impl RunningProcess {
    fn abort(&self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[derive(Debug, Default)]
pub struct Supervisor {
    running: Mutex<HashMap<ServiceId, RunningProcess>>,
}

impl Supervisor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, service_id: ServiceId, process: RunningProcess) -> Result<()> {
        let mut guard = self.lock()?;
        if let Some(previous) = guard.insert(service_id, process) {
            previous.abort();
        }
        Ok(())
    }

    pub fn identity(&self, service_id: &ServiceId) -> Result<Option<ProcessIdentity>> {
        Ok(self.lock()?.get(service_id).map(|p| p.identity))
    }

    pub fn port(&self, service_id: &ServiceId) -> Result<Option<u16>> {
        Ok(self.lock()?.get(service_id).and_then(|p| p.port))
    }

    pub fn set_port(&self, service_id: &ServiceId, port: Option<u16>) -> Result<()> {
        if let Some(process) = self.lock()?.get_mut(service_id) {
            process.port = port;
        }
        Ok(())
    }

    pub fn remove(&self, service_id: &ServiceId) -> Result<Option<RunningProcess>> {
        let removed = self.lock()?.remove(service_id);
        if let Some(process) = &removed {
            process.abort();
        }
        Ok(removed)
    }

    pub fn service_ids(&self) -> Result<Vec<ServiceId>> {
        Ok(self.lock()?.keys().cloned().collect())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<ServiceId, RunningProcess>>> {
        self.running
            .lock()
            .map_err(|_| RuntimeError::internal("supervisor lock poisoned"))
    }
}

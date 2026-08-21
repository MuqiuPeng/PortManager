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

    /// Stop tracking a service whose process has ended.
    ///
    /// Deliberately does **not** abort its tasks. The log pumps end on their
    /// own when the pipes close, and cutting them short discards whatever the
    /// process printed on its way out — which is the one thing worth having
    /// when something dies a second after starting.
    pub fn finish(&self, service_id: &ServiceId) -> Result<Option<RunningProcess>> {
        Ok(self.lock()?.remove(service_id))
    }

    /// Stop what `finish` handed back, once it has had time to drain.
    ///
    /// `finish` takes an entry out without stopping it, so the last lines a
    /// service printed still arrive. That leaves the tasks unreachable through
    /// the map, and they are still reading the capture file the next run will
    /// append to — so the caller has to be able to stop them afterwards, and
    /// this is where that lives rather than in a private field of the caller's.
    pub fn stop(process: RunningProcess) {
        process.abort();
    }

    /// Stop tracking a service and abandon its tasks.
    ///
    /// For shutdown, where waiting on pipes that may never close is worse than
    /// losing the tail of a log.
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

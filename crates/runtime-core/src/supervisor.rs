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
    /// Runs that have ended but whose readers are still being drained.
    ///
    /// Kept here rather than handed to the caller so that starting the same
    /// service again can find them. A drain deliberately outlives the process
    /// by a moment; a second run beginning inside that moment would otherwise
    /// share its capture file with a reader from the run before, and every
    /// line would be logged twice.
    draining: Mutex<HashMap<ServiceId, RunningProcess>>,
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

    /// Hold a finished run's readers while they drain.
    pub fn park(&self, service_id: ServiceId, process: RunningProcess) -> Result<()> {
        if let Some(previous) = self.draining()?.insert(service_id, process) {
            previous.abort();
        }
        Ok(())
    }

    /// Stop a parked run's readers, whether or not it has finished draining.
    ///
    /// Called when the drain window is up, and again before the same service
    /// starts again: whatever the old run still had to say, it has had until
    /// the moment its replacement begins to say it, and it must not be reading
    /// when the new run starts writing.
    pub fn stop_parked(&self, service_id: &ServiceId) -> Result<()> {
        if let Some(process) = self.draining()?.remove(service_id) {
            process.abort();
        }
        Ok(())
    }

    /// Stop tracking a service and abandon its tasks.
    ///
    /// For shutdown, where waiting on pipes that may never close is worse than
    /// losing the tail of a log.
    pub fn remove(&self, service_id: &ServiceId) -> Result<Option<RunningProcess>> {
        self.stop_parked(service_id)?;
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

    fn draining(&self) -> Result<std::sync::MutexGuard<'_, HashMap<ServiceId, RunningProcess>>> {
        self.draining
            .lock()
            .map_err(|_| RuntimeError::internal("supervisor lock poisoned"))
    }
}

//! Job Objects, so a service dies with every descendant it created.
//!
//! `taskkill /T` walks a parent chain, which is exactly what a detached
//! grandchild has already left: the intermediate `cmd.exe` exits, the
//! grandchild reparents, and the walk never reaches it. A Job Object is
//! membership rather than ancestry — a process joins when it is assigned and
//! every process it spawns joins with it — so `TerminateJobObject` reaches the
//! whole tree however it has been rearranged.
//!
//! The job is *not* created with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. That
//! would tie every service's life to the daemon holding the handle, so a daemon
//! restart would take down everything it had started — the opposite of what
//! `Runtime::reconcile` exists to handle. Termination here is explicit.

use std::collections::HashMap;
use std::sync::Mutex;

use runtime_types::{Result, RuntimeError};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// An owned job handle.
///
/// Wrapped so the registry can be `Send + Sync`: a `HANDLE` is a bare pointer,
/// but a job handle is only ever used through the registry's mutex.
#[derive(Debug)]
struct Job(HANDLE);

// SAFETY: the handle is owned by the registry, only touched under its mutex,
// and closed exactly once in `Drop`.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateJobObjectW` and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// The job each service the runtime started belongs to, keyed by its pid.
#[derive(Debug, Default)]
pub struct JobRegistry {
    jobs: Mutex<HashMap<u32, Job>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put a freshly started process into its own job.
    ///
    /// Best-effort by design: a process that cannot be assigned — already in a
    /// job that forbids nesting, or gone before we got to it — still works,
    /// it just falls back to the process-tree walk when stopped.
    pub fn confine(&self, pid: u32) -> Result<()> {
        // SAFETY: a null name and null attributes create an unnamed job.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(RuntimeError::io(format!(
                "could not create a job object for pid {pid}"
            )));
        }
        let job = Job(job);

        // SAFETY: `pid` names a process that has just been spawned.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() || process == INVALID_HANDLE_VALUE {
            return Err(RuntimeError::io(format!(
                "could not open pid {pid} to confine it"
            )));
        }
        // SAFETY: both handles are valid and owned here.
        let assigned = unsafe { AssignProcessToJobObject(job.0, process) };
        // SAFETY: closing our reference to the process; the job keeps its own.
        unsafe { CloseHandle(process) };
        if assigned == 0 {
            return Err(RuntimeError::io(format!(
                "could not assign pid {pid} to a job object"
            )));
        }

        let mut guard = self.lock()?;
        guard.insert(pid, job);
        Ok(())
    }

    /// Kill every process in `pid`'s job.
    ///
    /// `Ok(false)` means there is no job for this pid — a service started
    /// before the daemon restarted, say — and the caller should fall back.
    pub fn terminate(&self, pid: u32) -> Result<bool> {
        let job = {
            let mut guard = self.lock()?;
            guard.remove(&pid)
        };
        let Some(job) = job else {
            return Ok(false);
        };
        // SAFETY: the handle is valid until `job` is dropped below.
        let terminated = unsafe { TerminateJobObject(job.0, 1) };
        if terminated == 0 {
            return Err(RuntimeError::io(format!(
                "could not terminate the job object for pid {pid}"
            )));
        }
        Ok(true)
    }

    /// Drop the job for a process that ended on its own.
    pub fn forget(&self, pid: u32) {
        if let Ok(mut guard) = self.jobs.lock() {
            guard.remove(&pid);
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<u32, Job>>> {
        self.jobs
            .lock()
            .map_err(|_| RuntimeError::internal("the job registry mutex was poisoned"))
    }
}

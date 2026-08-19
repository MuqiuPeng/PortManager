//! Windows adapter.
//!
//! # Status
//!
//! Sockets are read natively; process enumeration still delegates to the
//! portable `sysinfo` implementation. On top of that sit the Windows-specific
//! behaviours the runtime cannot do without: creating each service in its own
//! process group, and terminating the whole process tree.
//!
//! Everything marked `TODO(windows)` is a deliberate handoff point where a
//! native Win32 implementation should replace the baseline. The trait
//! signatures will not change, so that work is local to this crate.
//!
//! # State of the native work
//!
//! | Concern | Implementation |
//! |---|---|
//! | Port table | **`GetExtendedTcpTable` / `GetExtendedUdpTable`**, TCP and UDP, v4 and v6 |
//! | Tree termination | `taskkill /T /F` — to become a Job Object with `KILL_ON_JOB_CLOSE` |
//! | Graceful stop | forced — to become `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)` |
//! | Process metadata | `sysinfo` — `Toolhelp32Snapshot` only if it proves too slow |
//!
//! Parsing `netstat -ano` output is explicitly out of scope: the plan calls for
//! the API path in the shipped version, and the output is localised.

#![cfg(windows)]

mod jobs;
mod port;
mod process;
mod spawn;

use std::sync::Arc;

use runtime_adapter::{PlatformAdapter, PortProvider, ProcessProvider, SpawnProvider};

pub use jobs::JobRegistry;
pub use port::WindowsPortProvider;
pub use process::WindowsProcessProvider;
pub use spawn::WindowsSpawnProvider;

#[derive(Debug)]
pub struct WindowsAdapter {
    process: WindowsProcessProvider,
    port: WindowsPortProvider,
    spawn: WindowsSpawnProvider,
}

impl Default for WindowsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsAdapter {
    pub fn new() -> Self {
        // One registry, shared: the spawn side fills it and the process side
        // terminates from it.
        let jobs = Arc::new(JobRegistry::new());
        Self {
            process: WindowsProcessProvider::with_jobs(Arc::clone(&jobs)),
            port: WindowsPortProvider::new(),
            spawn: WindowsSpawnProvider::with_jobs(jobs),
        }
    }
}

impl PlatformAdapter for WindowsAdapter {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn process(&self) -> &dyn ProcessProvider {
        &self.process
    }

    fn port(&self) -> &dyn PortProvider {
        &self.port
    }

    fn spawn(&self) -> &dyn SpawnProvider {
        &self.spawn
    }
}

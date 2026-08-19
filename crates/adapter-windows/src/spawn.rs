//! Launching services on Windows.

use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::Arc;

use crate::jobs::JobRegistry;

use runtime_adapter::spawn::SpawnProvider;
use runtime_types::Result;

/// The child starts a new process group, which is the precondition for
/// `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)` to reach it and nothing else.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// No console window flashes when the daemon starts a service.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Default)]
pub struct WindowsSpawnProvider {
    jobs: Arc<JobRegistry>,
}

impl WindowsSpawnProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Share the job registry with the process provider, which is what has to
    /// terminate them.
    pub fn with_jobs(jobs: Arc<JobRegistry>) -> Self {
        Self { jobs }
    }
}

impl SpawnProvider for WindowsSpawnProvider {
    fn shell(&self) -> (String, Vec<String>) {
        // cmd.exe is the safe default: it is always present, and it resolves
        // the `.cmd` shims npm and pnpm install on Windows.
        //
        // TODO(windows): make this configurable so users on PowerShell profiles
        // get the same environment they see in their terminal.
        ("cmd.exe".to_string(), vec!["/C".to_string()])
    }

    fn prepare(&self, command: &mut Command) -> Result<()> {
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        Ok(())
    }

    /// Assigned after the spawn rather than prepared before it: a process can
    /// only join a job once it exists, and the pid is how we reach it.
    fn confine(&self, pid: u32) -> Result<()> {
        self.jobs.confine(pid)
    }

    fn release(&self, pid: u32) {
        self.jobs.forget(pid);
    }
}

//! Process inspection and termination on Windows.

use std::process::{Command, Stdio};
use std::sync::Arc;

use runtime_adapter::generic::GenericProcessProvider;
use runtime_adapter::process::{ProcessIdentity, ProcessInfo, ProcessProvider, TerminationMode};
use runtime_types::{Result, RuntimeError};

use crate::console::Console;
use crate::jobs::JobRegistry;
use crate::spawn::CREATE_NO_WINDOW;

#[derive(Debug, Default)]
pub struct WindowsProcessProvider {
    generic: GenericProcessProvider,
    jobs: Arc<JobRegistry>,
    console: Console,
}

impl WindowsProcessProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Share the job registry with the spawn provider, which fills it.
    pub fn with_jobs(jobs: Arc<JobRegistry>) -> Self {
        Self {
            generic: GenericProcessProvider,
            jobs,
            console: Console::new(),
        }
    }

    /// Terminate a pid and every descendant via `taskkill /T`.
    ///
    /// The fallback for anything with no job: a service started before the
    /// daemon last restarted, or one the kernel refused to confine. It walks
    /// the parent chain, so a descendant that has re-parented itself escapes —
    /// which is precisely why the job is tried first.
    fn taskkill(pid: u32) -> Result<bool> {
        let output = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|err| RuntimeError::io(format!("taskkill failed to run: {err}")))?;

        if output.status.success() {
            return Ok(true);
        }
        // Exit code 128 is "no such process", which is success for our purposes.
        if output.status.code() == Some(128) {
            return Ok(false);
        }
        Err(RuntimeError::io(format!(
            "taskkill /PID {pid} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

use std::os::windows::process::CommandExt;

impl ProcessProvider for WindowsProcessProvider {
    fn list_processes(&self) -> Result<Vec<ProcessInfo>> {
        // TODO(windows): Toolhelp32Snapshot gives the same data without the
        // full-system refresh sysinfo performs on every call.
        self.generic.list_processes()
    }

    fn process_info(&self, pid: u32) -> Result<Option<ProcessInfo>> {
        self.generic.process_info(pid)
    }

    fn terminate_tree(&self, identity: &ProcessIdentity, mode: TerminationMode) -> Result<bool> {
        // Re-verify identity immediately before signalling: Windows recycles
        // pids aggressively, so a stale record must never reach taskkill.
        let Some(current) = self.process_info(identity.pid)? else {
            return Ok(false);
        };
        if !current.identity().matches(identity) {
            return Ok(false);
        }

        // Ask first. The caller polls `is_alive` and escalates to `Forceful`
        // when its grace period runs out, so this only has to deliver the
        // request — not to wait for an answer.
        if matches!(mode, TerminationMode::Graceful) && self.console.interrupt(identity.pid)? {
            return Ok(true);
        }
        // An undeliverable request is not a granted one. Falling through rather
        // than reporting success keeps the caller from waiting out a grace
        // period the service never heard the start of.

        // Membership beats ancestry: the job holds every process the service
        // spawned, including any that re-parented away from it.
        if self.jobs.terminate(identity.pid)? {
            return Ok(true);
        }
        Self::taskkill(identity.pid)
    }
}

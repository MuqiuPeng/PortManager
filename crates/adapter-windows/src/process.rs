//! Process inspection and termination on Windows.

use std::process::{Command, Stdio};

use runtime_adapter::generic::GenericProcessProvider;
use runtime_adapter::process::{ProcessIdentity, ProcessInfo, ProcessProvider, TerminationMode};
use runtime_types::{Result, RuntimeError};

use crate::spawn::CREATE_NO_WINDOW;

#[derive(Debug, Default)]
pub struct WindowsProcessProvider {
    generic: GenericProcessProvider,
}

impl WindowsProcessProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Terminate a pid and every descendant via `taskkill /T`.
    ///
    /// This is the baseline tree kill. It is correct but coarse: it always
    /// forces, and it shells out.
    ///
    /// TODO(windows): assign each spawned service to a Job Object with
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and close the handle here, so the
    /// kernel guarantees no descendant survives even if it re-parents.
    fn taskkill(pid: u32) -> Result<bool> {
        let output = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|err| RuntimeError::Io(format!("taskkill failed to run: {err}")))?;

        if output.status.success() {
            return Ok(true);
        }
        // Exit code 128 is "no such process", which is success for our purposes.
        if output.status.code() == Some(128) {
            return Ok(false);
        }
        Err(RuntimeError::Io(format!(
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

        // TODO(windows): for TerminationMode::Graceful, send
        // GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) first and only fall
        // through to taskkill after the caller's grace period expires. The
        // process group created in WindowsSpawnProvider exists for this.
        let _ = mode;
        Self::taskkill(identity.pid)
    }
}

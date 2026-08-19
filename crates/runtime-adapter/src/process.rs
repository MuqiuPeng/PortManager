//! Process inspection and termination.

use std::path::PathBuf;

use runtime_types::Result;
use serde::{Deserialize, Serialize};

/// A snapshot of one OS process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_pid: Option<u32>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_line: Vec<String>,
    /// Start time in milliseconds since the Unix epoch.
    pub start_time_ms: i64,
}

impl ProcessInfo {
    pub fn identity(&self) -> ProcessIdentity {
        ProcessIdentity {
            pid: self.pid,
            start_time_ms: self.start_time_ms,
        }
    }

    pub fn command_string(&self) -> String {
        self.command_line.join(" ")
    }
}

/// A pid paired with the process start time.
///
/// Pids are recycled by the OS. Storing the start time alongside the pid means
/// a stale record can never be used to signal an unrelated new process — the
/// single most important safety property of the whole lifecycle layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time_ms: i64,
}

impl ProcessIdentity {
    pub fn new(pid: u32, start_time_ms: i64) -> Self {
        Self { pid, start_time_ms }
    }

    /// Start times are reported at differing resolutions across platforms
    /// (macOS microseconds, Windows 100ns, sysinfo whole seconds), so compare
    /// with a tolerance rather than for exact equality.
    pub fn matches(&self, other: &ProcessIdentity) -> bool {
        const TOLERANCE_MS: i64 = 1_500;
        self.pid == other.pid && (self.start_time_ms - other.start_time_ms).abs() <= TOLERANCE_MS
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminationMode {
    /// SIGTERM on Unix; CTRL_BREAK / WM_CLOSE on Windows.
    Graceful,
    /// SIGKILL on Unix; TerminateProcess on Windows.
    Forceful,
}

/// Reads and terminates processes.
///
/// Implementations must never terminate by pid alone: every kill path takes a
/// [`ProcessIdentity`] and must re-verify it immediately before signalling.
pub trait ProcessProvider: Send + Sync {
    /// Every process visible to the current user.
    fn list_processes(&self) -> Result<Vec<ProcessInfo>>;

    fn process_info(&self, pid: u32) -> Result<Option<ProcessInfo>>;

    /// True when a process with this exact identity is still running.
    fn is_alive(&self, identity: &ProcessIdentity) -> Result<bool> {
        Ok(self
            .process_info(identity.pid)?
            .is_some_and(|info| info.identity().matches(identity)))
    }

    /// Transitive children of `pid`, deepest last.
    ///
    /// Dev servers routinely fork (`npm` -> `node`, `uvicorn` -> workers), so
    /// restart must reach the whole tree or leave orphans holding the port.
    fn descendants(&self, pid: u32) -> Result<Vec<u32>> {
        let all = self.list_processes()?;
        let mut out = Vec::new();
        let mut frontier = vec![pid];
        while let Some(current) = frontier.pop() {
            for proc in &all {
                if proc.parent_pid == Some(current) && !out.contains(&proc.pid) {
                    out.push(proc.pid);
                    frontier.push(proc.pid);
                }
            }
        }
        Ok(out)
    }

    /// Signal the process and everything it spawned.
    ///
    /// Returns `Ok(false)` when the identity no longer matches a live process,
    /// which callers should treat as success.
    fn terminate_tree(&self, identity: &ProcessIdentity, mode: TerminationMode) -> Result<bool>;
}

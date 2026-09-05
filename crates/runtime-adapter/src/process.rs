//! Process inspection and termination.

use std::path::PathBuf;

use runtime_types::{Result, StopSignal};
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
    /// The signal the service asked for on Unix, SIGTERM unless it said
    /// otherwise; CTRL_BREAK / WM_CLOSE on Windows, which has no equivalent
    /// choice and ignores the carried signal.
    ///
    /// Carried rather than decided here, because only the service knows what
    /// its program reads SIGTERM as. A stop does reach the whole process
    /// group, so a wrapped process sees the raw signal whatever its wrapper
    /// intends — but a wrapper that translates a moment later still works on
    /// anything that accepts an escalating second signal. What this is for is
    /// the program with no wrapper at all.
    Graceful(StopSignal),
    /// SIGKILL on Unix; TerminateProcess on Windows.
    Forceful,
}

impl TerminationMode {
    /// The ordinary graceful stop, for callers with no service in hand.
    pub const TERM: Self = Self::Graceful(StopSignal::Term);
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

    /// Variables a running process was started with, filtered to `keys`.
    ///
    /// Filtered rather than whole, and by the caller's list rather than this
    /// one's judgement: an environment holds credentials, and a runtime that
    /// copies it wholesale into a registry has quietly written them to disk.
    /// What the caller actually needs is much smaller — the handful of
    /// variables that select which *mode* a service runs in.
    ///
    /// The default is "cannot tell", which callers must treat as unknown
    /// rather than as empty.
    fn environment(&self, _pid: u32, _keys: &[&str]) -> Result<Option<Vec<(String, String)>>> {
        Ok(None)
    }

    /// Signal the process and everything it spawned.
    ///
    /// Returns `Ok(false)` when the identity no longer matches a live process,
    /// which callers should treat as success.
    /// Which process group a pid belongs to, where the platform has them.
    ///
    /// A service is spawned into a group of its own, and the group outlives its
    /// leader: `pnpm run dev` hands off to `node` and steps out of the way,
    /// leaving the server running in a group whose leader is gone. Judged by
    /// the leader alone the service reads as stopped while it is serving, and
    /// — because its port is still held — the runtime then reports it as
    /// something somebody else started and refuses to stop it.
    ///
    /// This is deliberately a question about *another* pid rather than "is the
    /// group alive". Asking whether the leader's group still exists would say
    /// yes to a recycled pid that happens to lead a group of its own, and the
    /// runtime would claim a stranger's processes. Asking which group the
    /// holder of our port is in cannot: the answer names the group we made.
    ///
    /// `None` where the platform has no process groups. Windows keeps a
    /// service's processes in a job object and does not need this.
    fn group_of(&self, _pid: u32) -> Result<Option<u32>> {
        Ok(None)
    }

    /// Signal whatever is still in the group `leader` started.
    ///
    /// The counterpart to [`group_of`](Self::group_of), for the case where the
    /// leader has gone and the group is still serving. There is no identity
    /// left to re-verify at that point, so this is only for a caller that has
    /// already established the group is the runtime's own — which it does by
    /// finding the holder of the service's port inside it.
    ///
    /// `false` where the platform has no process groups.
    fn terminate_group(&self, _leader: u32, _mode: TerminationMode) -> Result<bool> {
        Ok(false)
    }

    fn terminate_tree(&self, identity: &ProcessIdentity, mode: TerminationMode) -> Result<bool>;
}

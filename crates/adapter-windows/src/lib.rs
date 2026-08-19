//! Windows adapter.
//!
//! # Status
//!
//! This is a **working baseline**, not the finished adapter. It delegates
//! enumeration to the portable `sysinfo` / `netstat2` implementation and adds
//! the two Windows-specific behaviours the runtime cannot do without:
//! creating each service in its own process group, and terminating the whole
//! process tree.
//!
//! Everything marked `TODO(windows)` is a deliberate handoff point where a
//! native Win32 implementation should replace the baseline. The trait
//! signatures will not change, so that work is local to this crate.
//!
//! # Planned native work
//!
//! | Concern | Baseline here | Target |
//! |---|---|---|
//! | Port table | `netstat2` | `GetExtendedTcpTable` / `GetExtendedUdpTable` |
//! | Tree termination | `taskkill /T /F` | Job Object with `KILL_ON_JOB_CLOSE` |
//! | Graceful stop | forced | `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)` |
//! | Process metadata | `sysinfo` | `Toolhelp32Snapshot` + `QueryFullProcessImageName` |
//!
//! Parsing `netstat -ano` output is explicitly out of scope: the plan calls for
//! the API path in the shipped version.

#![cfg(windows)]

mod process;
mod spawn;

use runtime_adapter::generic::GenericPortProvider;
use runtime_adapter::{PlatformAdapter, PortProvider, ProcessProvider, SpawnProvider};

pub use process::WindowsProcessProvider;
pub use spawn::WindowsSpawnProvider;

#[derive(Debug, Default)]
pub struct WindowsAdapter {
    process: WindowsProcessProvider,
    // TODO(windows): replace with a GetExtendedTcpTable-backed provider.
    port: GenericPortProvider,
    spawn: WindowsSpawnProvider,
}

impl WindowsAdapter {
    pub fn new() -> Self {
        Self::default()
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

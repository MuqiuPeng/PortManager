//! macOS adapter.
//!
//! Everything here refines the portable adapter rather than replacing it:
//! `sysinfo` already resolves pid -> cwd on macOS and `netstat2` reads the
//! socket table through `libproc`. What macOS needs on top is
//!
//! * microsecond-accurate process start times, so [`ProcessIdentity`] is not
//!   limited to `sysinfo`'s one-second resolution, and
//! * process-group signalling, so a `pnpm dev` that forks a `node` child dies
//!   as a unit instead of leaving the port held by an orphan.

#![cfg(target_os = "macos")]

mod panel;
mod process;
mod spawn;

use runtime_adapter::generic::GenericPortProvider;
use runtime_adapter::{
    PlatformAdapter, PortProvider, ProcessProvider, SpawnProvider, WindowProvider,
};

pub use panel::{raw_window, MacWindowProvider};
pub use process::MacProcessProvider;
pub use spawn::MacSpawnProvider;

#[derive(Debug, Default)]
pub struct MacosAdapter {
    process: MacProcessProvider,
    port: GenericPortProvider,
    spawn: MacSpawnProvider,
    window: MacWindowProvider,
}

impl MacosAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PlatformAdapter for MacosAdapter {
    fn name(&self) -> &'static str {
        "macos"
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

    fn window(&self) -> Option<&dyn WindowProvider> {
        Some(&self.window)
    }
}

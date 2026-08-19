//! The OS boundary.
//!
//! `runtime-core` depends on these traits and never on a platform API. Adding a
//! platform means adding a crate that implements [`PlatformAdapter`]; nothing
//! in the core changes.

pub mod desktop;
pub mod generic;
pub mod port;
pub mod process;
pub mod spawn;

pub use desktop::{
    PanelActivation, PanelConfig, PanelState, RawWindow, ScreenEdge, ScreenInfo, WindowProvider,
};
pub use port::{PortBinding, PortProvider, Protocol};
pub use process::{ProcessIdentity, ProcessInfo, ProcessProvider, TerminationMode};
pub use spawn::SpawnProvider;

/// Everything the runtime needs from one operating system.
pub trait PlatformAdapter: Send + Sync + 'static {
    /// Short platform name, surfaced in `runtime doctor` and daemon info.
    fn name(&self) -> &'static str;

    fn process(&self) -> &dyn ProcessProvider;
    fn port(&self) -> &dyn PortProvider;
    fn spawn(&self) -> &dyn SpawnProvider;

    /// Edge-docked panel support, when the platform has it.
    ///
    /// Optional rather than required: the daemon never needs it, and a platform
    /// without a native panel implementation should still be able to run
    /// everything else.
    fn window(&self) -> Option<&dyn WindowProvider> {
        None
    }
}

/// The portable fallback, used on platforms without a native adapter.
#[derive(Debug, Default)]
pub struct GenericAdapter {
    process: generic::GenericProcessProvider,
    port: generic::GenericPortProvider,
    spawn: generic::GenericSpawnProvider,
}

impl GenericAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PlatformAdapter for GenericAdapter {
    fn name(&self) -> &'static str {
        "generic"
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

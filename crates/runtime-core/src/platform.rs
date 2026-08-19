//! Adapter selection.
//!
//! The one place in the crate that knows which operating system it is on.

use std::sync::Arc;

use runtime_adapter::PlatformAdapter;

/// The adapter for the platform this binary was built for.
pub fn current() -> Arc<dyn PlatformAdapter> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(adapter_macos::MacosAdapter::new())
    }
    #[cfg(windows)]
    {
        Arc::new(adapter_windows::WindowsAdapter::new())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        // Linux and anything else run on the portable implementation until a
        // native adapter crate exists.
        Arc::new(runtime_adapter::GenericAdapter::new())
    }
}

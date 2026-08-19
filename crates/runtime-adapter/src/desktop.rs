//! Seams for the desktop phases.
//!
//! Declared now so that Phase 2 and Phase 4 slot into the existing adapter
//! boundary instead of reaching for platform APIs from UI code. The headless
//! daemon does not use any of these.

use runtime_types::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenEdge {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelConfig {
    pub edge: ScreenEdge,
    pub width: u32,
    pub always_on_top: bool,
    pub auto_hide: bool,
    /// Do not steal focus from the editor when shown.
    pub non_activating: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_id: Option<String>,
}

/// Edge-docked side panel: `NSPanel` on macOS, a layered Win32 window on Windows.
pub trait WindowProvider: Send + Sync {
    fn show_panel(&self, config: &PanelConfig) -> Result<()>;
    fn hide_panel(&self) -> Result<()>;
    fn set_panel_config(&self, config: &PanelConfig) -> Result<()>;
}

pub trait NotificationProvider: Send + Sync {
    fn notify(&self, title: &str, body: &str) -> Result<()>;
}

/// Start the daemon at login: `launchd` on macOS, Task Scheduler / Run key on Windows.
pub trait AutostartProvider: Send + Sync {
    fn is_enabled(&self) -> Result<bool>;
    fn enable(&self) -> Result<()>;
    fn disable(&self) -> Result<()>;
}

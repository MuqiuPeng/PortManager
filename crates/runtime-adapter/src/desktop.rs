//! Seams for the desktop phases.
//!
//! Declared in Phase 0 so that the desktop work slots into the existing adapter
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelConfig {
    pub edge: ScreenEdge,
    /// Width when expanded.
    pub width: u32,
    /// Fraction of the screen height the expanded panel occupies, 0.0–1.0.
    pub height_ratio: f64,
    /// Width of the resting tab.
    pub island_width: u32,
    /// Height of the resting tab.
    pub island_height: u32,
    /// How far past the tab the pointer may be and still expand it.
    pub hover_margin: u32,
    /// Duration of the expand and collapse animation, in milliseconds.
    pub animation_ms: u32,
    /// Stay expanded instead of collapsing when the pointer leaves.
    pub pinned: bool,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            edge: ScreenEdge::Right,
            width: 300,
            height_ratio: 0.9,
            // Narrow enough to live at the screen edge without covering a
            // scrollbar, tall enough to be an obvious target.
            island_width: 10,
            island_height: 96,
            hover_margin: 6,
            animation_ms: 170,
            pinned: false,
        }
    }
}

/// The two sizes the panel lives at.
///
/// Not "hidden" and "shown": the resting state is a visible tab, so the panel
/// is discoverable and the expansion is a resize rather than an appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelState {
    /// A slim tab at the screen edge.
    Island,
    /// The full panel.
    Expanded,
}

/// How the panel was asked to appear.
///
/// The distinction is the whole point of a non-activating panel: a pointer
/// reveal must not take focus from the editor, while a deliberate keystroke
/// should, or the user cannot type into what they just summoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PanelActivation {
    /// Show without taking focus. Mouse-driven.
    Passive,
    /// Show and take focus, so the keyboard works.
    Focused,
}

/// A screen the panel can dock to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenInfo {
    pub id: String,
    /// Visible frame in logical points, excluding the menu bar and Dock.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale_factor: f64,
    pub primary: bool,
}

/// Edge-docked side panel: `NSPanel` on macOS, a layered Win32 window on Windows.
///
/// The implementation is handed the platform's native window handle by the
/// desktop app; nothing above this trait knows what an `NSWindow` is.
pub trait WindowProvider: Send + Sync {
    /// Convert an existing window into a floating, non-activating panel.
    ///
    /// Called once per window, before it is first shown.
    ///
    /// # Safety
    ///
    /// `handle` must be a valid native window handle for this platform, owned
    /// by the caller and alive for the duration of the call.
    unsafe fn adopt_panel(&self, handle: RawWindow) -> Result<()>;

    /// Move the panel to the geometry for `state`, animating the change.
    ///
    /// Collapsing also makes the tab click-through, so a permanently visible
    /// strip at the screen edge does not swallow clicks meant for the window
    /// underneath.
    ///
    /// # Safety
    ///
    /// See [`WindowProvider::adopt_panel`].
    unsafe fn apply_state(
        &self,
        handle: RawWindow,
        config: &PanelConfig,
        screen: Option<&str>,
        state: PanelState,
        activation: PanelActivation,
    ) -> Result<()>;

    /// Where the resting tab is, in screen coordinates, so the caller can tell
    /// whether the pointer is near it.
    ///
    /// Returns `(x, y, width, height)`.
    fn island_rect(&self, config: &PanelConfig, screen: Option<&str>) -> Result<(f64, f64, f64, f64)>;

    /// Screens the panel can dock to, for the settings UI and for choosing
    /// the one under the pointer.
    fn screens(&self) -> Result<Vec<ScreenInfo>>;

    /// The screen currently containing the pointer, if it can be determined.
    fn screen_at_pointer(&self) -> Result<Option<ScreenInfo>>;
}

/// An opaque native window handle.
///
/// `NSWindow*` on macOS, `HWND` on Windows. Kept as a bare pointer so the
/// trait stays free of platform types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawWindow(pub *mut std::ffi::c_void);

// SAFETY: the pointer is only dereferenced by the platform implementation,
// which is responsible for doing so on the correct thread.
unsafe impl Send for RawWindow {}
unsafe impl Sync for RawWindow {}

pub trait NotificationProvider: Send + Sync {
    fn notify(&self, title: &str, body: &str) -> Result<()>;
}

/// Start the daemon at login: `launchd` on macOS, Stack Scheduler / Run key on Windows.
pub trait AutostartProvider: Send + Sync {
    fn is_enabled(&self) -> Result<bool>;
    fn enable(&self) -> Result<()>;
    fn disable(&self) -> Result<()>;
}

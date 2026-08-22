//! The edge-docked panel.
//!
//! The panel is never absent: at rest it is a slim tab against the screen edge,
//! and expanding is a resize rather than an appearance. That makes it
//! discoverable — an invisible hover strip is something you have to be told
//! about — and it makes the expansion animatable, because there is already a
//! window on screen to animate.
//!
//! ```text
//! island ──pointer reaches the tab──▶ expanded (passive: keeps the editor's focus)
//!   ▲                                    │
//!   └────pointer leaves the panel────────┘
//! island ──shortcut / menu bar──▶ expanded (focused: keyboard works)
//! pinned ────────────────────────▶ expanded always
//! ```
//!
//! The tab is click-through while resting, so a permanent strip at the screen
//! edge never swallows a click meant for the window underneath. Proximity is
//! found by polling the pointer, which works regardless of who receives events.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime_adapter::{PanelActivation, PanelConfig, PanelState, RawWindow, WindowProvider};
use runtime_types::{Result, RuntimeError};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

/// The window label the panel is created with.
pub const PANEL_LABEL: &str = "panel";

/// The frontend listens on this to switch between tab and full content.
pub const STATE_CHANNEL: &str = "panel://state";

/// Where the panel's geometry is stored.
///
/// In the daemon rather than beside the app, so it survives reinstalling the
/// bundle and there is one answer to "where is the state" instead of two.
pub const SETTINGS_KEY: &str = "desktop.panel";

/// Chosen for a low chance of collision; changeable from the settings screen.
pub const DEFAULT_SHORTCUT: &str = "CmdOrCtrl+Alt+L";

/// How often the pointer is checked against the tab.
///
/// Cheap enough to be unnoticeable and fast enough to feel immediate; a global
/// event tap would demand Accessibility permission for something this small.
const HOVER_POLL: Duration = Duration::from_millis(80);

/// How far past the expanded panel the pointer may stray before it collapses.
///
/// Without slack the panel snaps shut when the pointer crosses a one-pixel gap
/// on its way to a button.
const COLLAPSE_MARGIN: f64 = 32.0;

/// Everything the panel remembers between launches.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PanelSettings {
    #[serde(flatten)]
    pub config: PanelConfig,
    /// The global shortcut that summons the panel.
    #[serde(default = "default_shortcut")]
    pub shortcut: String,
    /// Screen to dock to; `None` follows the pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
}

fn default_shortcut() -> String {
    DEFAULT_SHORTCUT.to_string()
}

impl Default for PanelSettings {
    fn default() -> Self {
        Self {
            config: PanelConfig::default(),
            shortcut: default_shortcut(),
            screen: None,
        }
    }
}

pub struct PanelController {
    config: Mutex<PanelConfig>,
    shortcut: Mutex<String>,
    /// Screen id to dock to; `None` follows the pointer.
    screen: Mutex<Option<String>>,
    expanded: AtomicBool,
    /// True while expanded because the pointer is near the tab, as opposed to
    /// having been summoned deliberately.
    from_hover: AtomicBool,
}

impl Default for PanelController {
    fn default() -> Self {
        Self {
            config: Mutex::new(PanelConfig::default()),
            shortcut: Mutex::new(default_shortcut()),
            screen: Mutex::new(None),
            expanded: AtomicBool::new(false),
            from_hover: AtomicBool::new(false),
        }
    }
}

impl PanelController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn config(&self) -> PanelConfig {
        self.config.lock().map(|c| c.clone()).unwrap_or_default()
    }

    pub fn settings(&self) -> PanelSettings {
        PanelSettings {
            config: self.config(),
            shortcut: self
                .shortcut
                .lock()
                .map(|s| s.clone())
                .unwrap_or_else(|_| default_shortcut()),
            screen: self.screen.lock().map(|s| s.clone()).unwrap_or(None),
        }
    }

    /// Adopt settings read back from the daemon, without re-saving them.
    pub fn load(&self, settings: PanelSettings) {
        if let Ok(mut guard) = self.config.lock() {
            *guard = settings.config;
        }
        if let Ok(mut guard) = self.shortcut.lock() {
            *guard = settings.shortcut;
        }
        if let Ok(mut guard) = self.screen.lock() {
            *guard = settings.screen;
        }
    }

    pub fn set_screen(&self, screen: Option<String>) {
        if let Ok(mut guard) = self.screen.lock() {
            *guard = screen;
        }
    }

    pub fn set_shortcut(&self, shortcut: String) {
        if let Ok(mut guard) = self.shortcut.lock() {
            *guard = shortcut;
        }
    }

    pub fn set_config(&self, app: &AppHandle, config: PanelConfig) -> Result<()> {
        {
            let mut guard = self
                .config
                .lock()
                .map_err(|_| RuntimeError::internal("panel config lock poisoned"))?;
            *guard = config.clone();
        }
        // Pinning expands; unpinning leaves the panel out until the pointer
        // moves away, which is less jarring than snapping shut under the cursor.
        let state = if config.pinned || self.is_expanded() {
            PanelState::Expanded
        } else {
            PanelState::Island
        };
        self.apply(app, state, PanelActivation::Passive)
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded.load(Ordering::Relaxed)
    }

    /// Put the panel at rest as a tab. Called once at startup.
    pub fn rest(&self, app: &AppHandle) -> Result<()> {
        self.apply(app, PanelState::Island, PanelActivation::Passive)
    }

    pub fn expand(&self, app: &AppHandle, activation: PanelActivation) -> Result<()> {
        self.apply(app, PanelState::Expanded, activation)?;
        self.from_hover
            .store(activation == PanelActivation::Passive, Ordering::Relaxed);
        Ok(())
    }

    pub fn collapse(&self, app: &AppHandle) -> Result<()> {
        if self.config().pinned {
            return Ok(());
        }
        self.apply(app, PanelState::Island, PanelActivation::Passive)?;
        self.from_hover.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// The shortcut and the menu bar item both land here.
    pub fn toggle(&self, app: &AppHandle) -> Result<()> {
        // A panel that opened because the pointer drifted to the edge is not
        // "already open" as far as a deliberate keystroke is concerned — that
        // should focus it, not dismiss it, or the shortcut appears to do nothing.
        if self.is_expanded() && !self.from_hover.load(Ordering::Relaxed) {
            self.collapse(app)
        } else {
            self.expand(app, PanelActivation::Focused)
        }
    }

    fn apply(&self, app: &AppHandle, state: PanelState, activation: PanelActivation) -> Result<()> {
        let config = self.config();
        let screen = self.screen.lock().map(|s| s.clone()).unwrap_or(None);

        with_panel(app, move |provider, handle| {
            // SAFETY: `handle` comes from the live panel window, and Tauri runs
            // commands and main-thread closures on the main thread.
            unsafe { provider.apply_state(handle, &config, screen.as_deref(), state, activation) }
        })?;

        self.expanded
            .store(state == PanelState::Expanded, Ordering::Relaxed);
        // Told, not polled: the webview swaps between tab and full content.
        let _ = app.emit(STATE_CHANNEL, state);
        Ok(())
    }

    /// Watch the pointer and expand the tab when it arrives.
    ///
    /// Pointer and screen queries are AppKit calls, so the check itself is
    /// hopped onto the main thread; the timer is not.
    pub fn watch_edge(self: &Arc<Self>, app: &AppHandle) {
        let controller = Arc::clone(self);
        let app = app.clone();

        std::thread::spawn(move || loop {
            std::thread::sleep(HOVER_POLL);

            let controller = Arc::clone(&controller);
            let handle = app.clone();
            if app
                .run_on_main_thread(move || {
                    if let Err(err) = controller.check_pointer(&handle) {
                        tracing::debug!(%err, "edge watch failed");
                    }
                })
                .is_err()
            {
                return; // the app is shutting down
            }
        });
    }

    fn check_pointer(&self, app: &AppHandle) -> Result<()> {
        let config = self.config();
        if config.pinned {
            return Ok(());
        }
        let Some(provider) = window_provider() else {
            return Ok(());
        };
        let Some((px, py)) = pointer_location() else {
            return Ok(());
        };

        let screen = self.screen.lock().map(|s| s.clone()).unwrap_or(None);
        let (x, y, width, height) = provider.island_rect(&config, screen.as_deref())?;
        let margin = config.hover_margin as f64;

        if !self.is_expanded() {
            // Both axes: drifting along the far edge of the screen well above
            // or below the tab should not open the panel.
            let near = px >= x - margin
                && px <= x + width + margin
                && py >= y - margin
                && py <= y + height + margin;
            if near {
                self.expand(app, PanelActivation::Passive)?;
            }
            return Ok(());
        }

        // Only a hover-opened panel closes itself: one the user summoned
        // deliberately stays until they dismiss it.
        if !self.from_hover.load(Ordering::Relaxed) {
            return Ok(());
        }

        let expanded_left = match config.edge {
            runtime_adapter::ScreenEdge::Right => x + width - config.width as f64,
            runtime_adapter::ScreenEdge::Left => x,
        };
        let expanded_right = expanded_left + config.width as f64;
        let strayed = px < expanded_left - COLLAPSE_MARGIN || px > expanded_right + COLLAPSE_MARGIN;
        if strayed {
            self.collapse(app)?;
        }
        Ok(())
    }
}

/// Run something with the platform panel provider and the panel's native handle.
/// Do something to the panel window, on the thread that is allowed to.
///
/// Every AppKit call needs the main thread, and the callers here mostly do not
/// look like UI code: a global shortcut handler runs on the shortcut plugin's
/// thread, a tray callback on the tray's, an async command on the async
/// runtime. Each of them reaching straight for the window produced "panel
/// windows must be touched on the main thread" — a rule the platform enforces
/// and the callers were expected to remember.
///
/// So it is not remembered here either. The hop happens once, in the one place
/// that touches the window, and every caller is right by construction. Already
/// on the main thread the closure runs inline: dispatching to a thread you are
/// already on and then waiting for it is a deadlock.
fn with_panel<F>(app: &AppHandle, action: F) -> Result<()>
where
    F: FnOnce(&dyn WindowProvider, RawWindow) -> Result<()> + Send + 'static,
{
    let Some(provider) = window_provider() else {
        return Err(RuntimeError::unsupported("edge panels"));
    };

    if provider.on_main_thread() {
        let window = panel_window(app)?;
        return action(provider, native_handle(&window)?);
    }

    let (tell, hear) = std::sync::mpsc::sync_channel(1);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let outcome = window_provider()
            .ok_or_else(|| RuntimeError::unsupported("edge panels"))
            .and_then(|provider| {
                let window = panel_window(&handle)?;
                action(provider, native_handle(&window)?)
            });
        // The receiver is only gone if the caller was torn down mid-hop, which
        // is not worth failing the main thread over.
        let _ = tell.send(outcome);
    })
    .map_err(|err| RuntimeError::internal(format!("could not reach the main thread: {err}")))?;

    hear.recv()
        .unwrap_or_else(|_| Err(RuntimeError::internal("the main thread dropped the panel work")))
}

pub fn panel_window(app: &AppHandle) -> Result<WebviewWindow> {
    app.get_webview_window(PANEL_LABEL)
        .ok_or_else(|| RuntimeError::not_found("window", PANEL_LABEL))
}

/// Screens the panel can dock to.
pub fn screens() -> Vec<runtime_adapter::ScreenInfo> {
    window_provider()
        .and_then(|provider| provider.screens().ok())
        .unwrap_or_default()
}

/// Turn the panel into a real platform panel. Called once, before first show.
pub fn adopt(app: &AppHandle) -> Result<()> {
    with_panel(app, |provider, handle| {
        // SAFETY: the handle belongs to a window Tauri has just created and
        // this runs on the main thread during setup.
        unsafe { provider.adopt_panel(handle) }
    })
}

#[cfg(target_os = "macos")]
fn window_provider() -> Option<&'static dyn WindowProvider> {
    use std::sync::OnceLock;
    static PROVIDER: OnceLock<adapter_macos::MacWindowProvider> = OnceLock::new();
    Some(PROVIDER.get_or_init(adapter_macos::MacWindowProvider::new))
}

#[cfg(not(target_os = "macos"))]
fn window_provider() -> Option<&'static dyn WindowProvider> {
    // TODO(windows): a layered WS_EX_NOACTIVATE window, see docs/windows.md.
    None
}

#[cfg(target_os = "macos")]
fn native_handle(window: &WebviewWindow) -> Result<RawWindow> {
    window
        .ns_window()
        .map(RawWindow)
        .map_err(|err| RuntimeError::internal(format!("no native window handle: {err}")))
}

#[cfg(not(target_os = "macos"))]
fn native_handle(_window: &WebviewWindow) -> Result<RawWindow> {
    Err(RuntimeError::unsupported("edge panels"))
}

/// Pointer position in the platform's screen coordinates.
#[cfg(target_os = "macos")]
fn pointer_location() -> Option<(f64, f64)> {
    let point = objc2_app_kit::NSEvent::mouseLocation();
    Some((point.x, point.y))
}

#[cfg(not(target_os = "macos"))]
fn pointer_location() -> Option<(f64, f64)> {
    None
}

//! The edge-docked panel.
//!
//! Four ways in — a global shortcut, the menu bar item, hovering the screen
//! edge, and pinning it open — but one state machine, because they are all the
//! same question asked differently: should the panel be on screen right now?
//!
//! ```text
//! hidden ──hover──▶ shown (passive: keeps the editor's focus)
//!    ▲                 │
//!    └──pointer left───┘
//! hidden ──shortcut / menu bar──▶ shown (focused: keyboard works)
//! pinned ─────────────────────────▶ always shown, never auto-hides
//! ```
//!
//! Platform specifics live behind `WindowProvider`; this module only decides
//! *when*.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime_adapter::{PanelActivation, PanelConfig, RawWindow, ScreenEdge, WindowProvider};
use runtime_types::{Result, RuntimeError};
use tauri::{AppHandle, Manager, WebviewWindow};

/// The window label the panel is created with.
pub const PANEL_LABEL: &str = "panel";

/// How often the pointer is checked against the trigger strip.
///
/// Cheap enough to be unnoticeable and fast enough to feel immediate; the
/// alternative, a global event tap, would demand Accessibility permission for
/// something this small.
const HOVER_POLL: Duration = Duration::from_millis(90);

/// How far past the panel the pointer may stray before it hides.
///
/// Without slack the panel flickers shut when the pointer crosses a one-pixel
/// gap on its way to a button.
const HIDE_MARGIN: f64 = 24.0;

pub struct PanelController {
    config: Mutex<PanelConfig>,
    /// Screen id to dock to; `None` follows the pointer.
    screen: Mutex<Option<String>>,
    visible: AtomicBool,
    /// True while the panel is showing because the pointer is at the edge, as
    /// opposed to having been summoned deliberately.
    from_hover: AtomicBool,
}

impl Default for PanelController {
    fn default() -> Self {
        Self {
            config: Mutex::new(PanelConfig::default()),
            screen: Mutex::new(None),
            visible: AtomicBool::new(false),
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

    pub fn set_config(&self, app: &AppHandle, config: PanelConfig) -> Result<()> {
        {
            let mut guard = self
                .config
                .lock()
                .map_err(|_| RuntimeError::internal("panel config lock poisoned"))?;
            *guard = config.clone();
        }
        // Pinning is a show; unpinning leaves it where it is until the pointer
        // moves away, which is less jarring than snapping shut.
        if config.pinned {
            self.show(app, PanelActivation::Passive)?;
        }
        Ok(())
    }

    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    pub fn show(&self, app: &AppHandle, activation: PanelActivation) -> Result<()> {
        let config = self.config();
        let screen = self
            .screen
            .lock()
            .map(|s| s.clone())
            .unwrap_or(None);

        with_panel(app, move |provider, handle| {
            // SAFETY: `handle` comes from the live panel window, and Tauri runs
            // this closure on the main thread.
            unsafe { provider.show_panel(handle, &config, screen.as_deref(), activation) }
        })?;

        self.visible.store(true, Ordering::Relaxed);
        self.from_hover
            .store(activation == PanelActivation::Passive, Ordering::Relaxed);
        Ok(())
    }

    pub fn hide(&self, app: &AppHandle) -> Result<()> {
        if self.config().pinned {
            return Ok(());
        }
        with_panel(app, |provider, handle| {
            // SAFETY: see `show`.
            unsafe { provider.hide_panel(handle) }
        })?;
        self.visible.store(false, Ordering::Relaxed);
        self.from_hover.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// The shortcut and the menu bar item both land here.
    pub fn toggle(&self, app: &AppHandle) -> Result<()> {
        // A panel revealed by hovering is not "already open" as far as a
        // deliberate keystroke is concerned — that should focus it, not dismiss
        // it, or the shortcut appears to do nothing.
        if self.is_visible() && !self.from_hover.load(Ordering::Relaxed) {
            self.hide(app)
        } else {
            self.show(app, PanelActivation::Focused)
        }
    }

    /// Watch the pointer and reveal the panel when it reaches the edge.
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
            let posted = app.run_on_main_thread(move || {
                if let Err(err) = controller.check_pointer(&handle) {
                    tracing::debug!(%err, "edge watch failed");
                }
            });
            if posted.is_err() {
                return; // the app is shutting down
            }
        });
    }

    fn check_pointer(&self, app: &AppHandle) -> Result<()> {
        let config = self.config();
        if config.pinned || config.hover_strip_width == 0 {
            return Ok(());
        }

        let Some(provider) = window_provider() else {
            return Ok(());
        };
        let Some(screen) = provider.screen_at_pointer()? else {
            return Ok(());
        };
        let Some(pointer) = pointer_location() else {
            return Ok(());
        };

        let strip = config.hover_strip_width as f64;
        let at_edge = match config.edge {
            ScreenEdge::Right => pointer.0 >= screen.x + screen.width - strip,
            ScreenEdge::Left => pointer.0 <= screen.x + strip,
        };

        if at_edge && !self.is_visible() {
            self.show(app, PanelActivation::Passive)?;
            return Ok(());
        }

        // Only a hover-revealed panel closes itself: one the user summoned
        // deliberately stays until they dismiss it.
        if self.is_visible() && self.from_hover.load(Ordering::Relaxed) {
            let panel_edge = match config.edge {
                ScreenEdge::Right => screen.x + screen.width - config.width as f64,
                ScreenEdge::Left => screen.x + config.width as f64,
            };
            let strayed = match config.edge {
                ScreenEdge::Right => pointer.0 < panel_edge - HIDE_MARGIN,
                ScreenEdge::Left => pointer.0 > panel_edge + HIDE_MARGIN,
            };
            if strayed {
                self.hide(app)?;
            }
        }
        Ok(())
    }
}

/// Run something with the platform panel provider and the panel's native handle.
fn with_panel<F>(app: &AppHandle, action: F) -> Result<()>
where
    F: FnOnce(&dyn WindowProvider, RawWindow) -> Result<()>,
{
    let Some(provider) = window_provider() else {
        return Err(RuntimeError::unsupported("edge panels"));
    };
    let window = panel_window(app)?;
    action(provider, native_handle(&window)?)
}

pub fn panel_window(app: &AppHandle) -> Result<WebviewWindow> {
    app.get_webview_window(PANEL_LABEL)
        .ok_or_else(|| RuntimeError::not_found("window", PANEL_LABEL))
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

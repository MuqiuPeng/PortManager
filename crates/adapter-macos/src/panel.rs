//! The edge-docked panel on macOS.
//!
//! # Why this is not just a window
//!
//! The panel's defining property is that clicking it does **not** take focus
//! from the editor. On macOS that is `NSWindowStyleMask::NonactivatingPanel`,
//! and the window server only honours it for `NSPanel`. Tauri creates an
//! `NSWindow`, so the class is swapped at runtime — the standard approach for
//! this on macOS, and the reason this module exists rather than a few extra
//! lines in the desktop app.
//!
//! Everything else follows from that: the panel floats above ordinary windows,
//! joins every Space so it does not vanish when the user switches, and stays
//! available over full-screen apps.

use std::ffi::c_void;

use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, ClassType};
use objc2_app_kit::{
    NSAnimationContext, NSColor, NSPanel, NSScreen, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use runtime_adapter::desktop::{
    PanelActivation, PanelConfig, PanelState, RawWindow, ScreenEdge, ScreenInfo, WindowProvider,
};
use runtime_types::{Result, RuntimeError};

/// Above normal windows and the Dock, below the menu bar.
///
/// `NSFloatingWindowLevel` is not enough: a full-screen app's window sits
/// higher, and the panel would disappear behind it.
const PANEL_WINDOW_LEVEL: isize = 3; // NSStatusWindowLevel

#[derive(Debug, Default)]
pub struct MacWindowProvider;

impl MacWindowProvider {
    pub fn new() -> Self {
        Self
    }

    /// Reinterpret a raw handle as an `NSWindow`.
    ///
    /// # Safety
    ///
    /// `handle` must be a live `NSWindow*` and the caller must be on the main
    /// thread, which every AppKit call below requires.
    unsafe fn window<'a>(handle: RawWindow) -> Result<&'a NSWindow> {
        if handle.0.is_null() {
            return Err(RuntimeError::invalid("panel window handle is null"));
        }
        MainThreadMarker::new()
            .ok_or_else(|| RuntimeError::internal("panel windows must be touched on the main thread"))?;
        // SAFETY: the caller guarantees a live NSWindow pointer; the reference
        // does not outlive the call because the return type is tied to `'a`
        // chosen by the caller at each use site below.
        Ok(unsafe { &*(handle.0 as *const NSWindow) })
    }
}

impl MacWindowProvider {
    /// The screen to dock to: the requested one, else the one under the
    /// pointer, else the first available.
    fn resolve_screen(&self, screen: Option<&str>) -> Result<ScreenInfo> {
        let requested = match screen {
            Some(id) => self
                .screens()?
                .into_iter()
                .find(|candidate| candidate.id == id),
            None => None,
        };
        requested
            .or(self.screen_at_pointer()?)
            .or_else(|| self.screens().ok().and_then(|s| s.into_iter().next()))
            .ok_or_else(|| RuntimeError::internal("no screen available for the panel"))
    }
}

impl WindowProvider for MacWindowProvider {
    fn on_main_thread(&self) -> bool {
        MainThreadMarker::new().is_some()
    }

    unsafe fn adopt_panel(&self, handle: RawWindow) -> Result<()> {
        let window = unsafe { Self::window(handle)? };

        // Swap NSWindow -> NSPanel. `NonactivatingPanel` is silently ignored on
        // a plain NSWindow, so without this the panel would steal focus on
        // every click and the whole design would be pointless.
        let panel_class: &AnyClass = NSPanel::class();
        // SAFETY: NSPanel is a subclass of NSWindow with no extra ivars, so
        // reinterpreting an NSWindow instance as one is layout-compatible.
        // This is the documented approach used by every macOS panel library.
        unsafe {
            objc2::ffi::object_setClass(
                handle.0 as *mut objc2::runtime::AnyObject as *mut _,
                panel_class as *const AnyClass as *mut _,
            );
        }

        let style = window.styleMask() | NSWindowStyleMask::NonactivatingPanel;
        window.setStyleMask(style);
        window.setLevel(PANEL_WINDOW_LEVEL);

        // Without this the window paints an opaque rectangle behind the
        // content, and the rounded corners the CSS draws show white squares.
        window.setOpaque(false);
        let clear = NSColor::clearColor();
        window.setBackgroundColor(Some(&clear));
        window.setHasShadow(true);

        // Follow the user across Spaces and sit over full-screen apps rather
        // than being confined to the Space it was created on.
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary,
        );

        // A panel is not a document: it should not appear in the window menu,
        // in Exposé, or take part in window cycling.
        window.setHidesOnDeactivate(false);
        window.setMovableByWindowBackground(false);
        // SAFETY: `setFloatingPanel:` is declared on NSPanel, which the window
        // now is.
        unsafe {
            let _: () = msg_send![window, setFloatingPanel: true];
            let _: () = msg_send![window, setBecomesKeyOnlyIfNeeded: true];
        }

        // Read the result back rather than assuming it took. A silently failed
        // swap looks identical at startup and only shows up later as a panel
        // that steals focus on every click — the one thing it must never do.
        let applied = window
            .styleMask()
            .contains(NSWindowStyleMask::NonactivatingPanel);
        // SAFETY: `class` is a standard NSObject method.
        let class_name = unsafe {
            let class: *const AnyClass = msg_send![window, class];
            std::ffi::CStr::from_ptr(objc2::ffi::class_getName(class as *mut _))
                .to_string_lossy()
                .to_string()
        };

        if !applied {
            return Err(RuntimeError::internal(format!(
                "the panel window is a {class_name} and did not accept the non-activating style; \
                 it would steal focus on every click"
            )));
        }
        tracing::info!(class = %class_name, "panel adopted: non-activating, floating, all Spaces");
        Ok(())
    }

    unsafe fn apply_state(
        &self,
        handle: RawWindow,
        config: &PanelConfig,
        screen: Option<&str>,
        state: PanelState,
        activation: PanelActivation,
    ) -> Result<()> {
        let window = unsafe { Self::window(handle)? };
        let target = self.resolve_screen(screen)?;
        let frame = frame_for(&target, config, state);

        // The tab is on screen permanently, so it must not swallow clicks meant
        // for whatever is underneath. Proximity is detected by polling the
        // pointer, which works regardless of who receives the events.
        window.setIgnoresMouseEvents(state == PanelState::Island);

        // Order front before animating: a window that is not on screen has
        // nothing to animate, and the first expansion would simply appear.
        match activation {
            // `orderFrontRegardless` shows the panel without activating the
            // application, which is what keeps the editor's focus intact.
            PanelActivation::Passive => window.orderFrontRegardless(),
            PanelActivation::Focused => window.makeKeyAndOrderFront(None),
        }

        set_frame_animated(window, frame, config.animation_ms);
        Ok(())
    }

    fn island_rect(&self, config: &PanelConfig, screen: Option<&str>) -> Result<(f64, f64, f64, f64)> {
        let target = self.resolve_screen(screen)?;
        let frame = frame_for(&target, config, PanelState::Island);
        Ok((
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        ))
    }

    fn screens(&self) -> Result<Vec<ScreenInfo>> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| RuntimeError::internal("screens must be read on the main thread"))?;

        let screens = NSScreen::screens(mtm);
        let main = NSScreen::mainScreen(mtm);

        let mut out = Vec::new();
        for (index, screen) in screens.iter().enumerate() {
            // The *visible* frame, so the panel never lands under the menu bar
            // or the Dock.
            let frame = screen.visibleFrame();
            out.push(ScreenInfo {
                // NSScreen has no stable public identifier; the index is stable
                // for as long as the display arrangement is unchanged, which is
                // enough for a user picking a screen in settings.
                id: format!("screen-{index}"),
                x: frame.origin.x,
                y: frame.origin.y,
                width: frame.size.width,
                height: frame.size.height,
                scale_factor: screen.backingScaleFactor(),
                primary: main.as_ref().is_some_and(|m| std::ptr::eq(&**m, &*screen)),
            });
        }
        Ok(out)
    }

    fn screen_at_pointer(&self) -> Result<Option<ScreenInfo>> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| RuntimeError::internal("screens must be read on the main thread"))?;

        let location = objc2_app_kit::NSEvent::mouseLocation();
        for (index, screen) in NSScreen::screens(mtm).iter().enumerate() {
            let frame = screen.frame();
            if contains(frame, location) {
                let visible = screen.visibleFrame();
                return Ok(Some(ScreenInfo {
                    id: format!("screen-{index}"),
                    x: visible.origin.x,
                    y: visible.origin.y,
                    width: visible.size.width,
                    height: visible.size.height,
                    scale_factor: screen.backingScaleFactor(),
                    primary: index == 0,
                }));
            }
        }
        Ok(None)
    }
}

/// Where the panel sits on a screen, at a given size.
///
/// Cocoa's origin is bottom-left, so a vertically centred panel is placed from
/// the bottom up rather than the top down.
fn frame_for(screen: &ScreenInfo, config: &PanelConfig, state: PanelState) -> NSRect {
    let (width, height) = match state {
        PanelState::Island => (
            config.island_width as f64,
            (config.island_height as f64).min(screen.height),
        ),
        PanelState::Expanded => (
            config.width as f64,
            (screen.height * config.height_ratio.clamp(0.1, 1.0)).min(screen.height),
        ),
    };

    let y = screen.y + (screen.height - height) / 2.0;
    let x = match config.edge {
        ScreenEdge::Right => screen.x + screen.width - width,
        ScreenEdge::Left => screen.x,
    };

    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

/// Animate the window to a new frame.
///
/// The expansion has to be animated for the panel to read as sliding out of the
/// edge rather than appearing on top of the screen; `NSAnimationContext` drives
/// the window's own animator so the webview resizes with it.
fn set_frame_animated(window: &NSWindow, frame: NSRect, duration_ms: u32) {
    if duration_ms == 0 {
        window.setFrame_display(frame, true);
        return;
    }

    // SAFETY: standard AppKit animation grouping on the main thread; `animator`
    // returns a proxy that forwards `setFrame:display:` through the animation.
    unsafe {
        NSAnimationContext::beginGrouping();
        let context = NSAnimationContext::currentContext();
        context.setDuration(duration_ms as f64 / 1000.0);

        let animator: *mut AnyObject = msg_send![window, animator];
        let _: () = msg_send![animator, setFrame: frame, display: true];

        NSAnimationContext::endGrouping();
    }
}

fn contains(frame: NSRect, point: NSPoint) -> bool {
    point.x >= frame.origin.x
        && point.x < frame.origin.x + frame.size.width
        && point.y >= frame.origin.y
        && point.y < frame.origin.y + frame.size.height
}

/// Convert a Tauri window handle into the raw pointer the provider expects.
pub fn raw_window(ns_window: *mut c_void) -> RawWindow {
    RawWindow(ns_window)
}

#[cfg(test)]
mod tests {
    use runtime_adapter::WindowProvider;


    /// The question `with_panel` asks before touching a window.
    ///
    /// It has to be answerable from a thread that is not the main one, because
    /// that is the case it exists for: a global shortcut handler and a tray
    /// callback both arrive on threads of their own, and both used to reach
    /// straight for the window and fail.
    #[test]
    fn a_provider_knows_whether_this_thread_may_touch_windows() {
        let off = std::thread::spawn(|| MacWindowProvider::new().on_main_thread())
            .join()
            .unwrap();
        assert!(!off, "a spawned thread claimed it was the main one");
    }
    use super::*;

    fn screen() -> ScreenInfo {
        ScreenInfo {
            id: "screen-0".to_string(),
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
            scale_factor: 2.0,
            primary: true,
        }
    }

    #[test]
    fn the_right_edge_puts_both_states_flush_against_it() {
        let config = PanelConfig {
            edge: ScreenEdge::Right,
            width: 300,
            island_width: 10,
            ..PanelConfig::default()
        };

        let expanded = frame_for(&screen(), &config, PanelState::Expanded);
        assert_eq!(expanded.origin.x + expanded.size.width, 1440.0);
        assert_eq!(expanded.size.width, 300.0);

        // The tab has to share the outer edge, or expanding would look like the
        // panel jumping sideways rather than growing out of it.
        let island = frame_for(&screen(), &config, PanelState::Island);
        assert_eq!(island.origin.x + island.size.width, 1440.0);
        assert_eq!(island.size.width, 10.0);
    }

    #[test]
    fn the_left_edge_starts_at_the_screen_origin_in_both_states() {
        let config = PanelConfig {
            edge: ScreenEdge::Left,
            ..PanelConfig::default()
        };
        assert_eq!(frame_for(&screen(), &config, PanelState::Expanded).origin.x, 0.0);
        assert_eq!(frame_for(&screen(), &config, PanelState::Island).origin.x, 0.0);
    }

    #[test]
    fn both_states_share_a_vertical_centre() {
        let config = PanelConfig {
            height_ratio: 0.8,
            island_height: 96,
            ..PanelConfig::default()
        };
        let expanded = frame_for(&screen(), &config, PanelState::Expanded);
        let island = frame_for(&screen(), &config, PanelState::Island);

        assert_eq!(expanded.size.height, 720.0);
        // Same midpoint, so the panel grows symmetrically out of the tab.
        assert_eq!(
            expanded.origin.y + expanded.size.height / 2.0,
            island.origin.y + island.size.height / 2.0
        );
    }

    #[test]
    fn the_panel_is_never_taller_than_the_screen() {
        let oversized = PanelConfig {
            height_ratio: 2.0,
            ..PanelConfig::default()
        };
        assert_eq!(
            frame_for(&screen(), &oversized, PanelState::Expanded).size.height,
            900.0
        );
    }

    /// A secondary display sits at a non-zero origin; the panel must dock to
    /// that screen's edge, not the desktop's.
    #[test]
    fn a_secondary_screen_docks_to_its_own_edge() {
        let secondary = ScreenInfo {
            id: "screen-1".to_string(),
            x: 1440.0,
            y: 0.0,
            width: 2560.0,
            height: 1440.0,
            scale_factor: 2.0,
            primary: false,
        };
        let config = PanelConfig {
            edge: ScreenEdge::Right,
            width: 320,
            ..PanelConfig::default()
        };
        let frame = frame_for(&secondary, &config, PanelState::Expanded);
        assert_eq!(frame.origin.x, 1440.0 + 2560.0 - 320.0);
    }
}

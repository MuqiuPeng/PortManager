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

use objc2::runtime::AnyClass;
use objc2::{msg_send, ClassType};
use objc2_app_kit::{
    NSPanel, NSScreen, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use runtime_adapter::desktop::{
    PanelActivation, PanelConfig, RawWindow, ScreenEdge, ScreenInfo, WindowProvider,
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

impl WindowProvider for MacWindowProvider {
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

    unsafe fn show_panel(
        &self,
        handle: RawWindow,
        config: &PanelConfig,
        screen: Option<&str>,
        activation: PanelActivation,
    ) -> Result<()> {
        let window = unsafe { Self::window(handle)? };

        let target = match screen {
            Some(id) => self
                .screens()?
                .into_iter()
                .find(|candidate| candidate.id == id)
                .or(self.screen_at_pointer()?),
            None => self.screen_at_pointer()?,
        };
        let Some(target) = target.or_else(|| self.screens().ok().and_then(|s| s.into_iter().next()))
        else {
            return Err(RuntimeError::internal("no screen available for the panel"));
        };

        let frame = frame_for(&target, config);
        window.setFrame_display(frame, true);

        match activation {
            // `orderFrontRegardless` shows the panel without activating the
            // application, which is what keeps the editor's focus intact.
            PanelActivation::Passive => window.orderFrontRegardless(),
            PanelActivation::Focused => {
                window.makeKeyAndOrderFront(None);
            }
        }
        Ok(())
    }

    unsafe fn hide_panel(&self, handle: RawWindow) -> Result<()> {
        let window = unsafe { Self::window(handle)? };
        window.orderOut(None);
        Ok(())
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

/// Where the panel sits on a screen.
///
/// Cocoa's origin is bottom-left, so a vertically centred panel is placed from
/// the bottom up rather than the top down.
fn frame_for(screen: &ScreenInfo, config: &PanelConfig) -> NSRect {
    let width = config.width as f64;
    let height = (screen.height * config.height_ratio.clamp(0.1, 1.0)).min(screen.height);
    let y = screen.y + (screen.height - height) / 2.0;

    let x = match config.edge {
        ScreenEdge::Right => screen.x + screen.width - width,
        ScreenEdge::Left => screen.x,
    };

    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
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
    fn the_right_edge_puts_the_panel_flush_against_it() {
        let config = PanelConfig {
            edge: ScreenEdge::Right,
            width: 300,
            ..PanelConfig::default()
        };
        let frame = frame_for(&screen(), &config);

        assert_eq!(frame.origin.x + frame.size.width, 1440.0);
        assert_eq!(frame.size.width, 300.0);
    }

    #[test]
    fn the_left_edge_starts_at_the_screen_origin() {
        let config = PanelConfig {
            edge: ScreenEdge::Left,
            ..PanelConfig::default()
        };
        assert_eq!(frame_for(&screen(), &config).origin.x, 0.0);
    }

    #[test]
    fn the_panel_is_centred_vertically_and_never_taller_than_the_screen() {
        let config = PanelConfig {
            height_ratio: 0.8,
            ..PanelConfig::default()
        };
        let frame = frame_for(&screen(), &config);
        assert_eq!(frame.size.height, 720.0);
        assert_eq!(frame.origin.y, 90.0);

        let oversized = PanelConfig {
            height_ratio: 2.0,
            ..PanelConfig::default()
        };
        assert_eq!(frame_for(&screen(), &oversized).size.height, 900.0);
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
        let frame = frame_for(&secondary, &config);
        assert_eq!(frame.origin.x, 1440.0 + 2560.0 - 320.0);
    }
}

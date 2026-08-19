//! The desktop app.
//!
//! Two windows over one daemon: a main window for working through a project,
//! and an edge-docked panel for the far more common case of glancing at what is
//! running and starting or stopping one thing.
//!
//! The app itself is an accessory — no Dock icon, no place in ⌘-Tab. It is a
//! control plane that should be reachable in a keystroke, not something the
//! user switches to.

mod commands;
mod daemon;
mod panel;

use std::sync::Arc;

use daemon::DaemonHandle;
use panel::PanelController;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

/// The channel the frontend listens on for daemon events.
const EVENT_CHANNEL: &str = "runtime://event";

/// Chosen for a low chance of collision; configurable from the panel.
const DEFAULT_SHORTCUT: &str = "CmdOrCtrl+Alt+L";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(DaemonHandle::new())
        .manage(Arc::new(PanelController::new()))
        .setup(|app| {
            let handle = app.handle().clone();

            // No Dock icon: closing the main window leaves the tray and the
            // panel, which is the intended resting state.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            build_tray(&handle)?;
            forward_events(handle.clone());

            if let Err(err) = panel::adopt(&handle) {
                // A missing panel must not take the whole app down; the main
                // window and the tray still work.
                tracing::warn!(%err, "the edge panel is unavailable on this platform");
            } else {
                let controller = app.state::<Arc<PanelController>>().inner().clone();
                controller.watch_edge(&handle);
                register_shortcut(&handle);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_projects,
            commands::discover_projects,
            commands::add_project,
            commands::remove_project,
            commands::list_worktrees,
            commands::get_service,
            commands::start_service,
            commands::stop_service,
            commands::restart_service,
            commands::get_logs,
            commands::get_health,
            commands::list_ports,
            commands::check_port,
            commands::daemon_info,
            commands::get_panel_config,
            commands::set_panel_config,
            commands::hide_panel,
            commands::open_main_window,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start the desktop app");
}

/// Register the global shortcut that summons the panel.
///
/// A failure here is not fatal: the shortcut may already be taken by another
/// app, and the tray and edge hover still work.
fn register_shortcut(app: &AppHandle) {
    use tauri_plugin_global_shortcut::{Builder as ShortcutBuilder, ShortcutState};

    let plugin = ShortcutBuilder::new()
        .with_handler(|app, _shortcut, event| {
            // Fire on press, not release, or the panel toggles twice.
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let controller = app.state::<Arc<PanelController>>().inner().clone();
            if let Err(err) = controller.toggle(app) {
                tracing::warn!(%err, "could not toggle the panel");
            }
        })
        .build();

    if let Err(err) = app.plugin(plugin) {
        tracing::warn!(%err, "global shortcuts are unavailable");
        return;
    }

    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    if let Err(err) = app.global_shortcut().register(DEFAULT_SHORTCUT) {
        tracing::warn!(%err, shortcut = DEFAULT_SHORTCUT, "could not register the shortcut");
    }
}

/// Menu-bar item on macOS, system tray on Windows.
///
/// A left click opens the panel — the common case — while the menu behind a
/// right click keeps the main window and quitting reachable.
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open main window", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Local Runtime")
        .on_tray_icon_event(|tray, event| {
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                let controller = app.state::<Arc<PanelController>>().inner().clone();
                if let Err(err) = controller.toggle(app) {
                    tracing::warn!(%err, "could not toggle the panel");
                }
            }
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            // Quitting the app does not stop services — that is the daemon's
            // business, and it outlives this process on purpose.
            "quit" => app.exit(0),
            _ => {}
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

pub(crate) fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        // An accessory app has to ask to come forward; without this the window
        // appears behind whatever the user was in.
        #[cfg(target_os = "macos")]
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    }
}

/// Subscribe to the daemon's event stream and re-emit it to the frontend.
///
/// This is what makes a service started from the CLI or by an agent appear in
/// both windows without polling.
fn forward_events(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            match daemon::subscribe().await {
                Ok(mut client) => {
                    while let Ok(Some(event)) = client.next_event().await {
                        if app.emit(EVENT_CHANNEL, &event).is_err() {
                            return; // the app is shutting down
                        }
                    }
                    tracing::debug!("event stream ended; reconnecting");
                }
                Err(err) => tracing::debug!(%err, "cannot subscribe to the daemon"),
            }
            // The daemon may be restarting; retry rather than losing live
            // updates for the rest of the session.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

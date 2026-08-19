//! The desktop app.
//!
//! Phase 2 of the plan: main window, project sidebar, service list, port
//! status, start/stop/restart, logs and a tray. The edge-docked side panel is
//! Phase 4 and deliberately not here — the runtime is the product, the panel is
//! its appearance.

mod commands;
mod daemon;

use daemon::DaemonHandle;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

/// The channel the frontend listens on for daemon events.
const EVENT_CHANNEL: &str = "runtime://event";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(DaemonHandle::new())
        .setup(|app| {
            build_tray(app.handle())?;
            forward_events(app.handle().clone());
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
        ])
        .run(tauri::generate_context!())
        .expect("failed to start the desktop app");
}

/// Menu-bar item on macOS, system tray on Windows.
///
/// Closing the window leaves this behind: the daemon keeps running either way,
/// and the tray is how the user gets back to it.
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Open Local Runtime", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("Local Runtime")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
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

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Subscribe to the daemon's event stream and re-emit it to the frontend.
///
/// This is what makes a service started from the CLI or by an agent appear in
/// the window without polling.
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

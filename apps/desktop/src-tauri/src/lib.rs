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
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// The channel the frontend listens on for daemon events.
const EVENT_CHANNEL: &str = "runtime://event";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // A second launch — from the Dock while a copy is already running, say
        // — would put a second panel on the same screen edge, a second tray
        // icon, and fail to register the shortcut. Hand the request to the
        // instance that is already up instead.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .manage(DaemonHandle::new())
        .manage(Arc::new(PanelController::new()))
        .setup(|app| {
            let handle = app.handle().clone();

            // The policy follows the main window rather than being fixed, so
            // the window behaves the same however it was opened.
            sync_activation_policy(&handle);

            build_tray(&handle)?;
            keep_main_window_alive(&handle);
            forward_events(handle.clone());

            if let Err(err) = panel::adopt(&handle) {
                // A missing panel must not take the whole app down; the main
                // window and the tray still work.
                tracing::warn!(%err, "the edge panel is unavailable on this platform");
            } else {
                let controller = app.state::<Arc<PanelController>>().inner().clone();
                // Rest as a tab straight away: the panel is meant to be visible
                // from the moment the app starts, not discovered by accident.
                if let Err(err) = controller.rest(&handle) {
                    tracing::warn!(%err, "could not dock the panel");
                }
                controller.watch_edge(&handle);
                register_shortcut(&handle);
                restore_settings(handle.clone());
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
            commands::update_service,
            commands::add_service,
            commands::remove_service,
            commands::start_service,
            commands::adopt_port,
            commands::control_supervised,
            commands::stop_service,
            commands::restart_service,
            commands::get_logs,
            commands::get_health,
            commands::control_container,
            commands::list_ports,
            commands::check_port,
            commands::daemon_info,
            commands::get_panel_settings,
            commands::set_panel_settings,
            commands::list_screens,
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

    bind_shortcut(app, panel::DEFAULT_SHORTCUT);
}

fn bind_shortcut(app: &AppHandle, shortcut: &str) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    if let Err(err) = app.global_shortcut().register(shortcut) {
        // Another app may already own it. The tray and the screen edge still
        // work, so this is a degraded state rather than a failure.
        tracing::warn!(%err, shortcut, "could not register the shortcut");
    }
}

/// Move the global shortcut, keeping the old one only if the new one is refused.
pub(crate) fn rebind_shortcut(
    app: &AppHandle,
    previous: &str,
    next: &str,
) -> runtime_types::Result<()> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    app.global_shortcut()
        .register(next)
        .map_err(|err| runtime_types::RuntimeError::invalid(format!("{next} is unavailable: {err}")))?;
    let _ = app.global_shortcut().unregister(previous);
    Ok(())
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

/// Keep the activation policy in step with the main window.
///
/// macOS withholds full-screen support from accessory apps, so the green button
/// only offers full screen while the app is `Regular`. Pinning the policy to
/// `Accessory` therefore made the window launched at startup behave differently
/// from the same window reopened from the tray, which switched to `Regular` on
/// the way.
///
/// Tying it to whether the main window is on screen gives that window the same
/// capabilities either way, and still leaves no Dock icon once it is closed.
fn sync_activation_policy(app: &AppHandle) {
    let _ = app;
    #[cfg(target_os = "macos")]
    {
        let visible = app
            .get_webview_window("main")
            .and_then(|window| window.is_visible().ok())
            .unwrap_or(false);
        let policy = if visible {
            tauri::ActivationPolicy::Regular
        } else {
            tauri::ActivationPolicy::Accessory
        };
        let _ = app.set_activation_policy(policy);
    }
}

/// Closing the main window hides it instead of destroying it.
///
/// Without this the window is gone for good after the first close: Tauri
/// destroys it, `get_webview_window` returns `None`, and the tray's "Open main
/// window" silently does nothing. Hiding is also what a menu-bar app should do
/// — the app has not quit, it has gone back to resting.
fn keep_main_window_alive(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Some(window) = handle.get_webview_window("main") {
                let _ = window.hide();
            }
            // Back to a menu-bar app: the Dock icon should not outlive the
            // window that justified it.
            sync_activation_policy(&handle);
        }
    });
}

pub(crate) fn show_main_window(app: &AppHandle) {
    // Policy first. An accessory app cannot come to the front, so focusing
    // before switching leaves the window behind whatever the user was in — the
    // reason opening from the tray felt different from launching the app.
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);

    let window = match app.get_webview_window("main") {
        Some(window) => Some(window),
        // Defensive: if the window was destroyed anyway, rebuild it rather
        // than leaving the menu item dead.
        None => match WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title("Local Runtime")
            .inner_size(1120.0, 720.0)
            .min_inner_size(760.0, 480.0)
            .build()
        {
            Ok(window) => {
                keep_main_window_alive(app);
                Some(window)
            }
            Err(err) => {
                tracing::error!(%err, "could not recreate the main window");
                None
            }
        },
    };

    if let Some(window) = window {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Load the stored panel settings once the daemon is reachable.
///
/// Asynchronous because it needs the daemon, which may still be starting; the
/// panel meanwhile rests with defaults rather than waiting for it.
fn restore_settings(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let handle = app.state::<DaemonHandle>().inner().clone();
        let request = runtime_ipc::protocol::Request::GetSetting {
            key: panel::SETTINGS_KEY.to_string(),
        };
        let Ok(runtime_ipc::protocol::ResponseBody::Setting { value: Some(raw) }) =
            handle.call(request).await
        else {
            return;
        };
        let Ok(settings) = serde_json::from_str::<panel::PanelSettings>(&raw) else {
            tracing::warn!("stored panel settings are unreadable; keeping defaults");
            return;
        };

        let controller = app.state::<Arc<PanelController>>().inner().clone();
        if settings.shortcut != panel::DEFAULT_SHORTCUT {
            let _ = rebind_shortcut(&app, panel::DEFAULT_SHORTCUT, &settings.shortcut);
        }
        controller.load(settings.clone());
        if let Err(err) = controller.set_config(&app, settings.config) {
            tracing::warn!(%err, "could not apply stored panel settings");
        }
    });
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

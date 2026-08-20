//! Tauri commands.
//!
//! Each one is a direct translation of an IPC request. The app deliberately
//! adds no logic of its own: anything computed here would be something the CLI
//! and MCP do not get.

use runtime_core::discover::Discovery;
use runtime_types::{ContainerView, ServicePatch};
use runtime_ipc::protocol::{Request, ResponseBody};
use runtime_types::{
    AdoptOutcome, DaemonInfo, HealthReport, LogLine, PortOwner, PortStatus, ProjectView,
    ServiceView, StartOutcome, SupervisedView, Workspace,
};
use tauri::State;

use crate::daemon::DaemonHandle;

/// Errors reach the UI as strings; the structured variants matter to the
/// protocol, not to a toast.
type CmdResult<T> = std::result::Result<T, String>;

async fn call(state: &State<'_, DaemonHandle>, request: Request) -> CmdResult<ResponseBody> {
    state.call(request).await.map_err(|err| err.to_string())
}

/// Every command knows exactly which variant it expects, so a mismatch is a
/// protocol bug worth reporting rather than an empty screen.
fn unexpected(response: &ResponseBody) -> String {
    format!("unexpected response from the daemon: {response:?}")
}

#[tauri::command]
pub async fn list_projects(state: State<'_, DaemonHandle>) -> CmdResult<Vec<ProjectView>> {
    match call(&state, Request::ListProjects).await? {
        ResponseBody::Projects { items } => Ok(items),
        other => Err(unexpected(&other)),
    }
}

/// Find projects without the user having to name them.
///
/// The empty state calls this automatically: being told to register your own
/// projects by hand is exactly the friction this tool exists to remove.
#[tauri::command]
pub async fn discover_projects(
    state: State<'_, DaemonHandle>,
    paths: Vec<String>,
    adopt: bool,
) -> CmdResult<Vec<Discovery>> {
    let request = Request::DiscoverProjects {
        paths: paths.into_iter().map(Into::into).collect(),
        adopt,
    };
    match call(&state, request).await? {
        ResponseBody::Discoveries { items } => Ok(items),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn add_project(
    state: State<'_, DaemonHandle>,
    path: String,
    name: Option<String>,
) -> CmdResult<ProjectView> {
    let request = Request::AddProject {
        path: path.into(),
        name,
    };
    match call(&state, request).await? {
        ResponseBody::Project(view) => Ok(view),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn remove_project(state: State<'_, DaemonHandle>, selector: String) -> CmdResult<bool> {
    match call(&state, Request::RemoveProject { selector }).await? {
        ResponseBody::Done { ok } => Ok(ok),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn list_worktrees(
    state: State<'_, DaemonHandle>,
    selector: String,
) -> CmdResult<Vec<Workspace>> {
    match call(&state, Request::ListWorktrees { selector }).await? {
        ResponseBody::Workspaces { items } => Ok(items),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn get_service(
    state: State<'_, DaemonHandle>,
    service: String,
) -> CmdResult<ServiceView> {
    let request = Request::GetService {
        project: None,
        service,
    };
    match call(&state, request).await? {
        ResponseBody::Service(view) => Ok(view),
        other => Err(unexpected(&other)),
    }
}

/// Correct how a service starts.
///
/// Detection guesses, and the guess is often close but wrong — a default port
/// the project does not use, the `dev` script where `dev:local` is the one that
/// works. Without this the only remedy is the CLI.
#[tauri::command]
pub async fn update_service(
    state: State<'_, DaemonHandle>,
    service: String,
    patch: ServicePatch,
) -> CmdResult<ServiceView> {
    let request = Request::UpdateService {
        project: None,
        service,
        patch,
    };
    match call(&state, request).await? {
        ResponseBody::Service(view) => Ok(view),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn add_service(
    state: State<'_, DaemonHandle>,
    project: String,
    name: String,
    command: String,
    port: Option<u16>,
    cwd: Option<String>,
) -> CmdResult<ServiceView> {
    let request = Request::AddService {
        selector: project,
        name,
        config: runtime_types::ServiceConfig {
            command,
            port,
            cwd: cwd.map(Into::into),
            service_type: None,
            env: Default::default(),
            health: None,
            auto_start: false,
            on_conflict: None,
            depends_on: Vec::new(),
            one_shot: false,
        },
    };
    match call(&state, request).await? {
        ResponseBody::Service(view) => Ok(view),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn remove_service(state: State<'_, DaemonHandle>, service: String) -> CmdResult<bool> {
    let request = Request::RemoveService {
        project: None,
        service,
    };
    match call(&state, request).await? {
        ResponseBody::Done { ok } => Ok(ok),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn start_service(
    state: State<'_, DaemonHandle>,
    service: String,
) -> CmdResult<StartOutcome> {
    let request = Request::StartService {
        project: None,
        service,
        port: None,
        on_conflict: None,
        // Anything started from this window is attributed to the desktop app,
        // which is what makes the ownership column meaningful.
        started_by: Some("desktop".to_string()),
        session: None,
    };
    match call(&state, request).await? {
        ResponseBody::Started(outcome) => Ok(outcome),
        other => Err(unexpected(&other)),
    }
}

/// Declare whatever is on a port so the runtime can start it again.
///
/// Refused by the daemon when another supervisor is keeping it alive, unless
/// `force` — taking a service away from PM2 means deleting it there, which
/// usually changes what starts at boot.
#[tauri::command]
pub async fn adopt_port(
    state: State<'_, DaemonHandle>,
    port: u16,
    force: bool,
) -> CmdResult<AdoptOutcome> {
    match call(&state, Request::AdoptPort { port, force }).await? {
        ResponseBody::Adopted(outcome) => Ok(outcome),
        other => Err(unexpected(&other)),
    }
}

/// Switch an entry another supervisor keeps.
///
/// PM2 still owns what it is and whether it starts at boot; this owns whether
/// it is running now. Only start, stop and restart — deleting an entry is what
/// stops it coming back, and is not offered.
#[tauri::command]
pub async fn control_supervised(
    state: State<'_, DaemonHandle>,
    name: String,
    action: String,
) -> CmdResult<SupervisedView> {
    match call(&state, Request::ControlSupervised { name, action }).await? {
        ResponseBody::Supervised(view) => Ok(view),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn stop_service(
    state: State<'_, DaemonHandle>,
    service: String,
) -> CmdResult<ServiceView> {
    let request = Request::StopService {
        project: None,
        service,
        timeout_seconds: None,
    };
    match call(&state, request).await? {
        ResponseBody::Service(view) => Ok(view),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn restart_service(
    state: State<'_, DaemonHandle>,
    service: String,
) -> CmdResult<StartOutcome> {
    let request = Request::RestartService {
        project: None,
        service,
        started_by: Some("desktop".to_string()),
        session: None,
    };
    match call(&state, request).await? {
        ResponseBody::Started(outcome) => Ok(outcome),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn get_logs(
    state: State<'_, DaemonHandle>,
    service: String,
    max_lines: Option<usize>,
    since_seq: Option<u64>,
) -> CmdResult<Vec<LogLine>> {
    let request = Request::GetLogs {
        project: None,
        service,
        max_lines,
        since_seq,
    };
    match call(&state, request).await? {
        ResponseBody::Logs { items } => Ok(items),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn get_health(
    state: State<'_, DaemonHandle>,
    service: String,
) -> CmdResult<HealthReport> {
    let request = Request::GetHealth {
        project: None,
        service,
    };
    match call(&state, request).await? {
        ResponseBody::Health(report) => Ok(report),
        other => Err(unexpected(&other)),
    }
}

/// Switch a container on or off.
///
/// Offered for containers the runtime did not create because `docker stop` is
/// a graceful operation on a named, restartable object — unlike signalling a
/// pid, which is why processes started elsewhere have no such button.
#[tauri::command]
pub async fn control_container(
    state: State<'_, DaemonHandle>,
    name: String,
    action: String,
) -> CmdResult<ContainerView> {
    match call(&state, Request::ControlContainer { name, action }).await? {
        ResponseBody::Container(view) => Ok(view),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn list_ports(state: State<'_, DaemonHandle>) -> CmdResult<Vec<PortOwner>> {
    match call(&state, Request::ListPorts).await? {
        ResponseBody::Ports { items } => Ok(items),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn check_port(state: State<'_, DaemonHandle>, port: u16) -> CmdResult<PortStatus> {
    match call(&state, Request::CheckPort { port }).await? {
        ResponseBody::Port(status) => Ok(status),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub async fn daemon_info(state: State<'_, DaemonHandle>) -> CmdResult<DaemonInfo> {
    match call(&state, Request::DaemonInfo).await? {
        ResponseBody::Info(info) => Ok(info),
        other => Err(unexpected(&other)),
    }
}

// ---- panel ---------------------------------------------------------------

use std::sync::Arc;

use runtime_adapter::ScreenInfo;
use tauri::AppHandle;

use crate::panel;
use crate::panel::{PanelController, PanelSettings};

/// Panel settings, as stored by the daemon.
#[tauri::command]
pub async fn get_panel_settings(
    state: State<'_, DaemonHandle>,
    controller: State<'_, Arc<PanelController>>,
) -> CmdResult<PanelSettings> {
    // The daemon is the record; the controller is a live copy of it.
    match call(&state, Request::GetSetting { key: panel::SETTINGS_KEY.to_string() }).await? {
        ResponseBody::Setting { value: Some(raw) } => match serde_json::from_str(&raw) {
            Ok(settings) => Ok(settings),
            // A stored blob from an older layout should not brick the screen.
            Err(err) => {
                tracing::warn!(%err, "stored panel settings are unreadable; using defaults");
                Ok(controller.settings())
            }
        },
        _ => Ok(controller.settings()),
    }
}

#[tauri::command]
pub async fn set_panel_settings(
    app: AppHandle,
    state: State<'_, DaemonHandle>,
    controller: State<'_, Arc<PanelController>>,
    settings: PanelSettings,
) -> CmdResult<()> {
    let previous = controller.settings();
    controller.set_screen(settings.screen.clone());

    // Re-registering only when it changed avoids a window where no shortcut is
    // registered at all.
    if settings.shortcut != previous.shortcut {
        crate::rebind_shortcut(&app, &previous.shortcut, &settings.shortcut)
            .map_err(|err| err.to_string())?;
        controller.set_shortcut(settings.shortcut.clone());
    }

    controller
        .set_config(&app, settings.config.clone())
        .map_err(|err| err.to_string())?;

    let raw = serde_json::to_string(&settings).map_err(|err| err.to_string())?;
    call(
        &state,
        Request::SetSetting {
            key: panel::SETTINGS_KEY.to_string(),
            value: raw,
        },
    )
    .await?;
    Ok(())
}

/// Screens the panel can be docked to, for the settings screen.
#[tauri::command]
pub fn list_screens() -> Vec<ScreenInfo> {
    panel::screens()
}

/// Collapse the panel from inside it — Escape, or after an action completes.
#[tauri::command]
pub fn hide_panel(app: AppHandle, controller: State<'_, Arc<PanelController>>) -> CmdResult<()> {
    controller.collapse(&app).map_err(|err| err.to_string())
}

/// Jump from the panel to the full window, for logs and anything else the
/// compact view deliberately leaves out.
#[tauri::command]
pub fn open_main_window(app: AppHandle) {
    crate::show_main_window(&app);
}

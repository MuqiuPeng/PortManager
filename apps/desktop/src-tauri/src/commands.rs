//! Tauri commands.
//!
//! Each one is a direct translation of an IPC request. The app deliberately
//! adds no logic of its own: anything computed here would be something the CLI
//! and MCP do not get.

use runtime_core::discover::Discovery;
use runtime_ipc::protocol::{Request, ResponseBody};
use runtime_types::{
    DaemonInfo, HealthReport, LogLine, PortOwner, PortStatus, ProjectView, ServiceView,
    StartOutcome, Workspace,
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

use runtime_adapter::PanelConfig;
use tauri::AppHandle;

use crate::panel::PanelController;

#[tauri::command]
pub fn get_panel_config(controller: State<'_, Arc<PanelController>>) -> PanelConfig {
    controller.config()
}

#[tauri::command]
pub fn set_panel_config(
    app: AppHandle,
    controller: State<'_, Arc<PanelController>>,
    config: PanelConfig,
) -> CmdResult<()> {
    controller
        .set_config(&app, config)
        .map_err(|err| err.to_string())
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

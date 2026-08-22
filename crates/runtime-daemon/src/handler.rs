//! Request dispatch.
//!
//! Every arm is a thin translation from the protocol to a `Runtime` call. Any
//! logic that appears here rather than in the core would be logic the desktop
//! app does not get, so there should never be much.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use runtime_core::lifecycle::{StartOptions, GRACEFUL_TIMEOUT};
use runtime_core::Runtime;
use runtime_ipc::protocol::{Request, ResponseBody, PROTOCOL_VERSION};
use runtime_types::{
    AgentSession, ConflictPolicy, Project, Result, RuntimeError, Service, SessionId, StartedBy,
};

/// Default cap on log lines returned in one call, low enough that an agent can
/// read the result without burning its context.
const DEFAULT_LOG_LINES: usize = 100;

const DEFAULT_HEALTH_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Dispatcher {
    runtime: Arc<Runtime>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl Dispatcher {
    pub fn new(runtime: Arc<Runtime>, shutdown: tokio::sync::watch::Sender<bool>) -> Self {
        Self { runtime, shutdown }
    }

    pub fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    pub async fn dispatch(&self, request: Request) -> Result<ResponseBody> {
        let runtime = &self.runtime;
        match request {
            Request::Ping => Ok(ResponseBody::Pong {
                protocol_version: PROTOCOL_VERSION,
            }),

            Request::DaemonInfo => Ok(ResponseBody::Info(runtime.info()?)),

            Request::Shutdown => {
                let _ = self.shutdown.send(true);
                Ok(ResponseBody::Done { ok: true })
            }

            // ---- projects ----------------------------------------------

            Request::ListProjects => Ok(ResponseBody::Projects { items: runtime.list_projects()? }),

            Request::DiscoverProjects { paths, adopt } => {
                if adopt {
                    // Report the whole picture afterwards, not just the new
                    // rows, so the caller sees the resulting state.
                    runtime.adopt_discovered(&paths)?;
                }
                Ok(ResponseBody::Discoveries {
                    items: runtime.discover_projects(&paths)?,
                })
            }

            Request::GetProject { selector } => {
                let project = runtime.resolve_project(&selector)?;
                Ok(ResponseBody::Project(runtime.project_view(&project)?))
            }

            Request::AddProject { path, name } => {
                Ok(ResponseBody::Project(runtime.add_project(path, name)?))
            }

            Request::RemoveProject { selector } => {
                let project = runtime.resolve_project(&selector)?;
                Ok(ResponseBody::Done {
                    ok: runtime.remove_project(&project.id)?,
                })
            }

            Request::ListWorktrees { selector } => {
                let project = runtime.resolve_project(&selector)?;
                // Refresh first: a worktree created since the last call should
                // appear without the user having to register it by hand.
                runtime.sync_worktrees(&project.id)?;
                Ok(ResponseBody::Workspaces {
                    items: runtime.store().list_workspaces(&project.id)?,
                })
            }

            Request::RegisterWorktree { selector, path } => {
                let project = runtime.resolve_project(&selector)?;
                Ok(ResponseBody::Workspace(
                    runtime.register_worktree(&project.id, &path)?,
                ))
            }

            // ---- services ----------------------------------------------

            Request::ListServices { project } => {
                let views = match project {
                    Some(selector) => {
                        let project = runtime.resolve_project(&selector)?;
                        runtime
                            .project_view(&project)?
                            .workspaces
                            .into_iter()
                            .flat_map(|workspace| workspace.services)
                            .collect()
                    }
                    None => runtime.list_all_services()?,
                };
                Ok(ResponseBody::Services { items: views })
            }

            Request::GetService { project, service } => {
                let service = self.resolve_service(project.as_deref(), &service)?;
                Ok(ResponseBody::Service(runtime.service_view(&service)?))
            }

            Request::UpdateService {
                project,
                service,
                patch,
            } => {
                let service = self.resolve_service(project.as_deref(), &service)?;
                let updated = runtime.update_service(&service.id, patch)?;
                Ok(ResponseBody::Service(runtime.service_view(&updated)?))
            }

            Request::AddService {
                selector,
                name,
                config,
            } => {
                let project = runtime.resolve_project(&selector)?;
                let workspace = runtime
                    .store()
                    .list_workspaces(&project.id)?
                    .into_iter()
                    .find(|workspace| !workspace.worktree)
                    .ok_or_else(|| RuntimeError::not_found("workspace", selector.as_str()))?;

                let cwd = match config.cwd {
                    Some(cwd) if cwd.is_absolute() => cwd,
                    Some(cwd) => workspace.path.join(cwd),
                    None => workspace.path.clone(),
                };
                let service_type = config.service_type.unwrap_or_else(|| {
                    // Inferred rather than defaulted: `Custom` is the right
                    // answer for something unrecognised, not for something
                    // nobody was asked about — and the type now decides how
                    // the service is checked.
                    runtime_core::detect::guess_type(&name, &config.command)
                });
                let service = Service {
                    id: runtime_types::ServiceId::new(),
                    workspace_id: workspace.id.clone(),
                    name,
                    service_type,
                    command: config.command,
                    cwd,
                    env: config.env,
                    preferred_port: config.port,
                    health_check: config.health,
                    auto_start: config.auto_start,
                    conflict_policy: config.on_conflict.unwrap_or_default(),
                    depends_on: config.depends_on,
                    one_shot: config.one_shot,
                };
                let created = runtime.add_service(&workspace.id, service)?;
                Ok(ResponseBody::Service(runtime.service_view(&created)?))
            }

            Request::RemoveService { project, service } => {
                let service = self.resolve_service(project.as_deref(), &service)?;
                Ok(ResponseBody::Done {
                    ok: runtime.delete_service(&service.id)?,
                })
            }

            Request::ExportConfig { selector } => {
                let project = runtime.resolve_project(&selector)?;
                Ok(ResponseBody::Config(runtime.export_config(&project.id)?))
            }

            Request::RecordLaunch { command, cwd, source, session } => {
                // Never an error the caller has to handle: this runs on the hot
                // path of somebody else's shell command, and a runtime that
                // cannot take a note must not be able to stop a developer from
                // working.
                let source = source.as_deref().map(StartedBy::parse).unwrap_or_default();
                runtime.record_launch(command, cwd, source, session);
                Ok(ResponseBody::Done { ok: true })
            }

            Request::ListLaunches => Ok(ResponseBody::Launches { items: runtime.launches() }),

            Request::ControlSupervised { name, action } => {
                let action = runtime_core::pm2::Pm2Action::parse(&action).ok_or_else(|| {
                    RuntimeError::invalid(format!(
                        "'{action}' is not one of start, stop, restart"
                    ))
                })?;
                Ok(ResponseBody::Supervised(
                    runtime.control_supervised(&name, action)?,
                ))
            }

            Request::ListStacks { selector } => {
                let workspace = self.primary_workspace(&selector)?;
                Ok(ResponseBody::Stacks { items: runtime.stack_views(&workspace.id)? })
            }
            Request::SetStack { selector, name, members } => {
                let workspace = self.primary_workspace(&selector)?;
                runtime.set_stack(&workspace.id, &name, members)?;
                Ok(ResponseBody::Stacks { items: runtime.stack_views(&workspace.id)? })
            }
            Request::RemoveStack { selector, name } => {
                let workspace = self.primary_workspace(&selector)?;
                Ok(ResponseBody::Done { ok: runtime.remove_stack(&workspace.id, &name)? })
            }
            Request::StopStack { selector, name } => {
                let workspace = self.workspace_to_run_in(&selector)?;
                Ok(ResponseBody::StackRun { done: runtime.stop_stack(&workspace.id, &name).await? })
            }

            Request::RunStack { selector, name } => {
                // Where it runs, not where it is declared.
                let workspace = self.workspace_to_run_in(&selector)?;
                Ok(ResponseBody::StackRun { done: runtime.run_stack(&workspace.id, &name).await? })
            }

            Request::Diagnose => Ok(ResponseBody::Findings { items: runtime.diagnose()? }),

            Request::ListFailures { detail_lines } => Ok(ResponseBody::Failures {
                // Enough to carry a stack's last frames without turning the
                // answer into the log it exists to save you reading.
                items: runtime.failures(if detail_lines == 0 { 8 } else { detail_lines })?,
            }),

            Request::AdoptPort { port, force } => {
                Ok(ResponseBody::Adopted(runtime.adopt_port(port, force)?))
            }

            Request::StartService {
                project,
                service,
                port,
                on_conflict,
                started_by,
                session,
            } => {
                let service = self.resolve_service(project.as_deref(), &service)?;
                // Somebody asked for this one by name, so the rule applies.
                // Members of a stack and things depended on are started
                // through the runtime directly and never reach here.
                runtime.require_in_a_stack(&service.id)?;
                let options = StartOptions {
                    started_by: started_by.as_deref().map(StartedBy::parse).unwrap_or_default(),
                    session: session.map(SessionId::from),
                    port,
                    conflict_policy: parse_policy(on_conflict.as_deref())?,
                };
                Ok(ResponseBody::Started(
                    runtime.start_service(&service.id, options).await?,
                ))
            }

            Request::StopService {
                project,
                service,
                timeout_seconds,
            } => {
                let service = self.resolve_service(project.as_deref(), &service)?;
                let timeout = timeout_seconds
                    .map(Duration::from_secs)
                    .unwrap_or(GRACEFUL_TIMEOUT);
                Ok(ResponseBody::Service(
                    runtime.stop_service(&service.id, timeout).await?,
                ))
            }

            Request::TakeOverService {
                project,
                service,
                timeout_seconds,
            } => {
                let service = self.resolve_service(project.as_deref(), &service)?;
                let timeout = timeout_seconds
                    .map(Duration::from_secs)
                    .unwrap_or(GRACEFUL_TIMEOUT);
                Ok(ResponseBody::Service(
                    runtime.take_over(&service.id, timeout).await?,
                ))
            }

            Request::RestartService {
                project,
                service,
                started_by,
                session,
            } => {
                let service = self.resolve_service(project.as_deref(), &service)?;
                // A restart ends with the service up, so it is a way of
                // starting it and answers to the same rule.
                runtime.require_in_a_stack(&service.id)?;
                let options = StartOptions {
                    started_by: started_by.as_deref().map(StartedBy::parse).unwrap_or_default(),
                    session: session.map(SessionId::from),
                    port: None,
                    conflict_policy: None,
                };
                Ok(ResponseBody::Started(
                    runtime.restart_service(&service.id, options).await?,
                ))
            }

            // ---- containers --------------------------------------------

            Request::ControlContainer { name, action } => {
                let action = match action.trim().to_ascii_lowercase().as_str() {
                    "start" => runtime_core::docker::ContainerAction::Start,
                    "stop" => runtime_core::docker::ContainerAction::Stop,
                    "restart" => runtime_core::docker::ContainerAction::Restart,
                    other => {
                        return Err(RuntimeError::invalid(format!(
                            "unknown container action '{other}'; expected start, stop or restart"
                        )))
                    }
                };
                Ok(ResponseBody::Container(
                    runtime.control_container(&name, action)?,
                ))
            }

            Request::GetContainerLogs { name, max_lines } => {
                let lines = runtime.container_logs(&name, max_lines.unwrap_or(DEFAULT_LOG_LINES))?;
                // Docker keeps its own timestamps and streams; passing the text
                // through unchanged is more honest than inventing metadata.
                Ok(ResponseBody::Logs {
                    items: lines
                        .into_iter()
                        .enumerate()
                        .map(|(index, message)| runtime_types::LogLine {
                            seq: index as u64,
                            service_id: runtime_types::ServiceId::from(name.as_str()),
                            stream: runtime_types::LogStream::Stdout,
                            timestamp: Utc::now(),
                            message,
                        })
                        .collect(),
                })
            }

            // ---- health ------------------------------------------------

            Request::GetHealth { project, service } => {
                let service = self.resolve_service(project.as_deref(), &service)?;
                Ok(ResponseBody::Health(runtime.health(&service.id).await?))
            }

            Request::WaitUntilHealthy {
                project,
                service,
                timeout_seconds,
            } => {
                let service = self.resolve_service(project.as_deref(), &service)?;
                let timeout = timeout_seconds
                    .map(Duration::from_secs)
                    .unwrap_or(DEFAULT_HEALTH_TIMEOUT);
                Ok(ResponseBody::Health(
                    runtime.wait_until_healthy(&service.id, timeout).await?,
                ))
            }

            // ---- ports -------------------------------------------------

            Request::CheckPort { port } => Ok(ResponseBody::Port(runtime.check_port(port)?)),

            Request::ListPorts => Ok(ResponseBody::Ports { items: runtime.list_ports()? }),

            Request::ReservePort {
                project,
                service,
                port,
                on_conflict,
                started_by,
            } => {
                let service = self.resolve_service(project.as_deref(), &service)?;
                let workspace = runtime.require_workspace(&service.workspace_id)?;
                let project = runtime.require_project(&workspace.project_id)?;
                let reservation = runtime.resolver().reserve(
                    &project,
                    &workspace,
                    &service,
                    port,
                    parse_policy(on_conflict.as_deref())?,
                    started_by.as_deref().map(StartedBy::parse).unwrap_or_default(),
                )?;
                Ok(ResponseBody::Reservation(reservation))
            }

            Request::ReleasePort { port } => Ok(ResponseBody::Done {
                ok: runtime.release_port(port)?,
            }),

            // ---- logs --------------------------------------------------

            Request::GetLogs {
                project,
                service,
                max_lines,
                since_seq,
            } => {
                let service = self.resolve_service(project.as_deref(), &service)?;
                Ok(ResponseBody::Logs {
                    items: runtime.read_logs(
                        &service.id,
                        max_lines.unwrap_or(DEFAULT_LOG_LINES),
                        since_seq,
                    )?,
                })
            }

            // ---- settings ----------------------------------------------

            Request::GetSetting { key } => Ok(ResponseBody::Setting {
                value: runtime.get_setting(&key)?,
            }),

            Request::SetSetting { key, value } => {
                runtime.set_setting(&key, &value)?;
                Ok(ResponseBody::Done { ok: true })
            }

            // ---- sessions ----------------------------------------------

            Request::ListSessions => Ok(ResponseBody::Sessions { items: runtime.store().list_sessions()? }),

            Request::RegisterSession {
                provider,
                client,
                cwd,
            } => {
                let now = Utc::now();
                // Associating the session with a project up front is what lets
                // the GUI later say "started by Claude Code" against a branch.
                let project_id = cwd
                    .as_ref()
                    .and_then(|path| runtime.resolve_project(&path.to_string_lossy()).ok())
                    .map(|project| project.id);

                let session = AgentSession {
                    id: SessionId::new(),
                    provider,
                    client,
                    cwd,
                    project_id,
                    started_at: now,
                    last_seen_at: now,
                };
                runtime.store().upsert_session(&session)?;
                Ok(ResponseBody::Session(session))
            }

            // Handled by the connection loop, which needs the socket itself.
            Request::Subscribe => Ok(ResponseBody::Done { ok: true }),
        }
    }

    /// The checkout a stack is *declared* in.
    ///
    /// The main one, not a worktree: a stack names services by the names they
    /// have in the project, and every worktree has the same set under different
    /// ports. Declaring one per worktree would multiply the same stack by the
    /// number of branches somebody happens to have checked out.
    fn primary_workspace(&self, selector: &str) -> Result<runtime_types::Workspace> {
        let project = self.runtime.resolve_project(selector)?;
        self.runtime
            .store()
            .list_workspaces(&project.id)?
            .into_iter()
            .find(|workspace| !workspace.worktree)
            .ok_or_else(|| {
                RuntimeError::invalid(format!("'{}' has no main checkout", project.name))
            })
    }

    /// The checkout a stack should *run* in.
    ///
    /// Declared once for the project, run wherever the caller is standing —
    /// which for a worktree is the whole point: two branches served at once,
    /// each on its own ports, from one definition. A selector that names no
    /// particular checkout gets the main one.
    fn workspace_to_run_in(&self, selector: &str) -> Result<runtime_types::Workspace> {
        let project = self.runtime.resolve_project(selector)?;
        let workspaces = self.runtime.store().list_workspaces(&project.id)?;

        if let Ok(path) = std::fs::canonicalize(std::path::Path::new(selector)) {
            if let Some(found) = workspaces
                .iter()
                .filter(|workspace| path.starts_with(&workspace.path))
                .max_by_key(|workspace| workspace.path.components().count())
            {
                return Ok(found.clone());
            }
        }

        workspaces
            .into_iter()
            .find(|workspace| !workspace.worktree)
            .ok_or_else(|| {
                RuntimeError::invalid(format!("'{}' has no main checkout", project.name))
            })
    }

    fn resolve_service(&self, project: Option<&str>, selector: &str) -> Result<Service> {
        // A selector that is a path names one checkout. Resolving only as far
        // as its project would send the request to whichever checkout came
        // first, which for a repository cloned twice is the other one.
        let workspace = project
            .and_then(|selector| self.runtime.workspace_for_selector(selector).ok().flatten());
        let project: Option<Project> = project
            .map(|selector| self.runtime.resolve_project(selector))
            .transpose()?;
        self.runtime
            .resolve_service_in(project.as_ref(), workspace.as_ref(), selector)
    }
}

fn parse_policy(raw: Option<&str>) -> Result<Option<ConflictPolicy>> {
    match raw {
        None => Ok(None),
        Some(value) => ConflictPolicy::parse(value).map(Some).ok_or_else(|| {
            RuntimeError::invalid(format!(
                "unknown conflict policy '{value}'; expected reuse, allocate-next, fail, ask or kill-existing"
            ))
        }),
    }
}

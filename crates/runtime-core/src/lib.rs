//! The runtime core.
//!
//! Everything the product does that is not a user interface lives here:
//! registry, port leases, process lifecycle, logs, health and reconciliation.
//! The desktop app, the CLI and the MCP server are all thin callers of this
//! type — which is what keeps the three from drifting into three behaviours.

pub mod detect;
pub mod discover;
pub mod docker;
pub mod events;
pub mod git;
pub mod health;
pub mod lifecycle;
pub mod logs;
pub mod paths;
pub mod platform;
pub mod ports;
pub mod store;
pub mod supervisor;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use runtime_adapter::{PlatformAdapter, ProcessIdentity};
use runtime_types::{
    DaemonInfo, ExternalService, LogLine, PortOwner, PortStatus, Project, ProjectId, ProjectView,
    Result, RuntimeError, RuntimeInstance, Service, ServiceId, ServiceStatus, ServiceView,
    Workspace, WorkspaceId, WorkspaceView,
};

use crate::docker::Docker;
use crate::events::{EventBus, RuntimeEvent};
use crate::logs::LogStore;
use crate::ports::PortResolver;
use crate::store::Store;
use crate::supervisor::Supervisor;

pub struct Runtime {
    adapter: Arc<dyn PlatformAdapter>,
    store: Arc<Store>,
    logs: Arc<LogStore>,
    supervisor: Arc<Supervisor>,
    docker: Arc<Docker>,
    /// Resolving one port walks the process table, so answering "what is
    /// listening" for a whole machine would do it dozens of times over.
    port_owners: Mutex<Option<(Instant, Vec<PortOwner>)>>,
    events: EventBus,
    started_at: Instant,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("platform", &self.adapter.name())
            .field("store", &self.store)
            .finish()
    }
}

impl Runtime {
    /// Open the runtime against the default data directory.
    pub fn open_default() -> Result<Self> {
        paths::ensure_data_dir()?;
        Self::open(paths::database_path()?)
    }

    pub fn open(database_path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::with_parts(
            platform::current(),
            Arc::new(Store::open(database_path)?),
        ))
    }

    /// An ephemeral runtime, used by tests.
    pub fn in_memory() -> Result<Self> {
        Ok(Self::with_parts(
            platform::current(),
            Arc::new(Store::open_in_memory()?),
        ))
    }

    pub fn with_parts(adapter: Arc<dyn PlatformAdapter>, store: Arc<Store>) -> Self {
        Self {
            adapter,
            store,
            logs: Arc::new(LogStore::default()),
            supervisor: Arc::new(Supervisor::new()),
            docker: Arc::new(Docker::new()),
            port_owners: Mutex::new(None),
            events: EventBus::new(),
            started_at: Instant::now(),
        }
    }

    pub fn events(&self) -> &EventBus {
        &self.events
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn adapter(&self) -> &dyn PlatformAdapter {
        self.adapter.as_ref()
    }

    pub(crate) fn adapter_arc(&self) -> Arc<dyn PlatformAdapter> {
        Arc::clone(&self.adapter)
    }

    pub(crate) fn store_arc(&self) -> Arc<Store> {
        Arc::clone(&self.store)
    }

    pub(crate) fn logs_arc(&self) -> Arc<LogStore> {
        Arc::clone(&self.logs)
    }

    pub(crate) fn supervisor(&self) -> &Supervisor {
        &self.supervisor
    }

    pub(crate) fn supervisor_arc(&self) -> Arc<Supervisor> {
        Arc::clone(&self.supervisor)
    }

    pub fn resolver(&self) -> PortResolver<'_> {
        PortResolver::new(&self.store, self.adapter.as_ref(), &self.docker)
    }

    pub fn docker(&self) -> &Docker {
        &self.docker
    }

    pub fn info(&self) -> Result<DaemonInfo> {
        Ok(DaemonInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            socket_path: paths::socket_path()?,
            database_path: self.store.path().to_path_buf(),
            platform: self.adapter.name().to_string(),
            uptime_seconds: self.started_at.elapsed().as_secs(),
        })
    }

    // ---- projects ------------------------------------------------------

    /// Register a directory as a project, inferring its services.
    ///
    /// Pointing at a worktree registers the *primary* checkout as the project
    /// and the worktree as one of its workspaces, so three checkouts of one
    /// repository never become three unrelated projects.
    pub fn add_project(&self, path: impl AsRef<Path>, name: Option<String>) -> Result<ProjectView> {
        let path = canonicalize(path.as_ref())?;
        if !path.is_dir() {
            return Err(RuntimeError::invalid(format!(
                "{} is not a directory",
                path.display()
            )));
        }

        let git = git::info(&path);
        let root = git
            .as_ref()
            .map(|info| info.main_root.clone())
            .unwrap_or_else(|| path.clone());

        let detection = detect::detect(&root);
        let now = Utc::now();
        let project = match self.store.find_project_by_path(&root)? {
            Some(mut existing) => {
                if let Some(name) = name.clone() {
                    existing.name = name;
                }
                existing.updated_at = now;
                existing
            }
            None => Project {
                id: ProjectId::new(),
                name: name.clone().unwrap_or(detection.name.clone()),
                root_path: root.clone(),
                repository_url: git.as_ref().and_then(|info| info.remote_url.clone()),
                created_at: now,
                updated_at: now,
            },
        };
        self.store.upsert_project(&project)?;
        // Re-read so the id is the stored one even when the path already existed.
        let project = self
            .store
            .find_project_by_path(&root)?
            .ok_or_else(|| RuntimeError::internal("project vanished after insert"))?;

        let workspace = self.register_workspace(&project.id, &root)?;
        for detected in &detection.services {
            let service = Service {
                id: ServiceId::new(),
                workspace_id: workspace.id.clone(),
                name: detected.name.clone(),
                service_type: detected.service_type,
                command: detected.command.clone(),
                cwd: detected.cwd.clone().unwrap_or_else(|| workspace.path.clone()),
                env: Default::default(),
                preferred_port: detected.port,
                health_check: None,
                auto_start: false,
                conflict_policy: Default::default(),
            };
            self.store.upsert_service(&service)?;
        }

        // Existing worktrees become workspaces immediately, each with the same
        // services offset to its own port range.
        self.sync_worktrees(&project.id)?;

        self.events.publish(RuntimeEvent::ProjectAdded {
            project_id: project.id.clone(),
            name: project.name.clone(),
        });
        self.project_view(&project)
    }

    /// Add or refresh one checkout of a project.
    pub fn register_workspace(
        &self,
        project_id: &ProjectId,
        path: impl AsRef<Path>,
    ) -> Result<Workspace> {
        let path = canonicalize(path.as_ref())?;
        let git = git::info(&path);

        let workspace = match self.store.find_workspace_by_path(&path)? {
            Some(mut existing) => {
                existing.git_branch = git.as_ref().and_then(|info| info.branch.clone());
                existing.git_commit = git.as_ref().and_then(|info| info.commit.clone());
                existing
            }
            None => Workspace {
                id: WorkspaceId::new(),
                project_id: project_id.clone(),
                path: path.clone(),
                git_branch: git.as_ref().and_then(|info| info.branch.clone()),
                git_commit: git.as_ref().and_then(|info| info.commit.clone()),
                worktree: git.as_ref().is_some_and(|info| info.is_worktree),
                // Offsets are assigned once and never reused, which is what
                // makes a branch's ports stable across restarts.
                port_offset: self.store.next_port_offset(project_id)?,
                created_at: Utc::now(),
            },
        };
        self.store.upsert_workspace(&workspace)?;
        self.store
            .find_workspace_by_path(&path)?
            .ok_or_else(|| RuntimeError::internal("workspace vanished after insert"))
    }

    /// Discover git worktrees of a project and register any that are new,
    /// copying the primary checkout's services into each.
    pub fn sync_worktrees(&self, project_id: &ProjectId) -> Result<Vec<Workspace>> {
        let project = self.require_project(project_id)?;
        let entries = git::worktrees(&project.root_path)?;
        let mut result = Vec::new();

        let main_services = self
            .store
            .list_workspaces(project_id)?
            .into_iter()
            .find(|w| !w.worktree)
            .map(|w| self.store.list_services(&w.id))
            .transpose()?
            .unwrap_or_default();

        for entry in entries {
            if entry.is_main || !entry.path.exists() {
                continue;
            }
            // Worktrees under a hidden directory belong to a tool — Claude
            // Code keeps them in `.claude/worktrees` — not to the developer.
            // Registering them copies every service into a checkout that will
            // be deleted, dilutes the running count, and burns a port offset
            // that a real branch should have had.
            if discover::is_tool_managed_path(&entry.path) {
                tracing::debug!(path = %entry.path.display(), "skipping a tool-managed worktree");
                continue;
            }
            let known = self.store.find_workspace_by_path(&entry.path)?.is_some();
            let workspace = self.register_workspace(project_id, &entry.path)?;
            if !known {
                for template in &main_services {
                    let service = Service {
                        id: ServiceId::new(),
                        workspace_id: workspace.id.clone(),
                        cwd: workspace.path.clone(),
                        ..template.clone()
                    };
                    self.store.upsert_service(&service)?;
                }
            }
            result.push(workspace);
        }
        Ok(result)
    }

    /// Find projects on this machine without being told where they are.
    ///
    /// Always includes projects inferred from what is currently listening;
    /// `roots` adds directory trees to walk for projects that are stopped.
    pub fn discover_projects(&self, roots: &[PathBuf]) -> Result<Vec<discover::Discovery>> {
        discover::discover(&self.store, self.adapter.as_ref(), &self.docker, roots)
    }

    /// Register everything discovery found that is not registered yet.
    ///
    /// Returns the projects that were added. Registration only records what is
    /// already there — no process is started, stopped or otherwise touched.
    pub fn adopt_discovered(&self, roots: &[PathBuf]) -> Result<Vec<ProjectView>> {
        let mut added = Vec::new();
        for discovery in self.discover_projects(roots)? {
            if discovery.registered {
                continue;
            }
            match self.add_project(&discovery.root_path, None) {
                Ok(view) => added.push(view),
                // One unreadable directory should not abandon the whole sweep.
                Err(err) => tracing::warn!(
                    path = %discovery.root_path.display(),
                    %err,
                    "skipping a discovered project"
                ),
            }
        }
        Ok(added)
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectView>> {
        self.store
            .list_projects()?
            .iter()
            .map(|project| self.project_view(project))
            .collect()
    }

    pub fn get_project(&self, id: &ProjectId) -> Result<ProjectView> {
        let project = self.require_project(id)?;
        self.project_view(&project)
    }

    pub fn remove_project(&self, id: &ProjectId) -> Result<bool> {
        let removed = self.store.delete_project(id)?;
        if removed {
            self.events
                .publish(RuntimeEvent::ProjectRemoved { project_id: id.clone() });
        }
        Ok(removed)
    }

    /// Look a project up by id, exact name, or a path inside it.
    pub fn resolve_project(&self, selector: &str) -> Result<Project> {
        let projects = self.store.list_projects()?;
        if let Some(found) = projects.iter().find(|p| p.id.as_str() == selector) {
            return Ok(found.clone());
        }
        let by_name: Vec<&Project> = projects
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case(selector))
            .collect();
        match by_name.as_slice() {
            [only] => return Ok((*only).clone()),
            // Two checkouts of unrelated repositories can share a name. Picking
            // one silently is how an agent restarts the wrong project.
            [_, ..] => {
                let options = by_name
                    .iter()
                    .map(|p| p.root_path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(RuntimeError::invalid(format!(
                    "'{selector}' matches several projects: {options}. Use a path or an id."
                )));
            }
            [] => {}
        }
        if let Ok(path) = canonicalize(Path::new(selector)) {
            if let Some(found) = projects
                .iter()
                .filter(|p| path.starts_with(&p.root_path))
                .max_by_key(|p| p.root_path.components().count())
            {
                return Ok(found.clone());
            }
        }
        Err(RuntimeError::not_found("project", selector))
    }

    pub fn require_project(&self, id: &ProjectId) -> Result<Project> {
        self.store
            .get_project(id)?
            .ok_or_else(|| RuntimeError::not_found("project", id.as_str()))
    }

    pub fn require_workspace(&self, id: &WorkspaceId) -> Result<Workspace> {
        self.store
            .get_workspace(id)?
            .ok_or_else(|| RuntimeError::not_found("workspace", id.as_str()))
    }

    pub fn require_service(&self, id: &ServiceId) -> Result<Service> {
        self.store
            .get_service(id)?
            .ok_or_else(|| RuntimeError::not_found("service", id.as_str()))
    }

    // ---- services ------------------------------------------------------

    pub fn upsert_service(&self, service: &Service) -> Result<Service> {
        self.require_workspace(&service.workspace_id)?;
        self.store.upsert_service(service)?;
        Ok(service.clone())
    }

    pub fn delete_service(&self, id: &ServiceId) -> Result<bool> {
        self.store.release_leases_for_service(id)?;
        self.store.delete_service(id)
    }

    /// Every service across every project, with live state attached.
    pub fn list_all_services(&self) -> Result<Vec<ServiceView>> {
        self.store
            .all_services()?
            .iter()
            .map(|service| self.service_view(service))
            .collect()
    }

    /// Find a service by id, or by `name` within a project.
    ///
    /// When a project has several workspaces, a bare name resolves to the
    /// primary checkout; a worktree's service needs `branch/name`.
    pub fn resolve_service(&self, project: Option<&Project>, selector: &str) -> Result<Service> {
        if let Some(service) = self.store.get_service(&ServiceId::from(selector))? {
            return Ok(service);
        }

        // Split from the right: branches contain slashes, service names do not,
        // so `feature/refund/web` is the `web` service on `feature/refund`.
        let (branch, name) = match selector.rsplit_once('/') {
            Some((branch, name)) => (Some(branch), name),
            None => (None, selector),
        };

        let projects = match project {
            Some(project) => vec![project.clone()],
            None => self.store.list_projects()?,
        };

        let mut matches = Vec::new();
        for project in &projects {
            for workspace in self.store.list_workspaces(&project.id)? {
                if let Some(branch) = branch {
                    let workspace_branch = workspace.git_branch.as_deref().unwrap_or_default();
                    if !workspace_branch.eq_ignore_ascii_case(branch) {
                        continue;
                    }
                }
                for service in self.store.list_services(&workspace.id)? {
                    if service.name.eq_ignore_ascii_case(name) {
                        matches.push((workspace.clone(), service));
                    }
                }
            }
        }

        if matches.is_empty() {
            return Err(RuntimeError::not_found("service", selector));
        }

        // Preferring the primary checkout disambiguates `main` from a worktree
        // of the *same* project. Applying it across projects would silently
        // pick one of several unrelated services with the same name.
        let single_project = matches
            .iter()
            .all(|(workspace, _)| workspace.project_id == matches[0].0.project_id);
        if branch.is_none() && single_project {
            if let Some((_, service)) = matches.iter().find(|(workspace, _)| !workspace.worktree) {
                return Ok(service.clone());
            }
        }
        if matches.len() > 1 {
            let options = matches
                .iter()
                .map(|(workspace, service)| {
                    let project = self
                        .store
                        .get_project(&workspace.project_id)
                        .ok()
                        .flatten()
                        .map(|p| p.name)
                        .unwrap_or_default();
                    format!(
                        "{project}/{}/{}",
                        workspace.git_branch.as_deref().unwrap_or("-"),
                        service.name
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(RuntimeError::invalid(format!(
                "'{selector}' matches several services: {options}"
            )));
        }
        Ok(matches.remove(0).1)
    }

    // ---- views ---------------------------------------------------------

    pub fn project_view(&self, project: &Project) -> Result<ProjectView> {
        let owners = self.port_owners()?;
        let mut workspaces = Vec::new();
        let mut running = 0;
        let mut total = 0;
        let mut external_total = 0;

        for workspace in self.store.list_workspaces(&project.id)? {
            let mut services = Vec::new();
            for service in self.store.list_services(&workspace.id)? {
                let view = self.service_view_with(&service, &owners)?;
                total += 1;
                if view.status.is_live() {
                    running += 1;
                }
                services.push(view);
            }

            let external = self.external_services(&workspace, &services, &owners);
            external_total += external.len();
            workspaces.push(WorkspaceView {
                workspace,
                services,
                external,
            });
        }

        Ok(ProjectView {
            project: project.clone(),
            workspaces,
            running_services: running,
            total_services: total,
            external_services: external_total,
        })
    }

    /// Live ports in a checkout that none of its declared services explain.
    ///
    /// The alternative — pinning each observation to whichever declared service
    /// looks closest — would be a guess, and a service reported as running when
    /// something else is on its port is worse than an honest gap.
    fn external_services(
        &self,
        workspace: &Workspace,
        services: &[ServiceView],
        owners: &[PortOwner],
    ) -> Vec<ExternalService> {
        let claimed: Vec<u16> = services.iter().filter_map(|view| view.actual_port).collect();

        owners
            .iter()
            .filter(|owner| owner.workspace_id.as_ref() == Some(&workspace.id))
            .filter(|owner| !claimed.contains(&owner.port))
            .map(|owner| ExternalService {
                port: owner.port,
                pid: owner.pid,
                container: owner.container.clone(),
                cwd: owner.cwd.clone(),
                command_line: owner.command_line.clone(),
                url: Some(format!("http://localhost:{}", owner.port)),
            })
            .collect()
    }

    /// A service with its current process state resolved against the OS.
    pub fn service_view(&self, service: &Service) -> Result<ServiceView> {
        let owners = self.port_owners()?;
        self.service_view_with(service, &owners)
    }

    fn service_view_with(&self, service: &Service, owners: &[PortOwner]) -> Result<ServiceView> {
        let (status, instance) = self.current_state(service)?;

        // Only report a port while something is actually bound to it. A stopped
        // service showing `:3005` reads as "it is on 3005", which is precisely
        // the confusion this tool exists to remove.
        let mut status = status;
        let mut managed = false;
        let mut actual_port = None;

        if status.is_live() {
            managed = true;
            actual_port = instance
                .as_ref()
                .and_then(|i| i.port)
                .or_else(|| self.supervisor.port(&service.id).ok().flatten());
        } else if let Some(port) = self.adopted_port(service, owners)? {
            // Started outside the runtime — from a terminal, or by whoever was
            // here before it was. It is listening on the port this service
            // declares, so calling it stopped would contradict the port table.
            status = ServiceStatus::Healthy;
            actual_port = Some(port);
        }

        let url = actual_port.and_then(|port| match service.service_type {
            runtime_types::ServiceType::Database | runtime_types::ServiceType::Cache => None,
            _ => Some(format!("http://localhost:{port}")),
        });

        Ok(ServiceView {
            service: service.clone(),
            status,
            instance,
            actual_port,
            url,
            managed,
        })
    }

    /// The port a service is already listening on, if it is.
    ///
    /// Deliberately strict: the declared port must be taken *and* held from
    /// inside this service's own checkout. Either half alone would attribute
    /// an unrelated process to the service.
    fn adopted_port(&self, service: &Service, owners: &[PortOwner]) -> Result<Option<u16>> {
        let workspace = self.require_workspace(&service.workspace_id)?;
        let Some(expected) = ports::PortResolver::preferred_port(service, &workspace) else {
            return Ok(None);
        };
        Ok(owners
            .iter()
            .find(|owner| {
                owner.port == expected && owner.workspace_id.as_ref() == Some(&workspace.id)
            })
            .map(|owner| owner.port))
    }

    /// Reconcile the stored instance for a service against the live process
    /// table. The OS wins: a database row is only a claim.
    pub(crate) fn current_state(
        &self,
        service: &Service,
    ) -> Result<(ServiceStatus, Option<RuntimeInstance>)> {
        let Some(mut instance) = self.store.latest_instance(&service.id)? else {
            return Ok((ServiceStatus::Stopped, None));
        };
        if !instance.status.is_live() {
            return Ok((instance.status, Some(instance)));
        }

        let identity = ProcessIdentity::new(instance.pid, instance.process_start_time);
        if self.adapter.process().is_alive(&identity)? {
            return Ok((instance.status, Some(instance)));
        }

        // The process is gone but nothing recorded the exit — the daemon was
        // not running when it happened. Record it now rather than reporting a
        // service as healthy forever.
        instance.status = ServiceStatus::Stopped;
        instance.stopped_at = Some(Utc::now());
        self.store.update_instance(&instance)?;
        if let Some(port) = instance.port {
            self.store.release_lease(port)?;
        }
        Ok((ServiceStatus::Stopped, Some(instance)))
    }

    // ---- ports ---------------------------------------------------------

    pub fn check_port(&self, port: u16) -> Result<PortStatus> {
        self.resolver().status(port)
    }

    /// Everything listening on this machine, resolved to projects where possible.
    ///
    /// Cached briefly: resolving one port walks the process table to follow the
    /// ancestor chain, so answering for every port on a busy machine would do
    /// that dozens of times in a row.
    pub fn port_owners(&self) -> Result<Vec<PortOwner>> {
        /// Short enough that a service started a moment ago shows up, long
        /// enough that rendering a whole project costs one scan.
        const TTL: Duration = Duration::from_millis(1_500);

        if let Ok(cache) = self.port_owners.lock() {
            if let Some((at, owners)) = cache.as_ref() {
                if at.elapsed() < TTL {
                    return Ok(owners.clone());
                }
            }
        }

        let owners = self.scan_ports()?;
        if let Ok(mut cache) = self.port_owners.lock() {
            *cache = Some((Instant::now(), owners.clone()));
        }
        Ok(owners)
    }

    /// Drop the cached view, so a change this runtime just made is visible at
    /// once rather than up to a TTL later.
    pub(crate) fn invalidate_port_owners(&self) {
        if let Ok(mut cache) = self.port_owners.lock() {
            *cache = None;
        }
    }

    /// Everything listening on this machine, resolved to projects where possible.
    ///
    /// One row per (port, pid): a server that binds both IPv4 and IPv6 appears
    /// twice in the socket table but is one thing to the user.
    pub fn list_ports(&self) -> Result<Vec<PortOwner>> {
        self.port_owners()
    }

    fn scan_ports(&self) -> Result<Vec<PortOwner>> {
        let resolver = self.resolver();
        let mut owners: Vec<PortOwner> = Vec::new();
        for binding in self.adapter.port().listening_ports()? {
            if owners.iter().any(|owner| {
                owner.port == binding.port && Some(owner.pid) == binding.primary_pid()
            }) {
                continue;
            }
            if let Some(owner) = resolver.owner_of(binding.port)? {
                if !owners
                    .iter()
                    .any(|existing| existing.port == owner.port && existing.pid == owner.pid)
                {
                    owners.push(owner);
                }
            }
        }
        owners.sort_by_key(|owner| (owner.port, owner.pid));
        Ok(owners)
    }

    pub fn release_port(&self, port: u16) -> Result<bool> {
        self.store.release_lease(port)
    }

    // ---- settings ------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.store.get_setting(key)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.store.set_setting(key, value)
    }

    // ---- logs ----------------------------------------------------------

    pub fn read_logs(
        &self,
        service_id: &ServiceId,
        max_lines: usize,
        since_seq: Option<u64>,
    ) -> Result<Vec<LogLine>> {
        self.logs.read(service_id, max_lines, since_seq)
    }

    pub fn log_cursor(&self, service_id: &ServiceId) -> Result<Option<u64>> {
        self.logs.cursor(service_id)
    }
}

/// Resolve symlinks so `/tmp` and `/private/tmp` do not register as two
/// different projects on macOS.
fn canonicalize(path: &Path) -> Result<PathBuf> {
    let expanded = if let Some(rest) = path.to_string_lossy().strip_prefix("~/") {
        directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().join(rest))
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    std::fs::canonicalize(&expanded).map_err(|err| {
        RuntimeError::io(format!("cannot resolve {}: {err}", expanded.display()))
    })
}

//! The runtime core.
//!
//! Everything the product does that is not a user interface lives here:
//! registry, port leases, process lifecycle, logs, health and reconciliation.
//! The desktop app, the CLI and the MCP server are all thin callers of this
//! type — which is what keeps the three from drifting into three behaviours.

pub mod builds;
pub mod detect;
pub mod discover;
pub mod docker;
pub mod dotenv;
pub mod events;
pub mod graph;
pub mod git;
pub mod health;
pub mod lifecycle;
pub mod launch;
pub mod logs;
pub mod paths;
pub mod platform;
pub mod pm2;
pub mod ports;
pub mod store;
pub mod supervisors;
pub mod supervisor;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use runtime_adapter::{PlatformAdapter, ProcessIdentity};
use runtime_types::{
    AdoptOutcome, CommandSource, ConflictPolicy, ContainerView, DaemonInfo, ExternalService, Failure, Finding, LaunchObservation, LogLine, LogStream, PortOwner, PortStatus, Project, ProjectId, ProjectView, Result, RuntimeError, RuntimeInstance, Service, ServiceId, ServiceStatus, ServiceView, StartedBy, SupervisedView, Stack, StackId, FlowNode, StackView, Workspace, WorkspaceId, WorkspaceView,
};

use crate::docker::Docker;
pub use runtime_adapter::without_a_console;

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
    pm2: Arc<crate::pm2::Pm2>,
    /// Resolving one port walks the process table, so answering "what is
    /// listening" for a whole machine would do it dozens of times over.
    port_owners: Mutex<Option<(Instant, Vec<PortOwner>)>>,
    /// The timestamp each compose service's log has been read up to.
    ///
    /// In memory rather than in the database: it is a position in something
    /// Docker holds, and losing it on a restart costs one re-read, not any
    /// correctness.
    compose_log_cursor: Mutex<HashMap<ServiceId, String>>,
    /// Launches the runtime was told about but did not perform.
    launches: crate::launch::LaunchLog,
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
        let mut runtime = Self::with_parts(platform::current(), Arc::new(Store::open(database_path)?));
        // Logs on disk are the whole point of persisting them: "why did it die"
        // is asked after the fact, often after the daemon restarted too.
        match LogStore::persistent(logs::DEFAULT_CAPACITY, paths::log_dir()?) {
            Ok(store) => runtime.logs = Arc::new(store),
            Err(err) => tracing::warn!(%err, "keeping logs in memory only"),
        }
        Ok(runtime)
    }

    /// An ephemeral runtime, used by tests.
    pub fn in_memory() -> Result<Self> {
        Ok(Self::with_parts(
            platform::current(),
            Arc::new(Store::open_in_memory()?),
        ))
    }

    /// An ephemeral runtime that still captures output to files.
    ///
    /// Capture files are what keep a service alive when nothing is reading, so
    /// testing that property needs them.
    pub fn in_memory_with_logs(directory: impl AsRef<Path>) -> Result<Self> {
        let mut runtime = Self::in_memory()?;
        runtime.logs = Arc::new(LogStore::persistent(logs::DEFAULT_CAPACITY, directory)?);
        Ok(runtime)
    }

    pub fn with_parts(adapter: Arc<dyn PlatformAdapter>, store: Arc<Store>) -> Self {
        Self {
            adapter,
            store,
            logs: Arc::new(LogStore::default()),
            supervisor: Arc::new(Supervisor::new()),
            compose_log_cursor: Mutex::new(HashMap::new()),
            docker: Arc::new(Docker::new()),
            pm2: Arc::new(crate::pm2::Pm2::new()),
            port_owners: Mutex::new(None),
            launches: crate::launch::LaunchLog::new(),
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
        // Canonicalised, because the checkout that gets registered for it will
        // be. git prints `C:/Users/...` where the filesystem calls the same
        // directory `\\?\C:\Users\...`, so on Windows a project's root and
        // its own root checkout were two different strings for one directory —
        // and every lookup that asks "which checkout is the root" found none.
        // On macOS the two forms happen to agree, which is why this held.
        let root = git
            .as_ref()
            .map(|info| info.main_root.clone())
            .unwrap_or_else(|| path.clone());
        let root = canonicalize(&root).unwrap_or(root);

        let detection = detect::detect(&root);
        let now = Utc::now();
        let known = self.store.find_project_by_path(&root)?;

        // A second clone of a repository already known is a checkout of it, not
        // a second project.
        //
        // The runtime already models a project as one thing with several
        // checkouts, told apart by branch — that is what git worktrees get. A
        // separate clone is the same situation reached a different way, and
        // registering it as its own project produced three entries all called
        // `stockviewer`, where `-p stockviewer` picked one of them by luck.
        //
        // Identified by remote, because that is what makes two directories the
        // same repository. A clone with no remote has nothing to be the same
        // as, and stays its own project.
        if known.is_none() {
            if let Some(remote) = git.as_ref().and_then(|info| info.remote_url.as_deref()) {
                if let Some(sibling) = self.project_with_remote(remote)? {
                    let workspace = self.register_workspace(&sibling.id, &root)?;
                    self.copy_services_from_primary(&sibling.id, &workspace)?;
                    self.events.publish(RuntimeEvent::WorkspaceChanged {
                        project_id: sibling.id.clone(),
                        workspace_id: workspace.id,
                    });
                    return self.project_view(&sibling);
                }
            }
        }
        // Detection runs when a project is first added, and never again.
        // Re-adding happens easily — the Discover tab, a scan, `project add`
        // twice — and must not undo curation: a service the user deleted would
        // come back, and a command they corrected would be overwritten by the
        // guess it replaced.
        let is_new = known.is_none();
        let project = match known {
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

        for detected in detection.services.iter().filter(|_| is_new) {
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
                // Empty unless a config file said otherwise: inference has no
                // way to know what depends on what, and a committed
                // `.runtime.json` is the only thing that does.
                depends_on: detected.depends_on.clone(),
                one_shot: detected.one_shot,
                // Inference never sets this; a config file can, and that is the
                // path it arrives by.
                stop_signal: detected.stop_signal,
                compose: None,
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
        let stored = self
            .store
            .find_workspace_by_path(&path)?
            .ok_or_else(|| RuntimeError::internal("workspace vanished after insert"))?;

        // A new checkout changes what the window should be showing, the same
        // way a new service does.
        self.events.publish(RuntimeEvent::WorkspaceChanged {
            project_id: stored.project_id.clone(),
            workspace_id: stored.id.clone(),
        });
        Ok(stored)
    }

    /// Register one worktree, with the primary checkout's services in it.
    ///
    /// A checkout with no services is a checkout nothing can be started in, and
    /// registering one by hand used to produce exactly that — `sync_worktrees`
    /// copied them and this did not, so the same act had two outcomes
    /// depending on which way it was asked for.
    /// A registered project cloned from the same place.
    ///
    /// Compared after trimming the shapes of the same URL apart: `git@host:a/b.git`
    /// and `https://host/a/b` are one repository, and a trailing `.git` or slash
    /// is punctuation rather than identity.
    fn project_with_remote(&self, remote: &str) -> Result<Option<Project>> {
        let wanted = normalise_remote(remote);
        Ok(self
            .store
            .list_projects()?
            .into_iter()
            .find(|project| {
                project
                    .repository_url
                    .as_deref()
                    .map(|url| normalise_remote(url) == wanted)
                    .unwrap_or(false)
            }))
    }

    /// Stop tracking a checkout.
    ///
    /// The directory is not touched — this is the runtime forgetting a
    /// checkout, not git losing one. Removing the worktree itself is `git
    /// worktree remove`, and doing it from here would mean deleting somebody's
    /// files because they asked to tidy a list.
    ///
    /// Refused for the project's own root: a project without its root is not a
    /// project with one fewer checkout, it is a project with nowhere to be, and
    /// what that asks for is `project remove`.
    ///
    /// Refused while anything in it is running, because the services go with
    /// the registration and the processes would not — they would carry on with
    /// nothing left that knows how to stop them.
    pub fn remove_workspace(&self, workspace_id: &WorkspaceId) -> Result<bool> {
        let workspace = self.require_workspace(workspace_id)?;
        let root = self.root_checkout(&workspace.project_id)?;
        if root.id == workspace.id {
            let project = self
                .store
                .get_project(&workspace.project_id)?
                .map(|project| project.name)
                .unwrap_or_default();
            return Err(RuntimeError::invalid(format!(
                "this is {project}'s own checkout, not one of its worktrees; remove the project instead"
            )));
        }

        let owners = self.port_owners()?;
        let live: Vec<String> = self
            .store
            .list_services(&workspace.id)?
            .into_iter()
            .filter(|service| {
                self.service_view_with(service, &owners)
                    .map(|view| view.status.is_live())
                    .unwrap_or(false)
            })
            .map(|service| service.name)
            .collect();
        if !live.is_empty() {
            return Err(RuntimeError::invalid(format!(
                "still running here: {}; stop them before forgetting the checkout they belong to",
                live.join(", ")
            )));
        }

        let removed = self.store.delete_workspace(&workspace.id)?;
        if removed {
            self.events.publish(RuntimeEvent::WorkspaceChanged {
                project_id: workspace.project_id.clone(),
                workspace_id: workspace.id.clone(),
            });
        }
        Ok(removed)
    }

    pub fn register_worktree(&self, project_id: &ProjectId, path: &Path) -> Result<Workspace> {
        let workspace = self.register_workspace(project_id, path)?;
        if workspace.worktree {
            self.copy_services_from_primary(project_id, &workspace)?;
        }
        Ok(workspace)
    }

    /// Give a checkout the services of the one it was branched from.
    ///
    /// Tops up rather than replaces, so registering a worktree again is how a
    /// service declared after the branch was made reaches it. Anything already
    /// there is left alone: a worktree's copy is edited on its own terms, and
    /// overwriting it would undo that quietly.
    ///
    /// Dependencies come across as they are — they are names within a
    /// workspace, so a copy resolves against its own siblings rather than
    /// reaching back into the checkout it came from.
    fn copy_services_from_primary(
        &self,
        project_id: &ProjectId,
        workspace: &Workspace,
    ) -> Result<()> {
        // The project's root, which is the first checkout registered for it.
        // Not "the one that is not a linked worktree": a second clone is not
        // one either, so that test can match more than one and pick by order.
        let Ok(primary) = self.root_checkout(project_id) else {
            return Ok(());
        };
        if primary.id == workspace.id {
            return Ok(());
        }

        let existing: Vec<String> = self
            .store
            .list_services(&workspace.id)?
            .into_iter()
            .map(|service| service.name)
            .collect();

        for template in self.store.list_services(&primary.id)? {
            if existing.contains(&template.name) {
                continue;
            }
            let service = Service {
                id: ServiceId::new(),
                workspace_id: workspace.id.clone(),
                cwd: workspace.path.clone(),
                ..template
            };
            self.store.upsert_service(&service)?;
        }
        Ok(())
    }

    /// Discover git worktrees of a project and register any that are new,
    /// copying the primary checkout's services into each.
    pub fn sync_worktrees(&self, project_id: &ProjectId) -> Result<Vec<Workspace>> {
        let project = self.require_project(project_id)?;
        let entries = git::worktrees(&project.root_path)?;
        let mut result = Vec::new();

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
            result.push(self.register_worktree(project_id, &entry.path)?);
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

            // A git worktree lives outside the checkout it was branched from,
            // so a path inside one matches no project root — and the checkout
            // the caller is standing in is exactly the one they meant.
            if let Some(workspace) = self
                .store
                .list_workspaces_all()?
                .into_iter()
                .filter(|workspace| path.starts_with(&workspace.path))
                .max_by_key(|workspace| workspace.path.components().count())
            {
                return self.require_project(&workspace.project_id);
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
        self.announce_service(service, false);
        Ok(service.clone())
    }

    /// Correct a declared service.
    ///
    /// Detection is inference and gets things wrong — a default port that the
    /// project does not actually use, a command from the wrong script. Without
    /// this the only remedy is hand-writing `.runtime.json`.
    pub fn update_service(
        &self,
        id: &ServiceId,
        patch: runtime_types::ServicePatch,
    ) -> Result<Service> {
        let mut service = self.require_service(id)?;
        let previous_name = service.name.clone();
        patch.apply(&mut service);

        // The registry is keyed on (workspace, name); renaming onto an existing
        // name would silently overwrite the other service.
        if service.name != previous_name {
            let taken = self
                .store
                .list_services(&service.workspace_id)?
                .into_iter()
                .any(|other| other.id != service.id && other.name == service.name);
            if taken {
                return Err(RuntimeError::AlreadyExists {
                    message: format!(
                        "this workspace already has a service called '{}'",
                        service.name
                    ),
                });
            }
        }

        self.store.upsert_service(&service)?;
        self.announce_service(&service, false);
        Ok(service)
    }

    /// Declare a service detection did not find.
    pub fn add_service(&self, workspace_id: &WorkspaceId, service: Service) -> Result<Service> {
        self.require_workspace(workspace_id)?;
        if self
            .store
            .list_services(workspace_id)?
            .iter()
            .any(|other| other.name == service.name)
        {
            return Err(RuntimeError::AlreadyExists {
                message: format!(
                    "this workspace already has a service called '{}'",
                    service.name
                ),
            });
        }
        self.store.upsert_service(&service)?;
        self.announce_service(&service, false);
        Ok(service)
    }

    /// Tell everyone watching that a definition changed.
    ///
    /// Best effort: an event that cannot be addressed to a project is not worth
    /// failing an edit over.
    fn announce_service(&self, service: &Service, removed: bool) {
        let Ok(Some(workspace)) = self.store.get_workspace(&service.workspace_id) else {
            return;
        };
        self.events.publish(RuntimeEvent::ServiceChanged {
            project_id: workspace.project_id,
            service_id: service.id.clone(),
            removed,
        });
    }

    /// Tell everyone watching that a checkout's contents changed.
    ///
    /// Groups are part of the checkout view, so declaring or dropping one has
    /// to reach a window that is showing them — otherwise the group somebody
    /// just made from the terminal is invisible until something unrelated
    /// happens. Best effort, like `announce_service`.
    fn announce_workspace(&self, workspace_id: &WorkspaceId) {
        let Ok(Some(workspace)) = self.store.get_workspace(workspace_id) else {
            return;
        };
        self.events.publish(RuntimeEvent::WorkspaceChanged {
            project_id: workspace.project_id,
            workspace_id: workspace.id,
        });
    }

    /// The project's services as a committable `.runtime.json`.
    ///
    /// Inference is a starting point; this is how a corrected registry becomes
    /// something the repository carries and a teammate gets for free.
    pub fn export_config(&self, project_id: &ProjectId) -> Result<runtime_types::ProjectConfig> {
        let project = self.require_project(project_id)?;
        // The root checkout defines the project; the others are copies of it.
        let primary = self.root_checkout(project_id)?;
        let primary = &primary;

        let mut services = std::collections::BTreeMap::new();
        for service in self.store.list_services(&primary.id)? {
            let cwd = service
                .cwd
                .strip_prefix(&primary.path)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())
                .map(|relative| relative.to_path_buf());

            services.insert(
                service.name.clone(),
                runtime_types::ServiceConfig {
                    command: service.command,
                    port: service.preferred_port,
                    cwd,
                    service_type: Some(service.service_type),
                    env: service.env,
                    health: service.health_check,
                    auto_start: service.auto_start,
                    on_conflict: Some(service.conflict_policy),
                    depends_on: service.depends_on,
                    one_shot: service.one_shot,
                    stop_signal: service.stop_signal,
                },
            );
        }

        Ok(runtime_types::ProjectConfig {
            name: Some(project.name),
            services,
        })
    }

    pub fn delete_service(&self, id: &ServiceId) -> Result<bool> {
        let service = self.store.get_service(id)?;
        self.store.release_leases_for_service(id)?;
        let removed = self.store.delete_service(id)?;
        if let Some(service) = service.filter(|_| removed) {
            self.announce_service(&service, true);
        }
        Ok(removed)
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
    /// The checkout a selector points *inside*, when it is a path.
    ///
    /// `-p /path/to/a/checkout` names one checkout, not just the project it
    /// belongs to. Resolving only as far as the project sends an edit to
    /// whichever checkout came first — which for a repository cloned twice is
    /// the other one, silently.
    pub fn workspace_for_selector(&self, selector: &str) -> Result<Option<Workspace>> {
        let path = Path::new(selector);
        if !path.is_absolute() && !path.exists() {
            return Ok(None);
        }
        let Ok(path) = canonicalize(path) else {
            return Ok(None);
        };
        for candidate in path.ancestors() {
            if let Some(workspace) = self.store.find_workspace_by_path(candidate)? {
                return Ok(Some(workspace));
            }
        }
        Ok(None)
    }

    pub fn resolve_service(&self, project: Option<&Project>, selector: &str) -> Result<Service> {
        self.resolve_service_in(project, None, selector)
    }

    /// As `resolve_service`, but confined to one checkout when one is known.
    pub fn resolve_service_in(
        &self,
        project: Option<&Project>,
        workspace_filter: Option<&Workspace>,
        selector: &str,
    ) -> Result<Service> {
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
                if let Some(only) = workspace_filter {
                    if workspace.id != only.id {
                        continue;
                    }
                }
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

        // Preferring the project's own root disambiguates it from its other
        // checkouts. Applying that across projects would silently pick one of
        // several unrelated services sharing a name.
        //
        // The root, not merely "not a worktree": a second clone of the same
        // repository is a checkout too and is not a linked worktree, so on
        // that test a project could have several primaries and the first one
        // found would win — which is the silent pick this is here to stop.
        let single_project = matches
            .iter()
            .all(|(workspace, _)| workspace.project_id == matches[0].0.project_id);
        if branch.is_none() && single_project {
            let root = self.root_checkout(&matches[0].0.project_id)?;
            if let Some((_, service)) = matches.iter().find(|(workspace, _)| workspace.id == root.id)
            {
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
                    // With the checkout path, because the branch is not always
                    // enough to tell them apart — and when it is not, this list
                    // was the same string printed twice, which told the reader
                    // nothing about how to choose.
                    format!(
                        "{project}/{}/{} ({})",
                        workspace.git_branch.as_deref().unwrap_or("-"),
                        service.name,
                        workspace.path.display()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(RuntimeError::invalid(format!(
                "'{selector}' matches several services: {options}. Name one with a path inside it"
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

            let containers = self.containers_for(&workspace);
            // Containers are listed as themselves; repeating them as
            // unexplained ports would be the same thing said twice.
            let container_ports: Vec<u16> =
                containers.iter().flat_map(|c| c.ports.clone()).collect();
            let external: Vec<ExternalService> = self
                .external_services(&workspace, &services, &owners)
                .into_iter()
                .filter(|item| !container_ports.contains(&item.port))
                .collect();

            external_total += external.len();
            // A PM2 entry that a declared service already stands for is not
            // listed again. The same process appearing twice — once saying it
            // cannot be stopped, once offering a Stop button — is the kind of
            // contradiction this tool exists to remove.
            let claimed: Vec<String> = services
                .iter()
                .filter_map(|view| view.supervisor_entry.clone())
                .collect();
            let supervised: Vec<SupervisedView> = self
                .supervised_for(&workspace, &owners)
                .into_iter()
                .filter(|entry| !claimed.contains(&entry.name))
                .collect();
            // Read from the same producer that answers `stack list`, so a
            // group is one fact with one source rather than something each
            // surface assembles for itself.
            let stacks = self.stack_views(&workspace.id).unwrap_or_default();
            workspaces.push(WorkspaceView {
                workspace,
                services,
                external,
                containers,
                supervised,
                stacks,
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

    /// Pids in the process trees of services the runtime started.
    ///
    /// One service can put several ports on the machine: a dev entrypoint that
    /// boots a throwaway Postgres and a schema browser alongside its API holds
    /// three, and all three die with it. Listing the two it spawned as
    /// unexplained would invite stopping them one by one, when `stop` already
    /// reaches them through the process group — and would report a project as
    /// having strangers in it when it does not.
    ///
    /// Only services this runtime started count as roots. A service found
    /// already listening is not ours to speak for, and claiming its children
    /// would be the same guess this module exists to avoid.
    fn managed_process_trees(&self, services: &[ServiceView]) -> HashSet<u32> {
        let mut roots: Vec<u32> = services
            .iter()
            .filter(|view| view.managed)
            .filter_map(|view| view.instance.as_ref())
            .map(|instance| instance.pid)
            .collect();
        // A recorded launch that turned into a listener is direct evidence, not
        // an inference: something asked for this command here, and this pid
        // answered moments later. Its children are its own.
        roots.extend(self.launches.bound_pids());
        if roots.is_empty() {
            // Nothing to attribute, so nothing worth a process scan.
            return HashSet::new();
        }

        let Ok(all) = self.adapter.process().list_processes() else {
            // Without a process table every port stays unexplained, which is
            // the honest answer rather than a wrong attribution.
            return HashSet::new();
        };

        let mut tree: HashSet<u32> = roots.iter().copied().collect();
        let mut frontier = roots;
        while let Some(current) = frontier.pop() {
            for proc in &all {
                if proc.parent_pid == Some(current) && tree.insert(proc.pid) {
                    frontier.push(proc.pid);
                }
            }
        }
        tree
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
        let spawned = self.managed_process_trees(services);

        owners
            .iter()
            .filter(|owner| owner.workspace_id.as_ref() == Some(&workspace.id))
            .filter(|owner| !claimed.contains(&owner.port))
            .filter(|owner| !spawned.contains(&owner.pid))
            .map(|owner| ExternalService {
                port: owner.port,
                pid: owner.pid,
                container: owner.container.clone(),
                cwd: owner.cwd.clone(),
                command_line: owner.command_line.clone(),
                supervisor: owner.supervisor.clone(),
                url: Some(format!("http://localhost:{}", owner.port)),
            })
            .collect()
    }

    /// Entries another supervisor keeps in this checkout.
    ///
    /// Listed beside the declared services rather than folded into them: PM2
    /// decided what these are and whether they come back after a reboot, and
    /// presenting them as the runtime's own would claim an ownership it does
    /// not have — and must not take, since removing an entry from PM2 is
    /// usually also what stops it starting at boot.
    fn supervised_for(&self, workspace: &Workspace, owners: &[PortOwner]) -> Vec<SupervisedView> {
        self.pm2
            .processes_in(&workspace.path)
            .into_iter()
            .map(|process| {
                let ports: Vec<u16> = process
                    .pid
                    .map(|pid| {
                        owners
                            .iter()
                            .filter(|owner| owner.pid == pid)
                            .map(|owner| owner.port)
                            .collect()
                    })
                    .unwrap_or_default();

                SupervisedView {
                    url: ports.first().map(|port| format!("http://localhost:{port}")),
                    restart_warning: restart_warning(&process),
                    name: process.name,
                    supervisor: "pm2".to_string(),
                    status: process.status,
                    pid: process.pid,
                    command: process.command,
                    restarts: process.restarts,
                    ports,
                }
            })
            .collect()
    }

    /// Switch a supervised entry on or off.
    pub fn control_supervised(&self, name: &str, action: crate::pm2::Pm2Action) -> Result<SupervisedView> {
        self.pm2.control(name, action)?;
        self.invalidate_port_owners();

        let process = self
            .pm2
            .process(name)
            .ok_or_else(|| RuntimeError::invalid(format!("pm2 has no entry '{name}'")))?;
        let owners = self.port_owners()?;
        let ports: Vec<u16> = process
            .pid
            .map(|pid| {
                owners
                    .iter()
                    .filter(|owner| owner.pid == pid)
                    .map(|owner| owner.port)
                    .collect()
            })
            .unwrap_or_default();

        Ok(SupervisedView {
            url: ports.first().map(|port| format!("http://localhost:{port}")),
            restart_warning: restart_warning(&process),
            name: process.name,
            supervisor: "pm2".to_string(),
            status: process.status,
            pid: process.pid,
            command: process.command,
            restarts: process.restarts,
            ports,
        })
    }

    /// Containers compose defines for a checkout.
    ///
    /// One directory can hold several stacks — a dev compose file and a `-prod`
    /// one — and listing every dead container from every stack buries the one
    /// being used. A stopped container is shown when its own stack has
    /// something running, so "the stack you are using, including the parts that
    /// are off" stays complete while dormant stacks stay out of the way. When
    /// nothing at all is running, they all appear: otherwise there would be
    /// nothing to switch on.
    fn containers_for(&self, workspace: &Workspace) -> Vec<ContainerView> {
        let all = self.docker.containers_in(&workspace.path);

        // A container a service has claimed is that service, and showing it
        // again on its own is the same thing twice with two switches: one that
        // knows about the stack it belongs to and one that does not. The
        // service is the better of the two, so this row goes.
        //
        // Only the claimed ones. Everything else in the compose file is still
        // worth a row — that is how somebody finds a container to claim, and
        // how a project nobody has claimed anything in still has switches.
        let claimed: Vec<(PathBuf, String)> = self
            .store
            .list_services(&workspace.id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|service| {
                let compose = service.compose?;
                Some((compose.file.parent()?.to_path_buf(), compose.service))
            })
            .collect();
        let all: Vec<_> = all
            .into_iter()
            .filter(|container| {
                !claimed.iter().any(|(dir, name)| {
                    container.compose_service.as_deref() == Some(name.as_str())
                        && container.working_dir.as_deref() == Some(dir.as_path())
                })
            })
            .collect();
        let live_stacks: Vec<Option<String>> = all
            .iter()
            .filter(|container| container.is_running())
            .map(|container| container.compose_project.clone())
            .collect();

        let mut containers: Vec<_> = all
            .into_iter()
            .filter(|container| {
                container.is_running()
                    || live_stacks.is_empty()
                    || live_stacks.contains(&container.compose_project)
            })
            .map(|container| ContainerView {
                url: container
                    .published_ports
                    .first()
                    .map(|port| format!("http://localhost:{port}")),
                name: container.name,
                service: container.compose_service,
                image: container.image,
                status: container.status,
                health: container.health,
                ports: container.published_ports,
            })
            .collect();

        // Running first: the view is read to see what is up.
        containers.sort_by(|a, b| {
            b.is_running()
                .cmp(&a.is_running())
                .then_with(|| a.name.cmp(&b.name))
        });
        containers
    }

    // ---- containers ----------------------------------------------------

    /// Switch a container on or off.
    pub fn control_container(
        &self,
        name: &str,
        action: docker::ContainerAction,
    ) -> Result<ContainerView> {
        self.docker.control(name, action)?;
        // What is listening just changed.
        self.invalidate_port_owners();

        let container = self
            .docker
            .container(name)
            .ok_or_else(|| RuntimeError::not_found("container", name))?;
        Ok(ContainerView {
            url: container
                .published_ports
                .first()
                .map(|port| format!("http://localhost:{port}")),
            name: container.name,
            service: container.compose_service,
            image: container.image,
            status: container.status,
            health: container.health,
            ports: container.published_ports,
        })
    }

    /// Pull anything new out of a compose service's container into the log
    /// store.
    ///
    /// Incremental by timestamp: compose can be asked for everything after a
    /// moment, and the last line's own timestamp is that moment. Without it
    /// every read would re-append the whole log and the store would grow by a
    /// copy of itself each time somebody looked.
    ///
    /// Quiet on failure. Docker being down is a reason to have no new lines,
    /// not a reason to refuse to show the ones already held.
    pub(crate) fn pull_compose_logs(&self, service_id: &ServiceId) -> Result<()> {
        let Some(service) = self.store.get_service(service_id)? else {
            return Ok(());
        };
        let Some(binding) = service.compose.as_ref() else {
            return Ok(());
        };

        let since = self
            .compose_log_cursor
            .lock()
            .ok()
            .and_then(|cursor| cursor.get(service_id).cloned());

        let Ok(lines) = self.docker.compose_logs(
            &binding.file,
            &binding.service,
            since.as_deref(),
            logs::MAX_READ_LINES,
        ) else {
            return Ok(());
        };

        // Two names for one thing: `seen` stays at where the last read
        // finished, and `newest` moves as lines are taken.
        let seen = since.clone();
        let mut newest = since;
        for line in lines {
            // `service-1  | 2026-09-05T01:39:29.173Z the message`
            let body = line.split_once('|').map(|(_, rest)| rest).unwrap_or(&line).trim_start();
            let (stamp, message) = match body.split_once(' ') {
                Some((stamp, message)) if stamp.contains('T') => (Some(stamp), message),
                _ => (None, body),
            };
            if let Some(stamp) = stamp {
                // `--since` includes the moment it is given, so the line that
                // set the cursor comes back every time. Compared rather than
                // matched for equality: docker prints a fixed-width RFC3339,
                // which orders the same as its text, and one line arriving out
                // of order would otherwise reopen the whole log.
                if let Some(seen) = seen.as_deref() {
                    if stamp <= seen {
                        continue;
                    }
                }
                newest = Some(stamp.to_string());
            }
            // Docker merges the container's two streams and does not say which
            // was which, so claiming one would be inventing the distinction.
            self.logs.append(service_id, LogStream::Stdout, message)?;
        }

        if let (Ok(mut cursor), Some(newest)) = (self.compose_log_cursor.lock(), newest) {
            cursor.insert(service_id.clone(), newest);
        }
        Ok(())
    }

    // ---- compose ----------------------------------------------------------

    /// Hand a declared service over to compose.
    ///
    /// The manual half of the claim. Detection can propose these from a
    /// compose file, but a project that runs its database through compose and
    /// everything else natively has a service somebody already declared, and
    /// re-declaring it would lose whatever they had corrected about it. This
    /// binds the one they have.
    ///
    /// The compose service has to exist in the file. A binding that names
    /// nothing is a service that will fail at the moment somebody tries to
    /// start it, with an error about a file rather than about the mistake.
    pub fn claim_compose(
        &self,
        service_id: &ServiceId,
        file: &Path,
        compose_service: &str,
    ) -> Result<ServiceView> {
        let mut service = self.require_service(service_id)?;
        let file = if file.is_absolute() {
            file.to_path_buf()
        } else {
            let workspace = self.require_workspace(&service.workspace_id)?;
            workspace.path.join(file)
        };
        if !file.exists() {
            return Err(RuntimeError::not_found("compose file", file.display().to_string()));
        }
        // Stored resolved, for the same reason a project's path is: this is
        // compared against a container's own directory, and one side holding
        // `/tmp` while the other holds `/private/tmp` matches nothing.
        let file = canonicalize(&file).unwrap_or(file);

        let declared = self.docker.compose_declared(&file)?;
        if !declared.iter().any(|d| d.service == compose_service) {
            let names: Vec<&str> = declared.iter().map(|d| d.service.as_str()).collect();
            return Err(RuntimeError::invalid(format!(
                "'{compose_service}' is not in {}; it declares {}",
                file.display(),
                if names.is_empty() { "nothing".to_string() } else { names.join(", ") }
            )));
        }

        service.compose = Some(runtime_types::ComposeBinding {
            file,
            service: compose_service.to_string(),
        });
        self.store.upsert_service(&service)?;
        self.announce_service(&service, false);
        self.service_view(&service)
    }

    /// Stop claiming a service through compose; it becomes a command again.
    pub fn release_compose(&self, service_id: &ServiceId) -> Result<ServiceView> {
        let mut service = self.require_service(service_id)?;
        service.compose = None;
        self.store.upsert_service(&service)?;
        self.announce_service(&service, false);
        self.service_view(&service)
    }

    /// Take a compose project down: its containers and network, removed.
    ///
    /// Separate from stopping, and deliberately harder to reach. A stop is
    /// reversible in seconds; this throws the containers away. Named volumes
    /// are kept — a database deleted by something the person read as "stop"
    /// is not a trade the runtime gets to make on their behalf.
    ///
    /// Whole-project rather than per-service because that is what `down` is
    /// defined over: the network is shared, and taking it out from under the
    /// others is not something one service can ask for.
    pub fn compose_down(&self, service_id: &ServiceId) -> Result<Vec<ServiceView>> {
        let service = self.require_service(service_id)?;
        let binding = service.compose.clone().ok_or_else(|| {
            RuntimeError::invalid(format!(
                "'{}' is not a compose service; nothing to take down",
                service.name
            ))
        })?;

        self.docker.compose_down(&binding.file)?;
        self.invalidate_port_owners();

        // Every service in this workspace bound to the same file has just
        // lost its container, and saying so about one of them would leave the
        // rest reading as running.
        let mut touched = Vec::new();
        for other in self.store.list_services(&service.workspace_id)? {
            if other.compose.as_ref().map(|c| &c.file) == Some(&binding.file) {
                self.announce_service(&other, false);
                touched.push(self.service_view(&other)?);
            }
        }
        Ok(touched)
    }

    /// What a compose file declares, without starting anything.
    ///
    /// For an agent or a person deciding what to claim, and for detection to
    /// propose services with the dependencies the file already states.
    pub fn compose_declared(&self, file: &Path) -> Result<Vec<docker::ComposeDeclared>> {
        self.docker.compose_declared(file)
    }

    pub fn container_logs(&self, name: &str, max_lines: usize) -> Result<Vec<String>> {
        self.docker.logs(name, max_lines.clamp(1, logs::MAX_READ_LINES))
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
        let mut supervisor = None;
        let mut supervisor_entry = None;

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
            // Only for a service the runtime did not start. For one it did,
            // the runtime is the supervisor, and naming another would be
            // saying two things own the same process.
            let owner = owners.iter().find(|owner| owner.port == port);
            supervisor = owner.and_then(|owner| owner.supervisor.clone());
            // Which entry, not just which supervisor: the difference between
            // explaining why Stop is missing and being able to offer one.
            supervisor_entry = owner.and_then(|owner| {
                self.pm2
                    .processes()
                    .into_iter()
                    .find(|process| process.pid == Some(owner.pid))
                    .map(|process| process.name)
            });
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
            supervisor,
            supervisor_entry,
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

        let listening = owners.iter().any(|owner| {
            owner.port == expected && owner.workspace_id.as_ref() == Some(&workspace.id)
        });
        if !listening {
            return Ok(None);
        }

        // Two modes of the same package can declare the same port — one `dev`
        // and one `dev:local`, only ever one of them running. Adopting the
        // listener into both would report two services as up when at most one
        // is, and there is nothing in the process to say which. Reporting
        // neither leaves the port visible as unexplained, which is true.
        let claimants = self
            .store
            .list_services(&workspace.id)?
            .into_iter()
            .filter(|other| {
                ports::PortResolver::preferred_port(other, &workspace) == Some(expected)
            })
            .count();
        if claimants > 1 {
            return Ok(None);
        }

        Ok(Some(expected))
    }

    /// Reconcile the stored instance for a service against the live process
    /// table. The OS wins: a database row is only a claim.
    /// Whether the group this instance was started in still holds its port.
    ///
    /// The test the runtime needs when the process it recorded is gone. A
    /// service is spawned into a group of its own and the leader is often the
    /// first to leave — `pnpm run dev` execs its way out, `docker compose`
    /// finishes — while the server it started keeps serving from inside that
    /// group. Calling the service stopped then loses the runtime's claim on
    /// it, and since the port answers it comes back as "started elsewhere",
    /// which is what makes the Stop button disappear.
    ///
    /// Asked as "is the holder of our port in our group" rather than "does our
    /// group still exist", because a recycled pid leads a group of its own and
    /// the second question would hand the runtime a stranger's processes.
    fn group_still_serving(&self, instance: &RuntimeInstance) -> Result<bool> {
        Ok(crate::lifecycle::group_still_holds_port(
            &self.adapter,
            instance,
        ))
    }

    pub(crate) fn current_state(
        &self,
        service: &Service,
    ) -> Result<(ServiceStatus, Option<RuntimeInstance>)> {
        // Asked of Docker, because the recorded pid is zero and deliberately
        // so. Read from the cached container list rather than by shelling out
        // per service: this runs once per service per render.
        if let Some(binding) = &service.compose {
            let instance = self.store.latest_instance(&service.id)?;
            let found = self.docker.containers().into_iter().find(|container| {
                container.compose_service.as_deref() == Some(binding.service.as_str())
                    && container.working_dir.as_deref() == binding.file.parent()
            });
            let Some(container) = found else {
                // No container: never created, or removed by a `down` or by
                // somebody at a terminal. Reported as stopped rather than as
                // the last thing that was true, so that a container removed
                // out from under the runtime stops being described as running.
                return Ok((ServiceStatus::Stopped, instance));
            };
            let status = if container.is_running() {
                match container.health.as_deref() {
                    Some("unhealthy") => ServiceStatus::Unhealthy,
                    Some("starting") => ServiceStatus::Starting,
                    _ => ServiceStatus::Healthy,
                }
            } else if container.exit_code == 0 {
                ServiceStatus::Stopped
            } else if instance
                .as_ref()
                .is_some_and(|i| i.status == ServiceStatus::Stopped)
            {
                // Asked to stop, and it did. Docker reports the signal that
                // ended it — 143 for SIGTERM, 137 for SIGKILL — and reading
                // those as failures would put every service somebody switched
                // off into the list of what is broken.
                ServiceStatus::Stopped
            } else {
                // Exited on its own, and not well. Saying "stopped" here would
                // keep it out of every list of what is broken.
                ServiceStatus::Failed
            };
            return Ok((status, instance));
        }

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

        // The leader is gone, but the group it led may still be serving —
        // which is the ordinary shape of `pnpm run dev`, not an edge case.
        // Keeping the claim here is what keeps a Stop button on it.
        if self.group_still_serving(&instance)? {
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

    // ---- stacks -----------------------------------------------------------

    pub fn list_stacks(&self, workspace_id: &WorkspaceId) -> Result<Vec<Stack>> {
        self.stacks_for(workspace_id)
    }

    /// Stacks with what their members are actually doing.
    ///
    /// The group as a unit, because that is what it was declared to be. A
    /// database, an API and a front end shown as three peers with three buttons
    /// makes the reader reassemble the thing every time they look, and leaves
    /// the order to memory.
    pub fn stack_views(&self, workspace_id: &WorkspaceId) -> Result<Vec<StackView>> {
        let declared = self.store.list_services(workspace_id)?;
        let owners = self.port_owners()?;
        let mut out = Vec::new();

        for stack in self.stacks_for(workspace_id)? {
            let mut services = Vec::new();
            let mut missing = Vec::new();

            for step in &stack.members {
                match declared.iter().find(|service| &service.name == step) {
                    // In the order the stack names them, not the order they were
                    // declared: the order is the point.
                    Some(service) => services.push(self.service_view_with(service, &owners)?),
                    None => missing.push(step.clone()),
                }
            }

            // What is actually up. A one-shot used to be counted here on the
            // grounds that a migration which has run leaves nothing running —
            // but the test was `one_shot`, not "has run", so a stack whose only
            // member was a one-shot reported itself fully up before it had ever
            // been executed, showed a live dot, and offered a Stop that had
            // nothing to stop.
            //
            // One-shots are told apart in the flow instead, where each member
            // says whether it is one, so a surface can say "ran" about the
            // thing that ran and "up" about the thing that is up.
            let running = services
                .iter()
                .filter(|view| view.status.is_live())
                .count();

            let flow = flow_of(&stack, &services, &missing);
            out.push(StackView {
                stack,
                services,
                running,
                missing,
                flow,
            });
        }
        Ok(out)
    }

    /// Declare a named sequence of steps.
    ///
    /// Every step is checked against the checkout now rather than when it is
    /// run: a stack naming a service that does not exist is a stack that fails
    /// halfway through, having already started the things before it.
    /// Refuse to bring up a service that belongs to no stack.
    ///
    /// A service in no stack is one whose companions and order nobody has
    /// written down. Starting it on its own is how half a set comes up and
    /// then looks like a working one — which is the misreading a stack is
    /// declared to prevent.
    ///
    /// Here rather than on a button. The first version of this rule lived in
    /// the panel's start button, which left the same question answered three
    /// different ways: the panel refused, the window offered, the CLI obliged.
    /// A rule that only one caller obeys is a suggestion.
    ///
    /// Asked only of a request naming the service. Starting a stack starts its
    /// members, and starting anything brings up what it depends on, and
    /// neither of those is somebody asking for a loose service by name — they
    /// go through `start_service` directly and never past here.
    /// The stacks that apply to a checkout, wherever they were declared.
    ///
    /// A stack is declared once for the project, on its own root, and names
    /// services by the names they have in every checkout of it. Reading them
    /// out of the checkout they happen to be stored in makes a worktree look
    /// like it has none — which is how `stack list` came to show one and the
    /// start rule to deny it, about the same service.
    ///
    /// The root, not "the checkout that is not a linked worktree": a second
    /// clone is a checkout too and is not a worktree, so that test can match
    /// more than one and pick by iteration order.
    pub fn stacks_for(&self, workspace_id: &WorkspaceId) -> Result<Vec<Stack>> {
        let home = self.stack_home(workspace_id)?;
        self.store.list_stacks(&home)
    }

    /// The checkout a project's stacks are declared in: its root.
    /// The checkout a project's own root is registered as.
    ///
    /// The first one registered for it: `add_project` creates the project and
    /// registers its root, and anything else arrives later. Offsets are handed
    /// out in that order and never reused, so the lowest is the root, and
    /// `list_workspaces` is ordered by it.
    ///
    /// Not by comparing paths. That is the more direct statement of what a root
    /// is, and it is also a string comparison between a path git printed and a
    /// path the filesystem canonicalised — the same directory spelled two ways
    /// on Windows, where it found nothing and every caller quietly took the
    /// wrong branch. A number has no spelling.
    pub fn root_checkout(&self, project_id: &ProjectId) -> Result<Workspace> {
        self.store
            .list_workspaces(project_id)?
            .into_iter()
            .next()
            .ok_or_else(|| RuntimeError::not_found("checkout of project", project_id.as_str()))
    }

    fn stack_home(&self, workspace_id: &WorkspaceId) -> Result<WorkspaceId> {
        let workspace = self.require_workspace(workspace_id)?;
        let project = self
            .store
            .get_project(&workspace.project_id)?
            .ok_or_else(|| RuntimeError::not_found("project", workspace.project_id.as_str()))?;
        Ok(self.root_checkout(&project.id)?.id)
    }

    /// A service is not started or stopped on its own; its stack is.
    ///
    /// The unit somebody declared is the unit that runs. Bringing one member up
    /// by hand leaves the rest down and looks, from every list, like the stack
    /// is partly up — and taking one down out from under the others is the same
    /// thing in reverse. Whatever is true of the set is what there is to say.
    ///
    /// Asked only where a request names a service. Running a stack starts its
    /// members, and starting anything brings up what it depends on; both go
    /// through the runtime directly and never past here, so they are unaffected
    /// by construction rather than by an exception list.
    pub fn refuse_alone(&self, service_id: &ServiceId, verb: &str) -> Result<()> {
        let service = self.require_service(service_id)?;
        let stacks = self.stacks_for(&service.workspace_id)?;
        let named: Vec<&str> = stacks
            .iter()
            .filter(|stack| stack.members.iter().any(|member| member == &service.name))
            .map(|stack| stack.name.as_str())
            .collect();

        // The verb says what was refused; the advice is an instruction, so it
        // needs the imperative rather than the same past participle again —
        // "started 'dev'" is not something anybody can do.
        let how = if verb == "stopped" { "stack stop" } else { "stack run" };
        let advice = match named.as_slice() {
            [] => format!(
                "'{}' is in no stack, so there is nothing recorded about what belongs beside it; put it in one first",
                service.name
            ),
            [one] => format!(
                "services are {verb} as a stack, not one at a time — try `{how} {one}`, which '{}' is part of",
                service.name
            ),
            many => format!(
                "services are {verb} as a stack, not one at a time — '{}' is in {}, so `{how} <name>`",
                service.name,
                many.join(", ")
            ),
        };
        Err(RuntimeError::invalid(advice))
    }

    pub fn require_in_a_stack(&self, service_id: &ServiceId) -> Result<()> {
        let service = self.require_service(service_id)?;
        let stacks = self.stacks_for(&service.workspace_id)?;
        if stacks
            .iter()
            .any(|stack| stack.members.iter().any(|name| name == &service.name))
        {
            return Ok(());
        }
        Err(RuntimeError::invalid(format!(
            "'{}' is in no stack, so there is nothing recorded about what it needs beside it; put it in one first",
            service.name
        )))
    }

    /// Declare or replace a stack.
    ///
    /// `auto_start` is `None` for "leave it as it is", which is what every
    /// caller that only means to change the members passes — setting the
    /// members should not quietly switch off a stack somebody asked to come up
    /// at boot.
    pub fn set_stack(
        &self,
        workspace_id: &WorkspaceId,
        name: &str,
        members: Vec<String>,
        auto_start: Option<bool>,
    ) -> Result<Stack> {
        let declared = self.store.list_services(workspace_id)?;
        for step in &members {
            if !declared.iter().any(|service| &service.name == step) {
                return Err(RuntimeError::invalid(format!(
                    "'{step}' is not a service in this checkout"
                )));
            }
        }

        let declared_in = self.stack_home(workspace_id)?;
        let existing = self
            .store
            .list_stacks(&declared_in)?
            .into_iter()
            .find(|stack| stack.name == name);

        let stack = Stack {
            id: existing
                .as_ref()
                .map(|stack| stack.id.clone())
                .unwrap_or_else(StackId::new),
            workspace_id: declared_in,
            name: name.to_string(),
            members,
            auto_start: auto_start
                .unwrap_or_else(|| existing.as_ref().is_some_and(|stack| stack.auto_start)),
        };
        self.store.upsert_stack(&stack)?;
        self.announce_workspace(workspace_id);
        Ok(stack)
    }

    /// Every stack that asked to come up when the daemon starts.
    ///
    /// Across all checkouts, because the daemon serves the machine rather than
    /// a project — and a worktree that declared its own stack is as entitled to
    /// start as the one it was branched from.
    pub fn stacks_to_auto_start(&self) -> Result<Vec<(WorkspaceId, String)>> {
        let mut wanted = Vec::new();
        for workspace in self.store.list_workspaces_all()? {
            for stack in self.store.list_stacks(&workspace.id)? {
                if stack.auto_start {
                    wanted.push((workspace.id.clone(), stack.name));
                }
            }
        }
        Ok(wanted)
    }

    pub fn remove_stack(&self, workspace_id: &WorkspaceId, name: &str) -> Result<bool> {
        let Some(stack) = self
            .stacks_for(workspace_id)?
            .into_iter()
            .find(|stack| stack.name == name)
        else {
            return Ok(false);
        };
        let removed = self.store.remove_stack(&stack.id)?;
        if removed {
            self.announce_workspace(workspace_id);
        }
        Ok(removed)
    }

    // ---- what went wrong -------------------------------------------------

    /// Services that are not working, newest first, each with why.
    ///
    /// Answering "what broke" without being told where to look. The alternative
    /// is the two steps somebody debugging cannot take yet: name the service,
    /// then read its whole log for the few lines that matter.
    ///
    /// A service that was deliberately stopped is not a failure. One that
    /// exited on its own, or is up and not answering, is.
    pub fn failures(&self, per_service: usize) -> Result<Vec<Failure>> {
        let mut found: Vec<Failure> = Vec::new();
        let owners = self.port_owners()?;

        for project in self.store.list_projects()? {
            for workspace in self.store.list_workspaces(&project.id)? {
                for service in self.store.list_services(&workspace.id)? {
                    let view = self.service_view_with(&service, &owners)?;

                    // A one-shot that ran and failed counts; one that succeeded
                    // does not, and neither does anything simply not running.
                    if !matches!(
                        view.status,
                        ServiceStatus::Failed | ServiceStatus::Unhealthy
                    ) {
                        continue;
                    }

                    let instance = view.instance.as_ref();
                    found.push(Failure {
                        subject: format!("{}/{}", project.name, service.name),
                        status: view.status,
                        at: instance
                            .and_then(|i| i.stopped_at)
                            .or_else(|| instance.map(|i| i.started_at))
                            .unwrap_or_else(Utc::now),
                        exit_code: instance.and_then(|i| i.exit_code),
                        detail: self.last_words(
                            &service.id,
                            per_service,
                            instance.map(|i| i.started_at),
                        ),
                        service_id: service.id,
                    });
                }
            }
        }

        // Newest first: the thing that just broke is the thing being looked for.
        found.sort_by_key(|failure| std::cmp::Reverse(failure.at));
        Ok(found)
    }

    /// The last thing a service said during one run, preferring stderr.
    ///
    /// A failure normally explains itself and then stops, so the tail is the
    /// message. Stderr first because a busy service's access log will otherwise
    /// fill the tail with lines that were never about the problem.
    ///
    /// Bounded to the run that failed. Output is kept across restarts on
    /// purpose — losing it at the moment a service dies is exactly wrong — but
    /// that means a service failing for the third time has three failures in
    /// its log, and reading the tail of all of them produces an error message
    /// assembled from different attempts.
    fn last_words(
        &self,
        service_id: &ServiceId,
        lines: usize,
        since: Option<DateTime<Utc>>,
    ) -> Vec<String> {
        let Ok(everything) = self.read_logs(service_id, 400, None) else {
            return Vec::new();
        };
        let all: Vec<LogLine> = match since {
            Some(started_at) => everything
                .into_iter()
                .filter(|line| line.timestamp >= started_at)
                .collect(),
            None => everything,
        };

        let complaints: Vec<&LogLine> = all
            .iter()
            .filter(|line| line.stream == runtime_types::LogStream::Stderr)
            .collect();
        let chosen: Vec<&LogLine> = if complaints.is_empty() {
            all.iter().collect()
        } else {
            complaints
        };

        chosen
            .into_iter()
            .rev()
            .take(lines)
            .map(|line| line.message.trim_end().to_string())
            .filter(|message| !message.is_empty())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    // ---- diagnosis -------------------------------------------------------

    /// Everything wrong with what is declared, looked for rather than waited on.
    ///
    /// The point is timing. Each of these is already knowable now and only
    /// announces itself later, at the worst moment: a dependency that names
    /// nothing fails halfway through a start, having brought up everything
    /// before it; a cycle hangs; a shared build breaks the service that is not
    /// looking, on its next restart, hours after the cause.
    pub fn diagnose(&self) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();

        for project in self.store.list_projects()? {
            // Two checkouts on one branch is a real conflict, not a tidiness
            // complaint: the services in them are the same services, declared
            // twice, wanting the same ports. A branch is what you bring up
            // once, so which of the two is running becomes a question with no
            // answer in the tool — and both being up is a pair of servers
            // fighting over a port with the same name on each side.
            let mut on_branch: std::collections::BTreeMap<String, Vec<PathBuf>> =
                std::collections::BTreeMap::new();
            for workspace in self.store.list_workspaces(&project.id)? {
                if let Some(branch) = workspace.git_branch.clone() {
                    on_branch.entry(branch).or_default().push(workspace.path.clone());
                }
            }
            for (branch, paths) in on_branch.iter().filter(|(_, paths)| paths.len() > 1) {
                findings.push(Finding {
                    subject: format!("{}/{branch}", project.name),
                    message: format!(
                        "{} checkouts are on this branch, so naming a service by it is refused: {}. Say which with a path",
                        paths.len(),
                        paths
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    certain: false,
                });
            }

            for workspace in self.store.list_workspaces(&project.id)? {
                let declared = self.store.list_services(&workspace.id)?;

                for service in &declared {
                    let subject = format!("{}/{}", project.name, service.name);

                    for dependency in &service.depends_on {
                        if !declared.iter().any(|other| &other.name == dependency) {
                            findings.push(Finding {
                                subject: subject.clone(),
                                message: format!(
                                    "depends on '{dependency}', which this checkout does not declare"
                                ),
                                certain: true,
                            });
                        }
                    }

                    // Reuses the planner rather than a second cycle check, so
                    // the two cannot disagree about what a cycle is.
                    if !service.depends_on.is_empty() {
                        if let Err(error) =
                            crate::graph::plan(std::slice::from_ref(service), &declared, |_| false)
                        {
                            let message = error.to_string();
                            if message.contains("depend on each other") {
                                findings.push(Finding {
                                    subject: subject.clone(),
                                    message,
                                    certain: true,
                                });
                            }
                        }
                    }

                    // Two services in one checkout asking for the same port.
                    //
                    // Nothing refuses this and nothing said it: the second one
                    // to start finds the port taken and its policy decides —
                    // usually `allocate-next`, which quietly moves it. So the
                    // number somebody typed is not the number it runs on, the
                    // start succeeds, and there is nowhere to look. It is worth
                    // saying before it is started rather than after.
                    if let Some(port) = service.preferred_port.filter(|port| *port != runtime_types::ANY_PORT) {
                        let others: Vec<_> = declared
                            .iter()
                            .filter(|other| other.id != service.id)
                            .filter(|other| other.preferred_port == Some(port))
                            .map(|other| other.name.clone())
                            .collect();
                        if !others.is_empty() {
                            findings.push(Finding {
                                subject: subject.clone(),
                                message: format!(
                                    "asks for :{port}, and so does {}; whichever starts second will be moved somewhere else or refused",
                                    others.join(", ")
                                ),
                                certain: true,
                            });
                        }
                    }

                    if !command_is_findable(&service.command) {
                        let program = service
                            .command
                            .split_whitespace()
                            .next()
                            .unwrap_or(&service.command);
                        findings.push(Finding {
                            subject: subject.clone(),
                            message: format!(
                                "starts with '{program}', which is not on this daemon's PATH; \
                                 it will not start from here even though it works in a shell"
                            ),
                            certain: true,
                        });
                    }

                    if let Some(hazard) = self.build_hazard(service) {
                        findings.push(Finding {
                            subject: subject.clone(),
                            message: hazard.describe(),
                            // The overwrite only happens if it is started;
                            // the missing build fails whenever it next is.
                            certain: matches!(
                                hazard,
                                crate::builds::BuildHazard::MissingProductionBuild { .. }
                            ),
                        });
                    }
                }

                // A stack validates its steps when it is declared, and a service
                // can be renamed or removed afterwards.
                for stack in self.store.list_stacks(&workspace.id)? {
                    for step in &stack.members {
                        if !declared.iter().any(|service| &service.name == step) {
                            findings.push(Finding {
                                subject: format!("{}/{}", project.name, stack.name),
                                message: format!("step '{step}' is no longer a service here"),
                                certain: true,
                            });
                        }
                    }
                }
            }
        }

        // Deduplicated: one build directory shared by three services produces
        // the same sentence three times, which reads as three problems.
        findings.dedup_by(|a, b| a.subject == b.subject && a.message == b.message);
        Ok(findings)
    }

    /// The supervisor entry that would start this service, running or not.
    ///
    /// `ServiceView::supervisor_entry` is found through the pid holding the
    /// port, which answers only for a service that is already up — and the
    /// question that matters here is the other one: who should be asked to
    /// start it. So a stopped entry is matched by its working directory.
    ///
    /// Only when exactly one entry matches. Two entries in a directory is the
    /// same ambiguity that stops a port being adopted into two services, and
    /// picking one would be a guess about which of them somebody meant.
    pub fn supervised_entry_for(&self, service: &Service) -> Option<String> {
        if let Ok(view) = self.service_view(service) {
            if let Some(entry) = view.supervisor_entry {
                return Some(entry);
            }
        }

        let mut matches = self
            .pm2
            .processes()
            .into_iter()
            .filter(|entry| entry.cwd.as_deref() == Some(service.cwd.as_path()));
        let only = matches.next()?;
        match matches.next() {
            Some(_) => None,
            None => Some(only.name),
        }
    }

    // ---- build hazards ---------------------------------------------------

    /// What starting this service would do to a build directory in use.
    ///
    /// Everything that could be serving from the same directory counts as a
    /// neighbour, whoever started it — the damage does not care which of them
    /// the runtime owns.
    pub fn build_hazard(&self, service: &Service) -> Option<crate::builds::BuildHazard> {
        let Ok(workspace) = self.require_workspace(&service.workspace_id) else {
            return None;
        };
        let owners = self.port_owners().unwrap_or_default();

        let mut neighbours: Vec<crate::builds::Neighbour> = Vec::new();

        for other in self.store.list_services(&workspace.id).unwrap_or_default() {
            if other.id == service.id {
                continue;
            }
            let running = self
                .service_view_with(&other, &owners)
                .map(|view| view.status.is_live())
                .unwrap_or(false);
            neighbours.push(crate::builds::Neighbour {
                production: crate::builds::runs_in_production(&other.command, &other.env),
                directory: other.cwd.clone(),
                name: other.name,
                running,
            });
        }

        // A supervisor's entries are neighbours too, and on this machine they
        // are the ones most likely to be the production half of the pair.
        for entry in self.pm2.processes_in(&workspace.path) {
            let env: std::collections::BTreeMap<String, String> =
                entry.mode_environment.iter().cloned().collect();
            neighbours.push(crate::builds::Neighbour {
                production: entry.production
                    || crate::builds::runs_in_production(&entry.command, &env),
                directory: entry.cwd.unwrap_or_else(|| workspace.path.clone()),
                running: entry.status == "online",
                name: entry.name,
            });
        }

        crate::builds::hazard(&service.command, &service.env, &service.cwd, &neighbours)
    }

    // ---- taking things over ---------------------------------------------

    /// Write down what is on a port, so the runtime can start it again.
    ///
    /// The command comes from the process itself — its argv, or the launch that
    /// was recorded for it — and never from the project's scripts. Those are
    /// where the guessing happens: a checkout whose `dev` and `start` write to
    /// the same build directory is left unable to boot by adopting it under the
    /// wrong one, and the process table already knows which one is running.
    ///
    /// Nothing is stopped or started here. Adopting is about being *able* to,
    /// and a service that is serving traffic should not go down because someone
    /// asked the runtime to learn about it.
    /// Make sure a service adopting has touched can be started afterwards.
    ///
    /// Whichever stack already names it, or one made for it. Adopting exists so
    /// a running thing can be started again later, and a service in no stack
    /// cannot be started by name — so every path out of `adopt_port` has to
    /// leave it in one, not just the path that declares a new service. The
    /// first version of this only covered that path, and the test that calls
    /// `adopt_port` for real failed on the very case it was written for: a
    /// service already declared, being corrected.
    fn stack_for_adopted(
        &self,
        workspace_id: &WorkspaceId,
        name: &str,
        asked_for: Option<String>,
    ) -> Result<String> {
        let stacks = self.stacks_for(workspace_id)?;
        if asked_for.is_none() {
            if let Some(already) = stacks
                .iter()
                .find(|stack| stack.members.iter().any(|member| member == name))
            {
                return Ok(already.name.clone());
            }
        }
        let stack = asked_for.unwrap_or_else(|| name.to_string());
        let mut members = stacks
            .into_iter()
            .find(|candidate| candidate.name == stack)
            .map(|candidate| candidate.members)
            .unwrap_or_default();
        if !members.iter().any(|member| member == name) {
            members.push(name.to_string());
        }
        // Adopting a service into a stack says nothing about whether that stack
        // should come up at boot, so it leaves that alone.
        self.set_stack(workspace_id, &stack, members, None)?;
        Ok(stack)
    }

    pub fn adopt_port(
        &self,
        port: u16,
        force: bool,
        stack: Option<String>,
    ) -> Result<AdoptOutcome> {
        let owners = self.port_owners()?;
        let Some(owner) = owners.iter().find(|owner| owner.port == port) else {
            return Err(RuntimeError::invalid(format!("nothing is listening on {port}")));
        };

        if let Some(container) = &owner.container {
            return Err(RuntimeError::invalid(format!(
                "{port} is served by container '{container}'; compose owns what it is, and the runtime can already switch it on and off"
            )));
        }

        // Refused rather than warned: the caller is asking for a definition
        // they can restart from, and restarting something PM2 watches means
        // deleting it from PM2 — which usually changes what starts at boot.
        // That is a decision about the machine, not about this registry.
        if let Some(supervisor) = &owner.supervisor {
            if !force {
                let detail = self
                    .adapter
                    .process()
                    .list_processes()
                    .ok()
                    .and_then(|processes| {
                        crate::supervisors::detect(owner.pid, &processes, |candidate| {
                            self.adapter
                                .process()
                                .process_info(candidate)
                                .ok()
                                .flatten()
                                .map(|info| info.command_string())
                        })
                    })
                    .map(|found| found.taking_over)
                    .unwrap_or_else(|| "it will be restarted from there".to_string());
                return Err(RuntimeError::invalid(format!(
                    "{port} is kept alive by {supervisor}: {detail}. Pass --force to declare it here anyway"
                )));
            }
        }

        let Some(workspace_id) = owner.workspace_id.clone() else {
            return Err(RuntimeError::invalid(format!(
                "{port} does not resolve to a project the runtime knows about; add the project first"
            )));
        };
        let workspace = self.require_workspace(&workspace_id)?;

        // A recorded launch is the better source: it is what somebody asked
        // for, where argv is what that turned into after the shell and the
        // package manager were done with it.
        let recorded = self
            .launches
            .all()
            .into_iter()
            .find(|entry| entry.pid == Some(owner.pid));

        // A supervisor is a better source than the process for the same reason
        // it is a better source for the environment: it holds what it will run
        // next time. It is also the only source that survives the process
        // renaming itself — Next reports its argv as `next-server (v14.2.35)`,
        // which describes it accurately and cannot be executed.
        let supervised = self
            .pm2
            .processes()
            .into_iter()
            .find(|entry| entry.pid == Some(owner.pid));

        let (command, source) = match (&recorded, &supervised) {
            (Some(entry), _) => (entry.command.clone(), CommandSource::Recorded),
            (None, Some(entry)) => (entry.command.clone(), CommandSource::Supervisor),
            (None, None) => match &owner.command_line {
                Some(argv) if looks_runnable(argv) => {
                    (argv.trim().to_string(), CommandSource::ProcessArgv)
                }
                Some(argv) => {
                    return Err(RuntimeError::invalid(format!(
                        "the process on {port} reports itself as '{}', which describes it \
                         rather than starting it; declare the service by hand, or record a \
                         launch with the Claude Code hook",
                        argv.trim()
                    )))
                }
                None => {
                    return Err(RuntimeError::invalid(format!(
                        "the process on {port} will not say what it was started with"
                    )))
                }
            },
        };

        let cwd = owner.cwd.clone().unwrap_or_else(|| workspace.path.clone());

        // argv does not carry the environment, and for a great many services
        // the environment is the whole difference: the same `node server.mjs`
        // is the development server or the production one depending on
        // NODE_ENV, and they overwrite each other's build output. Adopting
        // without it produces a definition that looks exactly right and starts
        // the wrong thing.
        let env = self.mode_environment(owner);

        let declared = self.store.list_services(&workspace_id)?;

        if let Some(existing) = declared
            .iter()
            .find(|service| service.command == command && service.cwd == cwd)
        {
            // The command matching is not the same as the definition being
            // right: a service declared before the runtime read environments
            // has the correct command and no mode at all, which is precisely
            // the state that starts a production service in development mode.
            let missing: Vec<(String, String)> = env
                .iter()
                .filter(|(key, value)| existing.env.get(*key) != Some(*value))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            if missing.is_empty() {
                return Ok(AdoptOutcome {
                    stack: Some(self.stack_for_adopted(&existing.workspace_id, &existing.name, stack)?),
                    service: self.service_view(existing)?,
                    command_source: source,
                    declared: false,
                    replaced_command: None,
                    supervisor: owner.supervisor.clone(),
                });
            }

            let mut corrected = existing.clone();
            for (key, value) in missing {
                corrected.env.insert(key, value);
            }
            self.store.upsert_service(&corrected)?;
            self.announce_service(&corrected, false);
            return Ok(AdoptOutcome {
                stack: Some(self.stack_for_adopted(&corrected.workspace_id, &corrected.name, stack.clone())?),
                service: self.service_view(&corrected)?,
                command_source: source,
                declared: false,
                // The command did not change; what changed is the mode it runs
                // in, which the caller needs told just as loudly.
                replaced_command: Some(format!(
                    "the same command with no environment ({} added)",
                    corrected
                        .env
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                supervisor: owner.supervisor.clone(),
            });
        }

        // A service that already claims this port is this service, however
        // wrong its command is — and its command being wrong is the usual
        // reason to be here. Declaring a second one would leave two services
        // claiming one port, which the runtime resolves by adopting neither:
        // the row that was at least reporting the truth goes dark instead.
        //
        // So correct it. What is running is a fact; what was written down was
        // a guess, and this is the guess being replaced by the fact.
        if let Some(existing) = declared.into_iter().find(|service| {
            ports::PortResolver::preferred_port(service, &workspace) == Some(port)
        }) {
            let replaced = existing.command.clone();
            let mut corrected = existing;
            // A script keeps its script. `pnpm run dev:local` and the argv of
            // the process it spawned describe the same running service, and
            // only one of them starts it: the argv is what the package manager
            // ran *after* setting up an environment, and on its own it exits
            // immediately. Replacing the first with the second reads as making
            // the definition more accurate and leaves it unable to boot, which
            // is what happened to a service here.
            //
            // The port and the environment are still corrected — those are
            // facts about the run, and the run is what this reads.
            if !runs_through_a_script(&replaced) {
                corrected.command = command;
            }
            corrected.cwd = cwd;
            corrected.preferred_port = Some(port);
            // Mode variables replace whatever was there; anything else the
            // service was given by hand is left alone.
            for (key, value) in env {
                corrected.env.insert(key, value);
            }
            self.store.upsert_service(&corrected)?;
            self.announce_service(&corrected, false);
            return Ok(AdoptOutcome {
                stack: Some(self.stack_for_adopted(&corrected.workspace_id, &corrected.name, stack.clone())?),
                service: self.service_view(&corrected)?,
                command_source: source,
                declared: false,
                replaced_command: (replaced != corrected.command).then_some(replaced),
                supervisor: owner.supervisor.clone(),
            });
        }

        let name = self.unused_service_name(&workspace_id, &cwd, &workspace.path)?;
        let service_type = crate::detect::guess_type(&name, &command);
        let service = Service {
            id: ServiceId::new(),
            workspace_id,
            name,
            // Everything here is on a port, but not everything on a port
            // speaks HTTP, and the type decides how it gets checked. A
            // Postgres adopted as `Web` would be asked for a web page and
            // reported broken for declining.
            service_type,
            command,
            cwd,
            env,
            preferred_port: Some(port),
            health_check: None,
            auto_start: false,
            conflict_policy: ConflictPolicy::Reuse,
            depends_on: Vec::new(),
            one_shot: false,
            stop_signal: None,
            compose: None,
        };
        self.store.upsert_service(&service)?;
        self.announce_service(&service, false);

        // Into a stack, because that is what adopting is for. A service in no
        // stack cannot be started by name, and "so it can be started again
        // later" is the whole of this command's purpose — declaring one and
        // leaving it unstartable would undo that one step after succeeding.
        //
        // Its own, named after it, unless the caller said which. Nothing is
        // invented by this: running `adopt` is somebody saying they want this
        // managed, which is the declaration the rule asks for.
        let stack = self.stack_for_adopted(&workspace.id, &service.name, stack)?;

        Ok(AdoptOutcome {
            service: self.service_view(&service)?,
            command_source: source,
            declared: true,
            replaced_command: None,
            supervisor: owner.supervisor.clone(),
            stack: Some(stack),
        })
    }

    // ---- observed launches ---------------------------------------------

    /// Note a launch that is about to happen elsewhere.
    ///
    /// Called before the command runs, and deliberately does nothing to it. The
    /// runtime's answer to "a server was started without me" is not to take the
    /// launch away from whoever asked for it, but to be in a position to
    /// restart it later from the command that actually ran — which is the one
    /// thing that cannot be recovered from the process afterwards.
    pub fn record_launch(
        &self,
        command: String,
        cwd: PathBuf,
        source: StartedBy,
        session: Option<String>,
    ) -> Option<LaunchObservation> {
        if crate::launch::is_instantaneous(&command) {
            return None;
        }
        Some(self.launches.record(command, cwd, source, session))
    }

    /// Recorded launches, newest first.
    pub fn launches(&self) -> Vec<LaunchObservation> {
        self.launches.all()
    }

    /// Match recordings against what turned up listening.
    ///
    /// Runs off the back of a port scan the caller already paid for. A
    /// recording is only ever matched to a port that appeared after it, from a
    /// directory beneath the one it was announced in — everything else expires
    /// unclaimed, which is the correct outcome for the `git status` calls that
    /// make up most of what gets recorded.
    fn bind_launches(&self, owners: &[PortOwner]) {
        let pending = self.launches.pending();
        if pending.is_empty() {
            self.launches.sweep();
            return;
        }

        let already_bound: Vec<u32> = self.launches.bound_pids();

        for owner in owners {
            if already_bound.contains(&owner.pid) {
                continue;
            }
            // A port a declared service already accounts for needs no
            // explaining, and re-declaring it would duplicate the service.
            if self.port_is_declared(owner) {
                continue;
            }

            let Ok(Some(process)) = self.adapter.process().process_info(owner.pid) else {
                continue;
            };
            let started = chrono::DateTime::from_timestamp_millis(process.start_time_ms)
                .unwrap_or_else(Utc::now);

            let Some(entry) = pending
                .iter()
                .find(|entry| crate::launch::explains(entry, process.cwd.as_deref(), started))
            else {
                continue;
            };

            let service_id = self
                .declare_observed_service(entry, owner)
                .inspect_err(|error| {
                    tracing::debug!(%error, command = %entry.command, "could not declare an observed launch");
                })
                .ok()
                .flatten();

            self.launches
                .bind(&entry.id, owner.port, owner.pid, service_id);
        }

        self.launches.sweep();
    }

    /// Whether a declared service already explains this port.
    fn port_is_declared(&self, owner: &PortOwner) -> bool {
        let Some(workspace_id) = owner.workspace_id.as_ref() else {
            return false;
        };
        let Ok(services) = self.store.list_services(workspace_id) else {
            return false;
        };
        services.iter().any(|service| {
            let Ok(Some(workspace)) = self.store.get_workspace(&service.workspace_id) else {
                return false;
            };
            ports::PortResolver::preferred_port(service, &workspace) == Some(owner.port)
        })
    }

    /// Write down what a recorded launch turned out to be.
    ///
    /// Only inside a project the runtime already knows about. Registering a
    /// project because something served a port from it would turn every
    /// throwaway directory an agent runs a server in into a permanent entry.
    fn declare_observed_service(
        &self,
        entry: &LaunchObservation,
        owner: &PortOwner,
    ) -> Result<Option<ServiceId>> {
        let Some(workspace_id) = owner.workspace_id.clone() else {
            return Ok(None);
        };
        let workspace = self.require_workspace(&workspace_id)?;

        let cwd = owner
            .cwd
            .clone()
            .unwrap_or_else(|| entry.cwd.clone());

        // The same command from the same directory is the same service, however
        // many times it is launched.
        if let Some(existing) = self
            .store
            .list_services(&workspace_id)?
            .into_iter()
            .find(|service| service.command == entry.command && service.cwd == cwd)
        {
            return Ok(Some(existing.id));
        }

        let name = self.unused_service_name(&workspace_id, &cwd, &workspace.path)?;
        let service_type = crate::detect::guess_type(&name, &entry.command);
        let service = Service {
            id: ServiceId::new(),
            workspace_id,
            name,
            service_type,
            command: entry.command.clone(),
            cwd,
            // A recorded launch carries the command but not the environment the
            // shell had around it, and the mode it ran in is part of how it
            // runs. Read it off the process that answered.
            env: self.mode_environment(owner),
            preferred_port: Some(owner.port),
            health_check: None,
            auto_start: false,
            // Something is already on this port. Reusing it is the only policy
            // that does not fight the process that is serving.
            conflict_policy: ConflictPolicy::Reuse,
            depends_on: Vec::new(),
            one_shot: false,
            stop_signal: None,
            compose: None,
        };
        self.store.upsert_service(&service)?;
        self.announce_service(&service, false);
        Ok(Some(service.id))
    }

    /// The mode-selecting variables a running service was started with.
    ///
    /// Asked of PM2 first: it holds the environment it launched with, and
    /// reading it there works for an entry that is currently stopped, where
    /// the kernel has nothing to offer. Falls back to the process itself.
    ///
    /// Only the switches in `MODE_VARIABLES`. The rest of an environment is
    /// credentials, and the registry is not the place for those.
    fn mode_environment(&self, owner: &PortOwner) -> std::collections::BTreeMap<String, String> {
        let mut found = std::collections::BTreeMap::new();

        if let Some(pid) = Some(owner.pid).filter(|pid| *pid > 0) {
            if let Some(entry) = self
                .pm2
                .processes()
                .into_iter()
                .find(|process| process.pid == Some(pid))
            {
                for (key, value) in entry.mode_environment {
                    found.insert(key, value);
                }
            }

            if found.is_empty() {
                if let Ok(Some(pairs)) = self
                    .adapter
                    .process()
                    .environment(pid, crate::launch::MODE_VARIABLES)
                {
                    for (key, value) in pairs {
                        found.insert(key, value);
                    }
                }
            }
        }
        found
    }

    /// A name that reads like the directory and is not taken.
    fn unused_service_name(
        &self,
        workspace_id: &WorkspaceId,
        cwd: &Path,
        workspace_root: &Path,
    ) -> Result<String> {
        let base = cwd
            .strip_prefix(workspace_root)
            .ok()
            .and_then(|relative| relative.file_name())
            .or_else(|| cwd.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "service".to_string());

        let taken: Vec<String> = self
            .store
            .list_services(workspace_id)?
            .into_iter()
            .map(|service| service.name)
            .collect();

        if !taken.contains(&base) {
            return Ok(base);
        }
        for suffix in 2..100 {
            let candidate = format!("{base}-{suffix}");
            if !taken.contains(&candidate) {
                return Ok(candidate);
            }
        }
        Ok(format!("{base}-{}", ServiceId::new()))
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
        self.bind_launches(&owners);
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
            // The protocol is part of the identity: a port serving both TCP and
            // UDP is two things, and collapsing them hides one.
            if owners.iter().any(|owner| {
                owner.port == binding.port
                    && owner.protocol == binding.protocol
                    && Some(owner.pid) == binding.primary_pid()
            }) {
                continue;
            }
            let owner = resolver.owner_of_binding(&binding)?;
            if !owners.iter().any(|existing| {
                existing.port == owner.port
                    && existing.protocol == owner.protocol
                    && existing.pid == owner.pid
            }) {
                owners.push(owner);
            }
        }
        owners.sort_by_key(|owner| (owner.port, owner.protocol, owner.pid));
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
        // A container writes to Docker, not to a pipe the runtime holds, so
        // its output has to be fetched. Fetched *into* the log store rather
        // than returned around it, so that everything reading logs — the
        // cursor a caller pages with, `recent_errors`, the window — keeps
        // working off one source instead of two that disagree.
        self.pull_compose_logs(service_id)?;

        let lines = self.logs.read(service_id, max_lines, since_seq)?;
        if !lines.is_empty() {
            return Ok(lines);
        }

        // Only for a reader starting from the beginning. With a cursor, empty
        // means "nothing new", which is the ordinary state of a quiet service —
        // and answering it with a line would be worse than noise: the line has
        // no place in the sequence, so a caller that takes its `seq` as the new
        // cursor is sent backwards, and asks again for everything it has
        // already seen. The result is the whole log repeating, which reads as
        // the service repeating itself.
        if since_seq.is_some() {
            return Ok(lines);
        }

        // Reaching here means there is genuinely nothing. "(no output)" would
        // read as "it printed nothing", which is a different and misleading
        // claim about a service whose output simply goes somewhere else.
        let service = self.require_service(service_id)?;
        let view = self.service_view(&service)?;
        if view.status.is_live() && !view.managed {
            return Ok(vec![LogLine {
                seq: 0,
                service_id: service_id.clone(),
                stream: runtime_types::LogStream::System,
                timestamp: Utc::now(),
                message: "output is not captured: this service was not started by the runtime"
                    .to_string(),
            }]);
        }
        Ok(lines)
    }

    /// Drop log files for services that no longer exist.
    pub fn prune_logs(&self) -> Result<usize> {
        let ids: Vec<ServiceId> = self
            .store
            .all_services()?
            .into_iter()
            .map(|service| service.id)
            .collect();
        self.logs.prune(&ids)
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
    let resolved = std::fs::canonicalize(&expanded).map_err(|err| {
        RuntimeError::io(format!("cannot resolve {}: {err}", expanded.display()))
    })?;
    Ok(strip_verbatim(resolved))
}

/// Drop Windows' extended-length `\\?\` prefix.
///
/// `std::fs::canonicalize` always returns one. It is a legal path for most of
/// Win32 but not all of it — `cmd.exe` refuses it as a working directory, which
/// is how every service is launched — and nothing else reports paths that way,
/// so a root stored in that shape matches neither a process working directory
/// nor `git rev-parse` output.
/// Public because it is part of the contract rather than an internal tidy-up:
/// this is the spelling a path is *stored* under, so anything looking a
/// workspace up by path has to ask the same question the registry answered.
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return path;
    }
    let text = path.to_string_lossy().into_owned();
    // `\\?\UNC\server\share` is the verbatim spelling of `\\server\share`.
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

/// Why restarting a supervised entry would fail, when it would.
///
/// The pair of facts is what makes this knowable in advance: the entry runs in
/// production mode, and its build directory holds a development build. Next
/// writes both to `.next`, so a dev server run from the same checkout replaces
/// the production build with one that has no `BUILD_ID` — and the running
/// service does not notice, because it read what it needed at startup. The
/// failure appears at the next restart, long after the cause, which is exactly
/// the kind of gap this tool exists to close.
fn restart_warning(process: &crate::pm2::Pm2Process) -> Option<String> {
    if !process.production {
        return None;
    }
    let cwd = process.cwd.as_ref()?;
    let next = cwd.join(".next");
    if !next.is_dir() || next.join("BUILD_ID").exists() {
        return None;
    }
    Some(format!(
        "runs in production mode but {} holds a development build; restarting it will fail until `next build` is run",
        next.display()
    ))
}

/// Whether a declared command could be found at all, started from here.
///
/// A command is written in the shell that had it working, and run by a daemon
/// whose `PATH` is whatever launched the app. `python -m uvicorn ...` came off a
/// machine where `python` meant Anaconda's; started from here it means whatever
/// `PATH` says, which on a current macOS is nothing at all. The service is
/// declared, looks right, and fails the first time anybody presses Start.
///
/// Deliberately silent when it cannot tell. Anything with shell syntax in it is
/// run through `sh -c` and may resolve in ways this cannot follow, and a
/// warning that fires on working services is worse than no warning.
fn is_first_with_an_extension(found: &str, first: &str) -> bool {
    // On Windows a command is written without its extension and stored with
    // one: `node` on the command line is `node.exe` on disk. Comparing names
    // exactly would report every working command as missing.
    if !cfg!(windows) {
        return false;
    }
    let Some(stem) = found.rsplit_once('.').map(|(stem, _)| stem) else {
        return false;
    };
    stem.eq_ignore_ascii_case(first)
}

fn command_is_findable(command: &str) -> bool {
    /// Not on `PATH`, and perfectly runnable.
    const BUILTINS: &[&str] = &[
        "cd", "echo", "export", "exec", "set", "unset", "source", ".", "test", "[", "true",
        "false", "eval", "read", "printf", "wait", "trap", "shift", "return",
    ];

    let trimmed = command.trim();
    if trimmed.is_empty() {
        return true;
    }
    // A pipeline, a chain, a substitution, a redirect: `sh -c` territory.
    if trimmed.contains("&&")
        || trimmed.contains("||")
        || trimmed.contains('|')
        || trimmed.contains(';')
        || trimmed.contains('`')
        || trimmed.contains("$(")
        || trimmed.contains('>')
        || trimmed.contains('<')
    {
        return true;
    }

    let Some(first) = trimmed.split_whitespace().next() else {
        return true;
    };
    // `FOO=bar cmd` — the first word is an assignment, not the program.
    if first.contains('=') {
        return true;
    }
    if BUILTINS.contains(&first) {
        return true;
    }
    if first.contains('/') || first.contains('\\') {
        return std::path::Path::new(first).is_file();
    }
    looks_runnable(trimmed)
}

/// Whether a reported command line could actually start anything.
///
/// A process may rename itself, and the good ones do: `next-server (v14.2.35)`
/// and `PM2 v6.0.14: God Daemon` are far more useful in a process listing than
/// the paths they replaced. They are also not commands. Writing one into a
/// service definition produces a service that looks correctly declared and
/// cannot start — which is worse than declining to guess.
fn looks_runnable(command: &str) -> bool {
    // A worker a runtime forked for itself. Its argv is a real command with a
    // real interpreter in front, so every other test here passes it — and
    // running it starts a worker waiting on file descriptors that closed with
    // the parent, which is to say nothing at all. The service is the parent,
    // and its argv is not this.
    const FORKED: [&str; 3] = ["--multiprocessing-fork", "spawn_main(", "from multiprocessing"];
    if FORKED.iter().any(|mark| command.contains(mark)) {
        return false;
    }

    let Some(first) = command.split_whitespace().next() else {
        return false;
    };
    // An absolute path, or something a shell could find.
    if first.contains('/') || first.contains('\\') {
        return true;
    }
    // A bare word is only a command if it is on PATH. A title like
    // `next-server` is a bare word too, so this is the test that separates
    // them.
    //
    // Compared against the directory's real entries rather than by asking
    // whether the path exists: macOS is case-insensitive by default, so
    // `dir.join("PM2").is_file()` answers yes for a `pm2` that is nothing to
    // do with it — and `PM2 v6.0.14: God Daemon` would read as runnable.
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return false;
        };
        entries.flatten().any(|entry| {
            let found = entry.file_name();
            let found = found.to_string_lossy();
            found == first || is_first_with_an_extension(&found, first)
        })
    })
}

/// Place a group's members by what they wait for.
///
/// Edges come from the members' own `depends_on`, narrowed to the group: a
/// dependency outside it is still brought up, but it is not part of what
/// somebody declared and drawing it would make the group look bigger than it
/// is. Levels are waiting depth, so everything on one level can start at once
/// — which is the thing a list of steps could never say.
///
/// A cycle cannot survive here: `graph::plan` refuses one, so a group holding
/// it fails to run and says so. This only has to not spin, which it does by
/// giving up once a pass places nothing new and putting the rest on the last
/// level, where the reader can see them sitting on each other.
fn flow_of(stack: &Stack, services: &[ServiceView], missing: &[String]) -> Vec<FlowNode> {
    use std::collections::BTreeMap;

    let members: Vec<&str> = stack.members.iter().map(String::as_str).collect();
    let after: BTreeMap<&str, Vec<String>> = services
        .iter()
        .map(|view| {
            let waits = view
                .service
                .depends_on
                .iter()
                .filter(|name| members.contains(&name.as_str()))
                .cloned()
                .collect();
            (view.service.name.as_str(), waits)
        })
        .collect();

    let mut level: BTreeMap<&str, usize> = BTreeMap::new();
    loop {
        let mut placed = false;
        for name in &members {
            if level.contains_key(name) {
                continue;
            }
            let waits = after.get(name).map(Vec::as_slice).unwrap_or(&[]);
            if let Some(deepest) = waits
                .iter()
                .map(|dep| level.get(dep.as_str()).copied())
                .collect::<Option<Vec<_>>>()
            {
                let here = deepest.into_iter().max().map(|n| n + 1).unwrap_or(0);
                level.insert(name, here);
                placed = true;
            }
        }
        if !placed {
            break;
        }
    }
    // Anything left is in a cycle; show it after everything that is placed.
    let floor = level.values().copied().max().map(|n| n + 1).unwrap_or(0);

    let mut nodes: Vec<FlowNode> = stack
        .members
        .iter()
        .map(|name| {
            let view = services.iter().find(|view| &view.service.name == name);
            FlowNode {
                name: name.clone(),
                service_id: view.map(|view| view.service.id.clone()),
                after: after.get(name.as_str()).cloned().unwrap_or_default(),
                level: level.get(name.as_str()).copied().unwrap_or(floor),
                status: view.map(|view| view.status).unwrap_or(ServiceStatus::Stopped),
                one_shot: view.map(|view| view.service.one_shot).unwrap_or(false),
            }
        })
        .collect();

    // A step naming nothing still belongs on the diagram: a group with a hole
    // in it should look wrong rather than look smaller.
    let _ = missing;
    nodes.sort_by_key(|node| node.level);
    nodes
}

/// One repository written two ways is still one repository.
fn normalise_remote(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    // `git@host:owner/repo` and `https://host/owner/repo` differ only in how
    // they say where the host ends.
    let rest = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .rsplit_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(url);
    rest.replacen(':', "/", 1).to_lowercase()
}

/// Whether a command hands off to a package manager or task runner.
///
/// What such a command starts is not what it is: the process that ends up
/// holding the port is whatever the script ran, with an environment the script
/// set up. Its argv describes that process accurately and cannot reproduce it.
fn runs_through_a_script(command: &str) -> bool {
    const RUNNERS: [&str; 8] = ["npm", "pnpm", "yarn", "bun", "make", "just", "task", "cargo"];
    let mut words = command.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    let stem = std::path::Path::new(first)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    RUNNERS.contains(&stem.as_str())
}

#[cfg(test)]
mod tests {
    /// A command that hands off to a package manager keeps its own text.
    ///
    /// `pnpm run dev:local` and the argv of the process it spawned describe one
    /// running service, and only the first starts it — the second is what the
    /// script ran after setting up an environment, and on its own it exits at
    /// once. Adopting replaced the first with the second, which reads as making
    /// the definition more accurate and left the service unable to boot.
    #[test]
    fn a_script_is_not_replaced_by_what_it_ran() {
        for script in [
            "pnpm run dev:local",
            "npm run dev",
            "yarn dev",
            "/opt/homebrew/bin/pnpm run dev",
            "cargo run --bin api",
            "make serve",
        ] {
            assert!(super::runs_through_a_script(script), "{script}");
        }
        for direct in [
            "/usr/bin/python3 -m http.server 8000",
            "node server.mjs",
            "./target/release/api",
            "",
        ] {
            assert!(!super::runs_through_a_script(direct), "{direct}");
        }
    }

    /// A worker a runtime forked for itself is not how the service starts.
    ///
    /// Its argv has a real interpreter at the front and real arguments after
    /// it, so every other test for "could this start something" passes it.
    /// Adopting a multiprocessing service recorded the child's line, and
    /// starting it would wait on file descriptors that closed with the parent.
    #[test]
    fn a_forked_worker_is_not_a_command_that_starts_anything() {
        let worker = "/usr/bin/python3 -c from multiprocessing.spawn import spawn_main; \
                      spawn_main(tracker_fd=5, pipe_handle=7) --multiprocessing-fork";
        assert!(!super::looks_runnable(worker), "a fork worker read as runnable");
        assert!(super::looks_runnable("/usr/bin/python3 -m http.server 8000"));
    }

    use super::*;

    #[test]
    fn a_renamed_process_title_is_not_mistaken_for_a_command() {
        // The good ones rename themselves: `next-server (v14.2.35)` and
        // `PM2 v6.0.14: God Daemon` say far more in a process listing than the
        // paths they replaced. Writing one into a service definition produces
        // something that looks declared and cannot start.
        assert!(!looks_runnable("next-server (v14.2.35)"));
        assert!(!looks_runnable("PM2 v6.0.14: God Daemon (/Users/x/.pm2)"));
        assert!(!looks_runnable(""));
    }

    #[test]
    fn a_command_that_will_not_resolve_from_here_is_reported() {
        // Written in a shell where `python` meant Anaconda's, run by a daemon
        // where it means nothing.
        assert!(!command_is_findable("definitely-not-a-real-program --serve"));
    }

    /// A command line that runs through this platform's shell.
    ///
    /// `sh` is not a fact about every machine. Nothing on a stock Windows PATH
    /// is called that — `where sh` finds nothing unless somebody installed Git
    /// Bash — so a test asserting `sh -c '...'` is findable is really
    /// asserting it is running on a Unix box, and it failed the first time
    /// this suite ran anywhere else.
    ///
    /// What these tests mean is "a command that goes through a shell", which
    /// both platforms have and each spells its own way.
    fn through_a_shell(rest: &str) -> String {
        if cfg!(windows) {
            format!("cmd /c {rest}")
        } else {
            format!("sh -c '{rest}'")
        }
    }

    #[test]
    fn shell_syntax_is_left_alone() {
        // Shell territory: this cannot follow it, and a warning that fires on
        // working services is worse than no warning.
        let shelled = through_a_shell("exec thing");
        for command in [
            "cd frontend && pnpm dev",
            "NODE_ENV=production node server.mjs",
            "pnpm dev > log.txt",
            shelled.as_str(),
        ] {
            assert!(command_is_findable(command), "{command}");
        }
    }

    #[test]
    fn an_absolute_path_is_checked_as_a_file() {
        // Whatever this test is running as: the point is that a path is
        // checked as a path, and `/bin/sh` is only that on one platform.
        let me = std::env::current_exe().unwrap();
        assert!(command_is_findable(&format!("{} --help", me.display())));
        assert!(!command_is_findable("/nowhere/at/all/serve --port 3000"));
    }

    #[test]
    fn a_real_command_line_still_passes() {
        assert!(looks_runnable("/usr/local/bin/node server.mjs"));
        assert!(looks_runnable(&through_a_shell("pnpm dev")));
    }
}

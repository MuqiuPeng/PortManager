//! Port ownership and allocation.
//!
//! Two ideas carry this module. First, a port is answered as *who owns it*, not
//! as *which pid holds it* — the useful answer is "DOSSH/main/web". Second,
//! allocation is a lease taken before the process starts, which is what lets a
//! conflict be reported before a failed boot rather than after.

use std::path::Path;

use chrono::{Duration, Utc};
use runtime_adapter::PlatformAdapter;
use runtime_types::{
    ConflictPolicy, PortLease, PortLeaseStatus, PortOwner, PortReservation, PortStatus, Project,
    ProjectId, Result, RuntimeError, Service, StartedBy, Workspace,
};

use crate::docker::Docker;
use crate::store::Store;

/// How far above the preferred port `allocate-next` will search.
pub const ALLOCATION_SPAN: u16 = 100;

/// How long a reservation survives without a process claiming it.
pub const RESERVATION_TTL_SECONDS: i64 = 120;

pub struct PortResolver<'a> {
    store: &'a Store,
    adapter: &'a dyn PlatformAdapter,
    docker: &'a Docker,
}

impl<'a> PortResolver<'a> {
    pub fn new(store: &'a Store, adapter: &'a dyn PlatformAdapter, docker: &'a Docker) -> Self {
        Self {
            store,
            adapter,
            docker,
        }
    }

    /// The port a service should try first.
    ///
    /// Worktrees get a stable offset from the primary checkout, so
    /// `feature/refund` reliably lands on 3001 while `main` keeps 3000 — the
    /// same branch gets the same port on every machine and every restart.
    pub fn preferred_port(service: &Service, workspace: &Workspace) -> Option<u16> {
        service
            .preferred_port
            .map(|base| base.saturating_add(workspace.port_offset))
    }

    /// Who is listening on this port right now, resolved as far as possible
    /// back to a project.
    pub fn owner_of(&self, port: u16) -> Result<Option<PortOwner>> {
        let Some(binding) = self.adapter.port().binding_for(port)? else {
            return Ok(None);
        };
        let Some(pid) = binding.primary_pid() else {
            return Ok(Some(PortOwner {
                port,
                pid: 0,
                executable: None,
                cwd: None,
                command_line: None,
                project_id: None,
                project_name: None,
                workspace_id: None,
                git_branch: None,
                service_id: None,
                service_name: None,
                started_by: None,
                container: None,
                supervisor: None,
                managed: false,
            }));
        };

        let process = self.adapter.process().process_info(pid)?;
        let mut owner = PortOwner {
            port,
            pid,
            executable: process
                .as_ref()
                .and_then(|p| p.executable.as_ref())
                .map(|path| path.to_string_lossy().to_string()),
            cwd: process.as_ref().and_then(|p| p.cwd.clone()),
            command_line: process.as_ref().map(|p| p.command_string()),
            project_id: None,
            project_name: None,
            workspace_id: None,
            git_branch: None,
            service_id: None,
            service_name: None,
            started_by: None,
            container: None,
            supervisor: None,
            managed: false,
        };

        // Who else is already keeping this alive. Read here rather than at the
        // call sites so that every path producing a `PortOwner` carries it:
        // `check_port` and the whole-machine scan are different code, and a
        // fact known by only one of them is a fact the caller cannot rely on.
        if let Ok(processes) = self.adapter.process().list_processes() {
            owner.supervisor = crate::supervisors::detect(pid, &processes, |candidate| {
                self.adapter
                    .process()
                    .process_info(candidate)
                    .ok()
                    .flatten()
                    .map(|info| info.command_string())
            })
            .map(|found| found.kind);
        }

        // A process the runtime started is identified exactly, by pid and start
        // time. This is the only path that may set `managed`.
        //
        // The match is made against the whole ancestor chain, not just the pid
        // holding the socket: the runtime launches `sh -c "pnpm dev"`, and the
        // process that actually binds the port is a grandchild of that shell.
        let instances = self.store.live_instances()?;
        if !instances.is_empty() {
            let chain = self.ancestors(pid)?;
            for instance in instances {
                if !chain.contains(&instance.pid) {
                    continue;
                }
                let identity =
                    runtime_adapter::ProcessIdentity::new(instance.pid, instance.process_start_time);
                if !self.adapter.process().is_alive(&identity)? {
                    continue;
                }
                owner.managed = true;
                owner.started_by = Some(instance.started_by);
                owner.service_id = Some(instance.service_id.clone());
                if let Some(service) = self.store.get_service(&instance.service_id)? {
                    owner.service_name = Some(service.name.clone());
                    self.attach_workspace(&mut owner, &service.workspace_id)?;
                }
                break;
            }
        }

        // A container publishes through Docker's own process, so the pid and
        // its working directory describe Docker, not the service. Replace them
        // with what the container itself says.
        if owner.project_id.is_none() {
            if let Some(container) = self.docker.container_for_port(port) {
                owner.container = Some(container.name.clone());
                owner.service_name = Some(container.display_service().to_string());
                owner.command_line = Some(container.image.clone());
                owner.cwd = container.working_dir.clone();

                // The compose file's directory is a project root by the same
                // definition used for processes, so a registered project is
                // matched exactly; otherwise the compose project name is still
                // a better answer than Docker's.
                if let Some(directory) = &container.working_dir {
                    if let Some(workspace) = self.workspace_containing(directory)? {
                        owner.workspace_id = Some(workspace.id.clone());
                        owner.git_branch = workspace.git_branch.clone();
                        if let Some(project) = self.store.get_project(&workspace.project_id)? {
                            owner.project_id = Some(project.id);
                            owner.project_name = Some(project.name);
                        }
                    }
                }
                if owner.project_name.is_none() {
                    owner.project_name = container.compose_project.clone();
                }
                return Ok(Some(owner));
            }
        }

        // Otherwise fall back to the cwd, which still identifies the project
        // for anything started from a terminal inside the repo.
        if owner.project_id.is_none() {
            if let Some(cwd) = owner.cwd.clone() {
                if let Some(workspace) = self.workspace_containing(&cwd)? {
                    owner.workspace_id = Some(workspace.id.clone());
                    owner.git_branch = workspace.git_branch.clone();
                    if let Some(project) = self.store.get_project(&workspace.project_id)? {
                        owner.project_id = Some(project.id);
                        owner.project_name = Some(project.name);
                    }
                }
            }
        }

        Ok(Some(owner))
    }

    /// `pid` followed by its ancestors, nearest first.
    ///
    /// Built from a single process-table snapshot rather than repeated
    /// per-pid lookups, which on macOS would mean one megabyte-sized `sysctl`
    /// per generation.
    fn ancestors(&self, pid: u32) -> Result<Vec<u32>> {
        const MAX_DEPTH: usize = 16;

        let processes = self.adapter.process().list_processes()?;
        let mut chain = vec![pid];
        let mut current = pid;
        for _ in 0..MAX_DEPTH {
            let Some(process) = processes.iter().find(|p| p.pid == current) else {
                break;
            };
            let Some(parent) = process.parent_pid else {
                break;
            };
            // pid 1 is launchd/init, and a cycle would mean a corrupt table.
            if parent <= 1 || chain.contains(&parent) {
                break;
            }
            chain.push(parent);
            current = parent;
        }
        Ok(chain)
    }

    fn attach_workspace(
        &self,
        owner: &mut PortOwner,
        workspace_id: &runtime_types::WorkspaceId,
    ) -> Result<()> {
        let Some(workspace) = self.store.get_workspace(workspace_id)? else {
            return Ok(());
        };
        owner.workspace_id = Some(workspace.id.clone());
        owner.git_branch = workspace.git_branch.clone();
        if let Some(project) = self.store.get_project(&workspace.project_id)? {
            owner.project_id = Some(project.id);
            owner.project_name = Some(project.name);
        }
        Ok(())
    }

    /// The most specific registered workspace containing `path`.
    ///
    /// Longest match wins, so a worktree nested under its parent repository is
    /// not mistaken for the parent.
    fn workspace_containing(&self, path: &Path) -> Result<Option<Workspace>> {
        let mut best: Option<Workspace> = None;
        for workspace in self.store.all_workspaces()? {
            if !path.starts_with(&workspace.path) {
                continue;
            }
            let better = best
                .as_ref()
                .is_none_or(|current| workspace.path.components().count() > current.path.components().count());
            if better {
                best = Some(workspace);
            }
        }
        Ok(best)
    }

    pub fn status(&self, port: u16) -> Result<PortStatus> {
        let owner = self.owner_of(port)?;
        let lease = self.store.get_lease(port)?;
        let available = owner.is_none() && self.adapter.port().is_port_free(port)?;
        Ok(PortStatus {
            port,
            available,
            suggested_port: if available {
                None
            } else {
                self.next_free_port(port, None)?
            },
            owner,
            lease_status: lease.map(|l| l.status),
        })
    }

    /// First port at or above `from` that is neither bound nor leased.
    ///
    /// `exclude_service` lets a service skip its own lease when restarting.
    pub fn next_free_port(
        &self,
        from: u16,
        exclude_service: Option<&runtime_types::ServiceId>,
    ) -> Result<Option<u16>> {
        let leases = self.store.list_leases()?;
        for offset in 1..=ALLOCATION_SPAN {
            let Some(candidate) = from.checked_add(offset) else {
                break;
            };
            let leased = leases.iter().any(|lease| {
                lease.port == candidate
                    && exclude_service.is_none_or(|id| &lease.service_id != id)
                    && lease.status != PortLeaseStatus::Released
            });
            if leased {
                continue;
            }
            if self.adapter.port().is_port_free(candidate)? {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    /// Claim a port for a service, applying the conflict policy.
    ///
    /// The returned lease is `Reserved`; the lifecycle layer promotes it to
    /// `Active` once a process is actually bound.
    pub fn reserve(
        &self,
        project: &Project,
        workspace: &Workspace,
        service: &Service,
        requested: Option<u16>,
        policy: Option<ConflictPolicy>,
        owner: StartedBy,
    ) -> Result<PortReservation> {
        // Sweep abandoned reservations first, so a crashed agent's claim does
        // not permanently shift every later service one port up.
        self.store.expire_leases(Utc::now())?;

        let policy = policy.unwrap_or(service.conflict_policy);
        let preferred = requested.or_else(|| Self::preferred_port(service, workspace));
        let Some(preferred) = preferred else {
            return Err(RuntimeError::invalid(format!(
                "service '{}' has no port to reserve; set preferred_port or pass one explicitly",
                service.name
            )));
        };

        let conflict = self.owner_of(preferred)?;

        // A live reservation counts as occupying the port. Without this the
        // reservation does not actually reserve: between one agent claiming a
        // port and its process binding it, nothing is listening, so a second
        // agent asking at that moment is told the port is free and takes it too.
        let reserved_elsewhere = self
            .store
            .get_lease(preferred)?
            .filter(|lease| lease.service_id != service.id)
            .filter(|lease| lease.status != PortLeaseStatus::Released)
            .is_some();

        let free = conflict.is_none()
            && !reserved_elsewhere
            && self.adapter.port().is_port_free(preferred)?;

        if free {
            self.write_lease(project.id.clone(), workspace, service, preferred, true, owner)?;
            return Ok(PortReservation {
                port: preferred,
                preferred_port: Some(preferred),
                reallocated: false,
                policy,
                conflict: None,
            });
        }

        // The same service already holds the port: that is a reuse, not a
        // conflict, whatever the policy says.
        if let Some(existing) = &conflict {
            if existing.service_id.as_ref() == Some(&service.id) {
                return Ok(PortReservation {
                    port: preferred,
                    preferred_port: Some(preferred),
                    reallocated: false,
                    policy: ConflictPolicy::Reuse,
                    conflict: conflict.clone(),
                });
            }
        }

        // Name the reservation holder rather than saying "unknown process":
        // nothing is listening yet, so the process table cannot explain it.
        let holder = if conflict.is_none() && reserved_elsewhere {
            self.describe_lease(preferred)?
        } else {
            describe(conflict.as_ref())
        };

        match policy {
            ConflictPolicy::Reuse | ConflictPolicy::Fail => Err(RuntimeError::PortConflict {
                port: preferred,
                holder,
            }),

            // `Ask` reports the conflict without acting, leaving the decision
            // to the human or agent that asked.
            ConflictPolicy::Ask => Ok(PortReservation {
                port: preferred,
                preferred_port: Some(preferred),
                reallocated: false,
                policy,
                conflict,
            }),

            ConflictPolicy::AllocateNext => {
                let allocated = self
                    .next_free_port(preferred, Some(&service.id))?
                    .ok_or(RuntimeError::NoPortAvailable {
                        from: preferred,
                        to: preferred.saturating_add(ALLOCATION_SPAN),
                    })?;
                self.write_lease(project.id.clone(), workspace, service, allocated, false, owner)?;
                Ok(PortReservation {
                    port: allocated,
                    preferred_port: Some(preferred),
                    reallocated: true,
                    policy,
                    conflict,
                })
            }

            ConflictPolicy::KillExisting => {
                // Only processes the runtime started may be terminated to free
                // a port. An unknown process is never killed automatically —
                // this is the safety default the whole design rests on.
                let holder = conflict.as_ref().ok_or_else(|| RuntimeError::PortConflict {
                    port: preferred,
                    holder: "unknown".to_string(),
                })?;
                if !holder.managed {
                    return Err(RuntimeError::NotPermitted {
                        pid: holder.pid,
                        reason: format!(
                            "pid {} on port {preferred} was not started by the runtime",
                            holder.pid
                        ),
                    });
                }
                Ok(PortReservation {
                    port: preferred,
                    preferred_port: Some(preferred),
                    reallocated: false,
                    policy,
                    conflict,
                })
            }
        }
    }

    /// Who holds a reservation, for a conflict message.
    fn describe_lease(&self, port: u16) -> Result<String> {
        let Some(lease) = self.store.get_lease(port)? else {
            return Ok("a reservation".to_string());
        };
        let service = self
            .store
            .get_service(&lease.service_id)?
            .map(|service| service.name)
            .unwrap_or_else(|| "a service".to_string());
        let project = self
            .store
            .get_project(&lease.project_id)?
            .map(|project| project.name)
            .unwrap_or_default();
        let owner = serde_json::to_value(lease.owner)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        Ok(format!("a reservation held by {project}/{service} for {owner}"))
    }

    fn write_lease(
        &self,
        project_id: ProjectId,
        workspace: &Workspace,
        service: &Service,
        port: u16,
        preferred: bool,
        owner: StartedBy,
    ) -> Result<()> {
        let now = Utc::now();
        self.store.upsert_lease(&PortLease {
            port,
            project_id,
            workspace_id: workspace.id.clone(),
            service_id: service.id.clone(),
            preferred,
            status: PortLeaseStatus::Reserved,
            owner,
            created_at: now,
            expires_at: Some(now + Duration::seconds(RESERVATION_TTL_SECONDS)),
        })
    }

    /// Promote a reservation to active once a process is bound to it.
    pub fn activate(&self, port: u16) -> Result<()> {
        if let Some(mut lease) = self.store.get_lease(port)? {
            lease.status = PortLeaseStatus::Active;
            lease.expires_at = None;
            self.store.upsert_lease(&lease)?;
        }
        Ok(())
    }
}

/// A one-line description of a port's holder, used in every conflict message
/// so the CLI, the daemon and MCP all name the holder identically.
pub fn describe(owner: Option<&PortOwner>) -> String {
    let Some(owner) = owner else {
        return "an unknown process".to_string();
    };
    match (&owner.project_name, &owner.service_name) {
        (Some(project), Some(service)) => {
            let branch = owner.git_branch.as_deref().unwrap_or("-");
            format!("{project}/{branch}/{service} (pid {})", owner.pid)
        }
        (Some(project), None) => format!("{project} (pid {})", owner.pid),
        _ => {
            let exe = owner
                .executable
                .as_deref()
                .map(Path::new)
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            format!("{exe} (pid {})", owner.pid)
        }
    }
}

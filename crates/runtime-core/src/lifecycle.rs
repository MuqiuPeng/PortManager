//! Process lifecycle.
//!
//! The rules that matter here:
//!
//! * a port is leased *before* the process starts, so conflicts surface as a
//!   clear answer instead of a failed boot;
//! * termination always goes through [`ProcessIdentity`], never a bare pid;
//! * stopping escalates from graceful to forceful on a timer, and reaches the
//!   whole process tree, because dev servers fork.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use runtime_adapter::{PlatformAdapter, ProcessIdentity, TerminationMode};
use runtime_types::{
    ConflictPolicy, HealthCheck, HealthReport, InstanceId, LogStream, PortReservation, Result, RuntimeError, RuntimeInstance, Service, ServiceId, ServiceStatus, ServiceType, ServiceView, SessionId, StartOutcome, StartedBy, WorkspaceId,
};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::events::{EventBus, RuntimeEvent};
use crate::store::Store;
use crate::supervisor::RunningProcess;
use crate::Runtime;

/// How long a graceful stop is given before escalating.
pub const GRACEFUL_TIMEOUT: Duration = Duration::from_secs(8);

/// How long to watch a freshly started service before calling it started.
///
/// Long enough to catch the common failures — a missing binary, a port already
/// taken, a syntax error — and short enough that starting something healthy
/// does not feel slow.
pub const START_VERIFY: Duration = Duration::from_millis(1_500);

/// How long the log pumps keep reading after the process they follow has gone.
///
/// Longer than their poll interval, so a final line written just before the
/// exit is still picked up, and short enough that a service restarted promptly
/// does not overlap with its own predecessor.
const CAPTURE_DRAIN: Duration = Duration::from_millis(600);

/// How long a dependency is given to become healthy before giving up.
///
/// Generous, because the thing being waited for is a database accepting its
/// first connection or a compiler finishing a cold build, and the alternative
/// to waiting is starting the next service against something not ready.
pub const DEPENDENCY_TIMEOUT: Duration = Duration::from_secs(90);

/// How long a one-shot step may take before it is treated as hung.
///
/// A migration on a large database is minutes; a seed script that has stopped
/// making progress is forever. This is the line between them.
pub const ONE_SHOT_TIMEOUT: Duration = Duration::from_secs(600);

/// How long after start the runtime keeps probing before calling a service
/// unhealthy rather than starting.
pub const STARTUP_GRACE: Duration = Duration::from_secs(60);

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Options for one start.
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    pub started_by: StartedBy,
    pub session: Option<SessionId>,
    /// Overrides the service's preferred port for this start only.
    pub port: Option<u16>,
    pub conflict_policy: Option<ConflictPolicy>,
}

impl Runtime {
    /// Start a service, or adopt the instance that is already running.
    /// Bring a service up, and whatever it needs, in order.
    ///
    /// A dependency already running is left exactly as it is — under PM2, in a
    /// terminal, or from an earlier session. Restarting it would take a working
    /// service down to reach the state it was already in, and on this machine
    /// would lose a race for the port with whoever is supervising it.
    pub async fn start_service(
        &self,
        service_id: &ServiceId,
        options: StartOptions,
    ) -> Result<StartOutcome> {
        let service = self.require_service(service_id)?;

        // Worked out before anything happens, and reported by every path out of
        // here. The damage is done by the start itself; afterwards the only
        // evidence is a build directory that looks fine until something else
        // restarts and cannot find what it needs.
        let warning = self.build_hazard(&service).map(|hazard| {
            let text = hazard.describe();
            let _ = self.logs_arc().append(
                &service.id,
                LogStream::System,
                format!("warning: {text}"),
            );
            tracing::warn!(service = %service.name, "{text}");
            text
        });

        // Asking to "start" a step that runs to completion means running it.
        // Falling through to the ordinary path would spawn it, watch it exit
        // the moment it succeeded, and record that as a failure.
        if service.one_shot {
            for name in &service.depends_on {
                let declared = self.store().list_services(&service.workspace_id)?;
                let dependency = declared
                    .iter()
                    .find(|candidate| &candidate.name == name)
                    .ok_or_else(|| {
                        RuntimeError::invalid(format!(
                            "'{}' depends on '{name}', which this checkout does not declare",
                            service.name
                        ))
                    })?;
                if !self.service_view(dependency)?.status.is_live() {
                    Box::pin(self.start_service(&dependency.id, StartOptions::default())).await?;
                    self.wait_until_healthy(&dependency.id, DEPENDENCY_TIMEOUT).await?;
                }
            }

            self.run_to_completion(&service.id).await?;
            return Ok(StartOutcome {
                service: self.service_view(&service)?,
                reused: false,
                reservation: None,
                warning,
            });
        }

        if !service.depends_on.is_empty() {
            let declared = self.store().list_services(&service.workspace_id)?;
            let owners = self.port_owners()?;
            let live: Vec<ServiceId> = declared
                .iter()
                .filter(|candidate| {
                    self.service_view_with(candidate, &owners)
                        .map(|view| view.status.is_live())
                        .unwrap_or(false)
                })
                .map(|candidate| candidate.id.clone())
                .collect();

            let plan = crate::graph::plan(std::slice::from_ref(&service), &declared, |candidate| {
                live.contains(&candidate.id)
            })?;

            // Everything but the service itself, which the rest of this
            // function starts in the ordinary way.
            for step in plan.iter().filter(|step| step.service_id != service.id) {
                if !step.needs_start {
                    continue;
                }
                if step.one_shot {
                    self.run_to_completion(&step.service_id).await?;
                } else {
                    // A dependency somebody else supervises cannot be started
                    // here — the runtime would spawn a second copy beside the
                    // one that supervisor is about to bring back. Ask them
                    // instead, which is a start that sticks.
                    let dependency = self.require_service(&step.service_id)?;
                    match self.supervised_entry_for(&dependency) {
                        Some(entry) => {
                            self.control_supervised(&entry, crate::pm2::Pm2Action::Start)?;
                        }
                        None => {
                            Box::pin(
                                self.start_service(&step.service_id, StartOptions::default()),
                            )
                            .await?;
                        }
                    }
                    // A dependency that is up but not yet answering is not a
                    // dependency met: the whole point of the ordering is that
                    // the next service can talk to it.
                    self.wait_until_healthy(&step.service_id, DEPENDENCY_TIMEOUT)
                        .await?;
                }
            }
        }

        let workspace = self.require_workspace(&service.workspace_id)?;
        let project = self.require_project(&workspace.project_id)?;

        // Already running is not an error: an agent asking twice should get the
        // running service back, not a second copy of it. That has to include a
        // service found already listening — otherwise the one case where a
        // second copy does real damage is the one case not covered, since the
        // duplicate arrives with different arguments than the process that is
        // already serving.
        let view = self.service_view(&service)?;
        if view.status.is_live() && !view.managed {
            let reservation = view.actual_port.map(|port| PortReservation {
                port,
                preferred_port: crate::ports::PortResolver::preferred_port(&service, &workspace),
                reallocated: false,
                policy: ConflictPolicy::Reuse,
                conflict: None,
            });
            return Ok(StartOutcome {
                service: view,
                reused: true,
                reservation,
                warning,
            });
        }

        // Said before the spawn, not after: the damage is done by the start
        // itself, and afterwards the only evidence is a build directory that
        // looks fine until something else restarts.
        let (status, instance) = self.current_state(&service)?;
        if status.is_live() {
            if let Some(instance) = instance {
                return Ok(StartOutcome {
                    service: self.service_view(&service)?,
                    reused: true,
                    reservation: instance.port.map(|port| {
                        // The preferred port includes the workspace offset, so
                        // a worktree reusing 3001 is not reported as having
                        // wanted 3000.
                        let preferred = crate::ports::PortResolver::preferred_port(&service, &workspace);
                        PortReservation {
                            port,
                            preferred_port: preferred,
                            reallocated: Some(port) != preferred,
                            policy: ConflictPolicy::Reuse,
                            conflict: None,
                        }
                    }),
                    warning,
                });
            }
        }

        // Services without a port (workers, one-shots) skip leasing entirely.
        let reservation = if service.preferred_port.is_some() || options.port.is_some() {
            Some(self.resolver().reserve(
                &project,
                &workspace,
                &service,
                options.port,
                options.conflict_policy,
                options.started_by,
            )?)
        } else {
            None
        };

        // `Ask` returns the conflict without starting anything.
        if let Some(reservation) = &reservation {
            if reservation.policy == ConflictPolicy::Ask && reservation.conflict.is_some() {
                return Err(RuntimeError::PortConflict {
                    port: reservation.port,
                    holder: crate::ports::describe(reservation.conflict.as_ref()),
                });
            }
        }

        // `KillExisting` is only reachable for a process the runtime started;
        // `PortResolver::reserve` has already refused anything else.
        if let Some(reservation) = &reservation {
            if reservation.policy == ConflictPolicy::KillExisting {
                if let Some(conflict) = &reservation.conflict {
                    if let Some(other) = &conflict.service_id {
                        self.stop_service(other, GRACEFUL_TIMEOUT).await?;
                    }
                }
            }
        }

        let port = reservation.as_ref().map(|r| r.port);
        let instance = self.spawn_service(&service, port, &options).await?;

        // Reporting success the moment a process is spawned means reporting
        // success for a process that is already dead. The common failures —
        // a missing command, a port taken by something that ignores $PORT, a
        // syntax error — all happen within the first second.
        self.verify_started(&service, &instance).await?;

        // What is listening just changed; a cached answer would be wrong.
        self.invalidate_port_owners();
        if let Some(port) = port {
            self.resolver().activate(port)?;
            self.events.publish(RuntimeEvent::PortLeaseChanged {
                port,
                service_id: service.id.clone(),
            });
        }
        self.events.publish(RuntimeEvent::ServiceStatusChanged {
            service_id: service.id.clone(),
            status: instance.status,
            port,
        });

        Ok(StartOutcome {
            service: self.service_view(&service)?,
            reused: false,
            reservation,
            warning,
        })
    }

    /// Stop everything a task started, in the reverse of the order it started.
    ///
    /// Reverse because the order was there for a reason: a front end talking to
    /// an API that has already gone spends its last moments logging failures
    /// nobody asked for, and a database pulled out from under both is worse.
    ///
    /// A member the runtime did not start is left alone, as everywhere else,
    /// and a member already stopped is not an error — stopping a group is a
    /// statement about where things should end up, not about each step.
    pub async fn stop_task(&self, workspace_id: &WorkspaceId, name: &str) -> Result<Vec<String>> {
        let task = self
            .store()
            .list_tasks(workspace_id)?
            .into_iter()
            .find(|task| task.name == name)
            .ok_or_else(|| RuntimeError::invalid(format!("no task called '{name}' here")))?;

        let declared = self.store().list_services(workspace_id)?;
        let mut stopped = Vec::new();

        for step in task.steps.iter().rev() {
            let Some(service) = declared.iter().find(|service| &service.name == step) else {
                continue;
            };
            let view = self.service_view(service)?;
            if !view.status.is_live() {
                continue;
            }
            if !view.managed {
                stopped.push(format!("{step} (not ours to stop)"));
                continue;
            }
            self.stop_service(&service.id, GRACEFUL_TIMEOUT).await?;
            stopped.push(step.clone());
        }
        Ok(stopped)
    }

    /// Run a named task: each step brought up in order.
    ///
    /// Each step resolves its own dependencies, so a step already covered by an
    /// earlier one does nothing. Failure stops the task where it failed rather
    /// than carrying on: the later steps are there because the earlier ones
    /// were supposed to have worked.
    pub async fn run_task(&self, workspace_id: &WorkspaceId, name: &str) -> Result<Vec<String>> {
        // The definition lives on the project's main checkout; the services it
        // names are resolved in whichever checkout this is being run in.
        let workspace = self.require_workspace(workspace_id)?;
        let declared_in = self
            .store()
            .list_workspaces(&workspace.project_id)?
            .into_iter()
            .find(|candidate| !candidate.worktree)
            .map(|candidate| candidate.id)
            .unwrap_or_else(|| workspace_id.clone());

        let task = self
            .store()
            .list_tasks(&declared_in)?
            .into_iter()
            .find(|task| task.name == name)
            .ok_or_else(|| RuntimeError::invalid(format!("no task called '{name}' here")))?;

        let declared = self.store().list_services(workspace_id)?;
        let mut done = Vec::new();

        for step in &task.steps {
            let service = declared
                .iter()
                .find(|service| &service.name == step)
                .ok_or_else(|| {
                    RuntimeError::invalid(format!("'{step}' is no longer a service here"))
                })?;

            if service.one_shot {
                self.run_to_completion(&service.id).await?;
                done.push(format!("{step} (ran)"));
                continue;
            }

            let outcome = self.start_service(&service.id, StartOptions::default()).await?;
            self.wait_until_healthy(&service.id, DEPENDENCY_TIMEOUT).await?;
            done.push(if outcome.reused {
                format!("{step} (already up)")
            } else {
                step.clone()
            });
        }
        Ok(done)
    }

    /// Run a step to completion and require that it succeed.
    ///
    /// The opposite test from a service: a server that exits has failed, and a
    /// migration that keeps running has hung. Sharing one notion of "started"
    /// between the two would make one of them permanently wrong, which is why
    /// `one_shot` exists rather than a shorter health check.
    ///
    /// The run is recorded, though there is nothing left to stop afterwards.
    /// "Did the migration work?" is the question a step like this exists to
    /// answer, and without a record the answer is whatever the last attempt
    /// left behind — which is how a run that succeeded goes on reporting the
    /// failure before it.
    pub async fn run_to_completion(&self, service_id: &ServiceId) -> Result<()> {
        let service = self.require_service(service_id)?;
        let workspace = self.require_workspace(&service.workspace_id)?;

        let mut command = self.adapter().spawn().build(&service.command)?;
        command.current_dir(&service.cwd);
        let dotenv = crate::dotenv::load(&workspace.path, &service.cwd);
        for (key, value) in &dotenv.variables {
            command.env(key, value);
        }
        for (key, value) in &service.env {
            command.env(key, value);
        }
        command.env("LOCAL_RUNTIME_SERVICE", &service.name);
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let child = command.output();

        let output = match tokio::time::timeout(ONE_SHOT_TIMEOUT, child).await {
            Ok(result) => result.map_err(|err| {
                RuntimeError::io(format!("failed to run '{}': {err}", service.command))
            })?,
            Err(_) => {
                return Err(RuntimeError::StartFailed {
                    service: service.name.clone(),
                    exit_code: None,
                    detail: format!(
                        "still running after {}s",
                        ONE_SHOT_TIMEOUT.as_secs()
                    ),
                })
            }
        };

        let started_at = Utc::now();

        // Kept, not just returned. A step that runs to completion produces its
        // whole account of itself in one go and then is gone; without this the
        // only place it ever existed was the error of the call that ran it, so
        // asking afterwards what a migration said had no answer at all.
        for (stream, raw) in [
            (LogStream::Stdout, &output.stdout),
            (LogStream::Stderr, &output.stderr),
        ] {
            for line in String::from_utf8_lossy(raw).lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let _ = self.logs_arc().append(&service.id, stream, line.to_string());
            }
        }

        if output.status.success() {
            self.record_run(&service, started_at, Some(0), ServiceStatus::Stopped)?;
            return Ok(());
        }
        self.record_run(
            &service,
            started_at,
            output.status.code(),
            ServiceStatus::Failed,
        )?;

        // The last thing it printed is normally the reason, and a step that
        // failed silently in the middle of a start sequence is the worst case
        // to debug.
        let mut detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if detail.is_empty() {
            detail = String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
        let detail = detail
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");

        Err(RuntimeError::StartFailed {
            service: service.name.clone(),
            exit_code: output.status.code(),
            detail,
        })
    }

    /// Note that a one-shot ran, and how it went.
    fn record_run(
        &self,
        service: &Service,
        started_at: chrono::DateTime<Utc>,
        exit_code: Option<i32>,
        status: ServiceStatus,
    ) -> Result<()> {
        let instance = RuntimeInstance {
            id: InstanceId::new(),
            service_id: service.id.clone(),
            // No live process to identify: it has already exited, and a pid
            // that has been reused would make this look alive.
            pid: 0,
            process_start_time: 0,
            status,
            port: None,
            started_at,
            stopped_at: Some(Utc::now()),
            exit_code,
            started_by: StartedBy::Unknown,
            owner_session: None,
        };
        self.store().insert_instance(&instance)?;
        self.events().publish(RuntimeEvent::ServiceStatusChanged {
            service_id: service.id.clone(),
            status,
            port: None,
        });
        Ok(())
    }

    async fn spawn_service(
        &self,
        service: &Service,
        port: Option<u16>,
        options: &StartOptions,
    ) -> Result<RuntimeInstance> {
        let mut command = self.adapter().spawn().build(&service.command)?;
        command.current_dir(&service.cwd);

        // `.env` first, then whatever the service declares: an explicit value
        // in the registry is a correction, and a correction has to win.
        let workspace = self.require_workspace(&service.workspace_id)?;
        let dotenv = crate::dotenv::load(&workspace.path, &service.cwd);
        for (key, value) in &dotenv.variables {
            command.env(key, value);
        }
        for (key, value) in &service.env {
            command.env(key, value);
        }
        if let Some(port) = port {
            // The near-universal convention, and the reason a service can be
            // moved to another port without editing its command.
            command.env("PORT", port.to_string());
        }
        command.env("LOCAL_RUNTIME_SERVICE", &service.name);
        command.env("LOCAL_RUNTIME_SERVICE_ID", service.id.as_str());
        command.stdin(Stdio::null());

        // A file, not a pipe. A pipe has a read end, and that read end belongs
        // to the daemon: when the daemon dies the pipe breaks and the next
        // thing the service prints kills it with SIGPIPE. Capturing output must
        // not put the daemon in the service's critical path.
        let capture = self.logs_arc().capture_paths(&service.id);
        match &capture {
            Some((out, err)) => {
                command.stdout(Stdio::from(open_capture(out)?));
                command.stderr(Stdio::from(open_capture(err)?));
            }
            // No log directory — an in-memory store, which is what tests use.
            None => {
                command.stdout(Stdio::piped());
                command.stderr(Stdio::piped());
            }
        }

        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(false);
        let mut child = command.spawn().map_err(|err| {
            RuntimeError::io(format!("failed to start '{}': {err}", service.command))
        })?;

        let pid = child
            .id()
            .ok_or_else(|| RuntimeError::internal("child exited before it could be recorded"))?;

        // Read the start time back from the OS rather than using "now": the
        // stored identity must match exactly what a later lookup will report.
        let process_start_time = self
            .adapter()
            .process()
            .process_info(pid)?
            .map(|info| info.start_time_ms)
            .unwrap_or_else(|| Utc::now().timestamp_millis());

        let instance = RuntimeInstance {
            id: InstanceId::new(),
            service_id: service.id.clone(),
            pid,
            process_start_time,
            status: ServiceStatus::Starting,
            port,
            started_at: Utc::now(),
            stopped_at: None,
            exit_code: None,
            started_by: options.started_by,
            owner_session: options.session.clone(),
        };
        self.store().insert_instance(&instance)?;

        // A previous run's readers are drained rather than cut off, which
        // leaves them alive for a moment after it ends. This run appends to
        // the same capture file, so a start landing inside that moment would
        // be read by both — every line twice. Whatever the last run still had
        // to say, it has had until now to say it.
        self.supervisor().stop_parked(&service.id)?;

        self.logs_arc().append(
            &service.id,
            LogStream::System,
            format!(
                "starting `{}` in {} (pid {pid}{})",
                service.command,
                service.cwd.display(),
                port.map(|p| format!(", port {p}")).unwrap_or_default()
            ),
        )?;
        // Never silent: a service behaving differently because of a file nobody
        // mentioned is worse than one missing a variable.
        if !dotenv.is_empty() {
            self.logs_arc()
                .append(&service.id, LogStream::System, dotenv.describe())?;
        }

        let mut tasks = Vec::new();
        match &capture {
            Some((out, err)) => {
                // Tail from where the file already ends: a restarted service
                // appends to the same file, and its predecessor's output has
                // been ingested already.
                tasks.push(self.tail_capture(service.id.clone(), out.clone(), LogStream::Stdout));
                tasks.push(self.tail_capture(service.id.clone(), err.clone(), LogStream::Stderr));
            }
            None => {
                if let Some(stdout) = child.stdout.take() {
                    tasks.push(self.pump_logs(service.id.clone(), stdout, LogStream::Stdout));
                }
                if let Some(stderr) = child.stderr.take() {
                    tasks.push(self.pump_logs(service.id.clone(), stderr, LogStream::Stderr));
                }
            }
        }
        tasks.push(self.watch_exit(service.clone(), instance.clone(), child));
        tasks.push(self.watch_health(service.clone(), instance.clone()));

        self.supervisor().insert(
            service.id.clone(),
            RunningProcess {
                instance_id: instance.id.clone(),
                identity: ProcessIdentity::new(pid, process_start_time),
                port,
                tasks,
            },
        )?;

        Ok(instance)
    }

    /// Fail loudly if the process is gone almost immediately.
    async fn verify_started(&self, service: &Service, instance: &RuntimeInstance) -> Result<()> {
        let identity = ProcessIdentity::new(instance.pid, instance.process_start_time);

        // Poll rather than sleep the whole grace: a command that fails should
        // say so at once, not after a fixed pause.
        let deadline = tokio::time::Instant::now() + START_VERIFY;
        loop {
            if !self.adapter().process().is_alive(&identity)? {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // The process is gone; its last output is almost always the reason.
        // Output is pumped on another task, which may not have caught up with a
        // process that died this quickly — so give it a moment rather than
        // reporting "no output" for something that printed the answer.
        let mut detail = None;
        for wait in [150, 350] {
            tokio::time::sleep(Duration::from_millis(wait)).await;
            detail = self
                .read_logs(&service.id, 8, None)
                .unwrap_or_default()
                .iter()
                .rev()
                .find(|line| line.stream != LogStream::System && !line.message.trim().is_empty())
                .map(|line| line.message.trim().to_string());
            if detail.is_some() {
                break;
            }
        }
        let detail = detail.unwrap_or_else(|| "it produced no output".to_string());

        let exit_code = self
            .store()
            .get_instance(&instance.id)?
            .and_then(|stored| stored.exit_code);

        Err(RuntimeError::StartFailed {
            service: service.name.clone(),
            exit_code,
            detail,
        })
    }

    /// Follow a capture file, turning new lines into log entries.
    ///
    /// Polling rather than watching: the file is local, appended by one writer,
    /// and a filesystem watcher would be a dependency and a permission for
    /// something a 150ms read already does.
    fn tail_capture(
        &self,
        service_id: ServiceId,
        path: std::path::PathBuf,
        stream: LogStream,
    ) -> tokio::task::JoinHandle<()> {
        use std::io::{Read, Seek, SeekFrom};

        let logs = self.logs_arc();
        let events = self.events().clone();

        tokio::spawn(async move {
            // Start at the end: whatever is already in the file belongs to an
            // earlier run and has been ingested.
            let mut offset = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            let mut partial = String::new();

            loop {
                tokio::time::sleep(Duration::from_millis(150)).await;

                let Ok(mut file) = std::fs::File::open(&path) else {
                    continue;
                };
                let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
                if length < offset {
                    // Truncated or rotated underneath us; start over.
                    offset = 0;
                    partial.clear();
                }
                if length == offset {
                    continue;
                }
                if file.seek(SeekFrom::Start(offset)).is_err() {
                    continue;
                }

                let mut buffer = Vec::new();
                if file.read_to_end(&mut buffer).is_err() {
                    continue;
                }
                offset += buffer.len() as u64;
                partial.push_str(&String::from_utf8_lossy(&buffer));

                // Whatever follows the last newline is an unfinished line; keep
                // it until the rest arrives rather than splitting a message.
                let tail = match partial.rfind('\n') {
                    Some(index) => partial.split_off(index + 1),
                    None => continue,
                };
                let complete = std::mem::replace(&mut partial, tail);

                for line in complete.lines() {
                    match logs.append(&service_id, stream, line.to_string()) {
                        Ok(entry) => events.publish(RuntimeEvent::Log(entry)),
                        Err(err) => {
                            tracing::warn!(%err, "dropping log line");
                            return;
                        }
                    }
                }
            }
        })
    }

    fn pump_logs<R>(
        &self,
        service_id: ServiceId,
        reader: R,
        stream: LogStream,
    ) -> tokio::task::JoinHandle<()>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let logs = self.logs_arc();
        let events = self.events().clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match logs.append(&service_id, stream, line) {
                    Ok(entry) => events.publish(RuntimeEvent::Log(entry)),
                    Err(err) => {
                        tracing::warn!(%err, "dropping log line");
                        break;
                    }
                }
            }
        })
    }

    fn watch_exit(
        &self,
        service: Service,
        mut instance: RuntimeInstance,
        mut child: tokio::process::Child,
    ) -> tokio::task::JoinHandle<()> {
        let store = self.store_arc();
        let logs = self.logs_arc();
        let events = self.events().clone();
        let supervisor = self.supervisor_arc();

        tokio::spawn(async move {
            let status = child.wait().await;
            let exit_code = status.as_ref().ok().and_then(|s| s.code());

            instance.stopped_at = Some(Utc::now());
            instance.exit_code = exit_code;
            // A clean exit is a stop; anything else is a failure worth showing.
            instance.status = match exit_code {
                Some(0) | None => ServiceStatus::Stopped,
                Some(_) => ServiceStatus::Failed,
            };
            if let Err(err) = store.update_instance(&instance) {
                tracing::error!(%err, "failed to record service exit");
            }
            if let Some(port) = instance.port {
                let _ = store.release_lease(port);
            }
            let _ = logs.append(
                &service.id,
                LogStream::System,
                match exit_code {
                    Some(code) => format!("process exited with code {code}"),
                    None => "process terminated by signal".to_string(),
                },
            );
            drain_and_stop(&supervisor, &service.id).await;

            events.publish(RuntimeEvent::ServiceExited {
                service_id: service.id.clone(),
                exit_code,
            });
            events.publish(RuntimeEvent::ServiceStatusChanged {
                service_id: service.id,
                status: instance.status,
                port: instance.port,
            });
        })
    }

    /// Poll a freshly started service until it answers, so `starting` becomes a
    /// real state with an end rather than a label that never changes.
    fn watch_health(
        &self,
        service: Service,
        mut instance: RuntimeInstance,
    ) -> tokio::task::JoinHandle<()> {
        let store = self.store_arc();
        let adapter: Arc<dyn PlatformAdapter> = self.adapter_arc();
        let events = self.events().clone();
        let check = service
            .health_check
            .clone()
            .unwrap_or_else(|| default_health_check(&service, instance.port));
        let identity = ProcessIdentity::new(instance.pid, instance.process_start_time);

        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + STARTUP_GRACE;
            loop {
                let alive = adapter.process().is_alive(&identity).unwrap_or(false);
                if !alive {
                    // The exit watcher owns the transition in this case.
                    return;
                }

                let probe = crate::health::probe(&check, instance.port, true).await;
                if probe.status == ServiceStatus::Healthy {
                    transition(&store, &events, &mut instance, ServiceStatus::Healthy);
                    return;
                }
                if tokio::time::Instant::now() >= deadline {
                    transition(&store, &events, &mut instance, ServiceStatus::Unhealthy);
                    return;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
    }

    /// Stop a service and everything it spawned.
    pub async fn stop_service(&self, service_id: &ServiceId, timeout: Duration) -> Result<ServiceView> {
        let service = self.require_service(service_id)?;
        let (status, instance) = self.current_state(&service)?;

        // A finished instance is no more stoppable than no instance at all, and
        // both must reach the same check below: a service the runtime ran once
        // and stopped leaves a record behind, and treating that record as the
        // whole answer reports "not running" for something the port table — and
        // the view the caller is looking at — shows up and serving.
        let instance = instance.filter(|_| status.is_live());

        let Some(mut instance) = instance else {
            // It may well be running — just not by us. Saying "not running"
            // would contradict the view the caller is looking at.
            let view = self.service_view(&service)?;
            if view.status.is_live() && !view.managed {
                // "stop it where it was started" is only actionable if the
                // caller can find the process, so name it.
                let pid = view
                    .actual_port
                    .and_then(|port| {
                        let owners = self.port_owners().ok()?;
                        owners.into_iter().find(|owner| owner.port == port)
                    })
                    .map(|owner| owner.pid)
                    .unwrap_or(0);
                return Err(RuntimeError::NotPermitted {
                    pid,
                    reason: format!(
                        "'{}' is running but was not started by the runtime; stop it where it was started",
                        service.name
                    ),
                });
            }
            return Err(RuntimeError::NotRunning {
                service: service.name.clone(),
            });
        };

        instance.status = ServiceStatus::Stopping;
        self.store().update_instance(&instance)?;
        self.events().publish(RuntimeEvent::ServiceStatusChanged {
            service_id: service.id.clone(),
            status: ServiceStatus::Stopping,
            port: instance.port,
        });

        let identity = ProcessIdentity::new(instance.pid, instance.process_start_time);
        let process = self.adapter().process();

        process.terminate_tree(&identity, TerminationMode::Graceful)?;

        // Escalate rather than hang: a dev server that ignores SIGTERM must
        // still release its port within a bounded time.
        let deadline = tokio::time::Instant::now() + timeout;
        let mut forced = false;
        loop {
            if !process.is_alive(&identity)? {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                if forced {
                    return Err(RuntimeError::internal(format!(
                        "pid {} survived a forced termination",
                        identity.pid
                    )));
                }
                self.logs_arc().append(
                    &service.id,
                    LogStream::System,
                    "graceful stop timed out; forcing termination",
                )?;
                process.terminate_tree(&identity, TerminationMode::Forceful)?;
                forced = true;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        instance.status = ServiceStatus::Stopped;
        instance.stopped_at = Some(Utc::now());
        self.store().update_instance(&instance)?;
        if let Some(port) = instance.port {
            self.store().release_lease(port)?;
        }
        drain_and_stop(self.supervisor(), &service.id).await;
        self.invalidate_port_owners();
        self.events().publish(RuntimeEvent::ServiceStatusChanged {
            service_id: service.id.clone(),
            status: ServiceStatus::Stopped,
            port: instance.port,
        });

        self.service_view(&service)
    }

    pub async fn restart_service(
        &self,
        service_id: &ServiceId,
        options: StartOptions,
    ) -> Result<StartOutcome> {
        let service = self.require_service(service_id)?;

        // Restarting means stopping, and the runtime does not stop what it did
        // not start. Falling through would skip the stop and start a second
        // copy beside the one already serving.
        let view = self.service_view(&service)?;
        if view.status.is_live() && !view.managed {
            let via = view
                .supervisor_entry
                .as_deref()
                .zip(view.supervisor.as_deref())
                .map(|(entry, supervisor)| {
                    format!("; ask {supervisor} to restart '{entry}' instead")
                })
                .unwrap_or_default();
            let pid = view
                .actual_port
                .and_then(|port| {
                    let owners = self.port_owners().ok()?;
                    owners.into_iter().find(|owner| owner.port == port)
                })
                .map(|owner| owner.pid)
                .unwrap_or(0);
            return Err(RuntimeError::NotPermitted {
                pid,
                reason: format!(
                    "'{}' is running but was not started by the runtime{via}",
                    service.name
                ),
            });
        }

        let (status, _) = self.current_state(&service)?;
        if status.is_live() {
            self.stop_service(service_id, GRACEFUL_TIMEOUT).await?;
        }
        self.start_service(service_id, options).await
    }

    pub async fn health(&self, service_id: &ServiceId) -> Result<HealthReport> {
        let service = self.require_service(service_id)?;
        let (status, instance) = self.current_state(&service)?;

        let Some(instance) = instance.filter(|_| status.is_live()) else {
            // Not ours does not mean not running. A service found already
            // listening is reported as up everywhere else, and answering "not
            // running" here contradicts the view the caller is looking at —
            // while also skipping the check on most of a machine, since
            // adopted is the common case rather than the exception.
            let view = self.service_view(&service)?;
            if let (true, Some(port)) = (view.status.is_live(), view.actual_port) {
                let check = service
                    .health_check
                    .clone()
                    .unwrap_or_else(|| default_health_check(&service, Some(port)));
                // Something holds the port, so the process half of the question
                // is already answered; what is left is whether it responds.
                let probe = crate::health::probe(&check, Some(port), true).await;
                return Ok(HealthReport {
                    service_id: service.id,
                    status: probe.status,
                    detail: probe.detail,
                    checked_port: probe.checked_port,
                });
            }

            return Ok(HealthReport {
                service_id: service.id,
                status: ServiceStatus::Stopped,
                detail: Some("service is not running".to_string()),
                checked_port: None,
            });
        };

        let identity = ProcessIdentity::new(instance.pid, instance.process_start_time);
        let alive = self.adapter().process().is_alive(&identity)?;
        let check = service
            .health_check
            .clone()
            .unwrap_or_else(|| default_health_check(&service, instance.port));

        let mut probe = crate::health::probe(&check, instance.port, alive).await;

        // A live process that never binds the port it was given is the shape of
        // a service that ignores $PORT — it is listening somewhere else, or
        // failed to bind at all. Saying only "connection refused" leaves the
        // caller to work that out.
        if alive && probe.status == ServiceStatus::Unhealthy {
            let running_for = Utc::now() - instance.started_at;
            if running_for.num_seconds() >= 3 {
                if let Some(port) = instance.port {
                    probe.detail = Some(format!(
                        "nothing is listening on {port} although the process is running; \
                         it may not be honouring $PORT — check its logs, or set the port it \
                         does use with `service set --port`"
                    ));
                }
            }
        }

        // Persist what we just observed, so a caller that probes and then lists
        // does not see two different answers for the same moment.
        if probe.status != instance.status
            && matches!(
                probe.status,
                ServiceStatus::Healthy | ServiceStatus::Unhealthy
            )
        {
            let mut updated = instance.clone();
            updated.status = probe.status;
            self.store().update_instance(&updated)?;
            self.events().publish(RuntimeEvent::ServiceStatusChanged {
                service_id: service.id.clone(),
                status: probe.status,
                port: updated.port,
            });
        }

        Ok(HealthReport {
            service_id: service.id,
            status: probe.status,
            detail: probe.detail,
            checked_port: probe.checked_port,
        })
    }

    /// Block until a service reports healthy, or the timeout expires.
    ///
    /// This is what makes "restart the API and wait until it is healthy" a
    /// single agent step instead of a polling loop in the agent's context.
    pub async fn wait_until_healthy(
        &self,
        service_id: &ServiceId,
        timeout: Duration,
    ) -> Result<HealthReport> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut last = self.health(service_id).await?;
        loop {
            if last.status == ServiceStatus::Healthy {
                return Ok(last);
            }
            // A stopped service will not become healthy by waiting.
            if matches!(last.status, ServiceStatus::Failed | ServiceStatus::Stopped) {
                return Ok(last);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(last);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
            last = self.health(service_id).await?;
        }
    }

    /// Bring the database back in line with the OS.
    ///
    /// Run on daemon start: instances recorded as live whose process is gone
    /// are closed out, and their port leases released. Without this the state
    /// drifts a little further from reality after every crash.
    pub fn reconcile(&self) -> Result<usize> {
        let mut corrected = 0;
        for mut instance in self.store().live_instances()? {
            let identity = ProcessIdentity::new(instance.pid, instance.process_start_time);
            if self.adapter().process().is_alive(&identity)? {
                if let Some(port) = instance.port {
                    self.resolver().activate(port)?;
                }
                continue;
            }
            instance.status = ServiceStatus::Stopped;
            instance.stopped_at = Some(Utc::now());
            self.store().update_instance(&instance)?;
            if let Some(port) = instance.port {
                self.store().release_lease(port)?;
            }
            corrected += 1;
        }
        self.store().expire_leases(Utc::now())?;
        Ok(corrected)
    }

    /// Stop every service this daemon started. Used on shutdown.
    pub async fn stop_all(&self) -> Result<usize> {
        let mut stopped = 0;
        for service_id in self.supervisor().service_ids()? {
            match self.stop_service(&service_id, GRACEFUL_TIMEOUT).await {
                Ok(_) => stopped += 1,
                Err(err) => tracing::warn!(service = %service_id, %err, "failed to stop service"),
            }
        }
        Ok(stopped)
    }
}

/// Open a capture file for appending, creating it if needed.
fn open_capture(path: &std::path::Path) -> Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| RuntimeError::io(format!("cannot open {}: {err}", path.display())))
}

/// Without an explicit check, a service with a port is judged by whether that
/// port accepts connections, and one without by whether the process is alive.
/// What to check when a service does not say.
///
/// A TCP connect only proves something is holding the port, which is exactly
/// what a wedged dev server does: this machine had one accepting connections
/// and answering none of them, reported healthy, for an unknown length of
/// time. So anything declared as serving HTTP is asked to answer, with any
/// response counting — the question is whether it is alive, not whether it
/// agrees with us about the path.
///
/// Only for the types that say they speak HTTP. A database or a worker that
/// happens to hold a port would fail an HTTP check while being perfectly
/// healthy, and a check that is wrong in that direction is worse than a weak
/// one: it teaches the reader to ignore it.
fn default_health_check(service: &Service, port: Option<u16>) -> HealthCheck {
    match (port, service.service_type) {
        (Some(_), ServiceType::Web | ServiceType::Api) => HealthCheck::Http {
            path: "/".to_string(),
            port: None,
            expect_status: Vec::new(),
        },
        (Some(_), _) => HealthCheck::Tcp { port: None },
        (None, _) => HealthCheck::Process,
    }
}

fn transition(
    store: &Arc<Store>,
    events: &EventBus,
    instance: &mut RuntimeInstance,
    status: ServiceStatus,
) {
    instance.status = status;
    if let Err(err) = store.update_instance(instance) {
        tracing::error!(%err, "failed to record status change");
        return;
    }
    events.publish(RuntimeEvent::ServiceStatusChanged {
        service_id: instance.service_id.clone(),
        status,
        port: instance.port,
    });
}

/// Wind down the readers following a run's output.
///
/// Not stopped at once: the last thing a service prints is usually the reason
/// it went, and the readers poll — cutting them off immediately throws that
/// away. Not left running either: `finish` takes them out of the supervisor, so
/// anything still tailing afterwards is unreachable, and it is still reading the
/// capture file the *next* run appends to. Two runs, two readers, every line
/// logged twice.
///
/// One function because a run has two endings — it exits on its own, or it is
/// stopped — and the first version of this fixed only the first. The duplicate
/// lines came straight back through the other door.
async fn drain_and_stop(supervisor: &crate::supervisor::Supervisor, service_id: &ServiceId) {
    let Ok(Some(process)) = supervisor.finish(service_id) else {
        return;
    };
    // Parked rather than held here, so that a start arriving inside the drain
    // window can find these readers and end them before writing.
    if supervisor.park(service_id.clone(), process).is_err() {
        return;
    }
    tokio::time::sleep(CAPTURE_DRAIN).await;
    let _ = supervisor.stop_parked(service_id);
}

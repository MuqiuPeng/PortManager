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
    ConflictPolicy, HealthCheck, HealthReport, InstanceId, LogStream, PortReservation, Result,
    RuntimeError, RuntimeInstance, Service, ServiceId, ServiceStatus, ServiceView, SessionId,
    StartOutcome, StartedBy,
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
    pub async fn start_service(
        &self,
        service_id: &ServiceId,
        options: StartOptions,
    ) -> Result<StartOutcome> {
        let service = self.require_service(service_id)?;
        let workspace = self.require_workspace(&service.workspace_id)?;
        let project = self.require_project(&workspace.project_id)?;

        // Already running is not an error: an agent asking twice should get the
        // running service back, not a second copy of it.
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
        })
    }

    async fn spawn_service(
        &self,
        service: &Service,
        port: Option<u16>,
        options: &StartOptions,
    ) -> Result<RuntimeInstance> {
        let mut command = self.adapter().spawn().build(&service.command)?;
        command.current_dir(&service.cwd);
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
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

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

        let mut tasks = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            tasks.push(self.pump_logs(service.id.clone(), stdout, LogStream::Stdout));
        }
        if let Some(stderr) = child.stderr.take() {
            tasks.push(self.pump_logs(service.id.clone(), stderr, LogStream::Stderr));
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
            // `finish`, not `remove`: aborting the log pumps here would throw
            // away the output that explains the exit.
            let _ = supervisor.finish(&service.id);

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
            .unwrap_or(default_health_check(instance.port));
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

        let Some(mut instance) = instance else {
            // It may well be running — just not by us. Saying "not running"
            // would contradict the view the caller is looking at.
            let view = self.service_view(&service)?;
            if view.status.is_live() && !view.managed {
                return Err(RuntimeError::NotPermitted {
                    pid: 0,
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
        if !status.is_live() {
            return Err(RuntimeError::NotRunning {
                service: service.name.clone(),
            });
        }

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
        // The pipes close when the process does, so the pumps drain the last
        // lines and end by themselves.
        self.supervisor().finish(&service.id)?;
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
            .unwrap_or(default_health_check(instance.port));

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

/// Without an explicit check, a service with a port is judged by whether that
/// port accepts connections, and one without by whether the process is alive.
fn default_health_check(port: Option<u16>) -> HealthCheck {
    match port {
        Some(_) => HealthCheck::Tcp { port: None },
        None => HealthCheck::Process,
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

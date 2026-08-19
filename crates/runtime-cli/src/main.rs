//! `runtime` — the command line interface.
//!
//! A thin client over the daemon. It starts the daemon on demand, so the first
//! command a user runs works without a separate install step.

mod render;

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use runtime_core::paths;
use runtime_ipc::protocol::{Request, ResponseBody};
use runtime_ipc::Client;
use runtime_types::{Result, RuntimeError};

#[derive(Debug, Parser)]
#[command(
    name = "runtime",
    version,
    about = "See and control everything running on localhost"
)]
struct Cli {
    /// Emit JSON instead of a table.
    #[arg(long, global = true)]
    json: bool,

    /// Project selector (id, name, or a path inside it).
    #[arg(long, short, global = true)]
    project: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show every project, workspace and service.
    List,

    /// Manage projects.
    #[command(subcommand)]
    Project(ProjectCommand),

    /// Inspect services.
    #[command(subcommand)]
    Service(ServiceCommand),

    /// Start a service.
    Start {
        service: String,
        /// Use this port instead of the configured one.
        #[arg(long)]
        port: Option<u16>,
        /// reuse | allocate-next | fail | ask | kill-existing
        #[arg(long)]
        on_conflict: Option<String>,
        /// Record who started it: manual, cli, claude-code, codex, cursor.
        #[arg(long)]
        started_by: Option<String>,
        /// Block until the service reports healthy.
        #[arg(long)]
        wait: bool,
    },

    /// Stop a service and everything it spawned.
    Stop {
        service: String,
        /// Seconds to wait before forcing termination.
        #[arg(long)]
        timeout: Option<u64>,
    },

    /// Restart a service.
    Restart {
        service: String,
        #[arg(long)]
        wait: bool,
    },

    /// Read captured output.
    Logs {
        service: String,
        /// Number of lines to show.
        #[arg(long, short = 'n', default_value_t = 100)]
        lines: usize,
        /// Follow new output until interrupted.
        #[arg(long, short)]
        follow: bool,
    },

    /// Report a service's health.
    Health {
        service: String,
        /// Wait until healthy, up to this many seconds.
        #[arg(long)]
        wait: Option<u64>,
    },

    /// Inspect ports.
    #[command(subcommand)]
    Port(PortCommand),

    /// Manage git worktrees of a project.
    #[command(subcommand)]
    Worktree(WorktreeCommand),

    /// Manage the daemon.
    #[command(subcommand)]
    Daemon(DaemonCommand),

    /// Check that the runtime can see processes and ports on this machine.
    Doctor,
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// List registered projects.
    List,
    /// Register a directory, inferring its services.
    Add {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Show one project.
    Show { selector: Option<String> },
    /// Unregister a project. Running services are left alone.
    Remove { selector: String },
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// List services, optionally for one project.
    List,
    /// Show one service in detail.
    Show { service: String },
}

#[derive(Debug, Subcommand)]
enum PortCommand {
    /// Show everything listening, resolved to projects.
    List,
    /// Ask who owns a port and what to use instead.
    Check { port: u16 },
    /// Claim a port for a service before starting it.
    Reserve {
        service: String,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        on_conflict: Option<String>,
    },
    /// Drop a lease.
    Release { port: u16 },
}

#[derive(Debug, Subcommand)]
enum WorktreeCommand {
    /// List a project's checkouts and their port offsets.
    List { selector: Option<String> },
    /// Register a checkout that git does not report yet.
    Add { path: PathBuf },
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Start the daemon if it is not already running.
    Start,
    /// Ask the daemon to exit.
    Stop,
    /// Report daemon status.
    Status,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(output) => {
            if !output.is_empty() {
                println!("{output}");
            }
            std::process::ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<String> {
    // Daemon control is the one thing that must work without a daemon.
    if let Command::Daemon(command) = &cli.command {
        return daemon_command(command, cli.json).await;
    }
    if let Command::Doctor = &cli.command {
        return doctor().await;
    }

    let mut client = connect().await?;
    let project = cli.project.clone();

    let response = match cli.command {
        Command::List => client.call(Request::ListProjects).await?,

        Command::Project(ProjectCommand::List) => client.call(Request::ListProjects).await?,
        Command::Project(ProjectCommand::Add { path, name }) => {
            let path = std::fs::canonicalize(&path)
                .map_err(|err| RuntimeError::Io(format!("{}: {err}", path.display())))?;
            client.call(Request::AddProject { path, name }).await?
        }
        Command::Project(ProjectCommand::Show { selector }) => {
            let selector = selector
                .or(project.clone())
                .unwrap_or_else(|| ".".to_string());
            client.call(Request::GetProject { selector }).await?
        }
        Command::Project(ProjectCommand::Remove { selector }) => {
            client.call(Request::RemoveProject { selector }).await?
        }

        Command::Service(ServiceCommand::List) => {
            client.call(Request::ListServices { project }).await?
        }
        Command::Service(ServiceCommand::Show { service }) => {
            client.call(Request::GetService { project, service }).await?
        }

        Command::Start {
            service,
            port,
            on_conflict,
            started_by,
            wait,
        } => {
            let started = client
                .call(Request::StartService {
                    project: project.clone(),
                    service: service.clone(),
                    port,
                    on_conflict,
                    started_by: Some(started_by.unwrap_or_else(|| "cli".to_string())),
                    session: None,
                })
                .await?;
            if wait {
                let health = client
                    .call(Request::WaitUntilHealthy {
                        project,
                        service,
                        timeout_seconds: None,
                    })
                    .await?;
                return Ok(format!(
                    "{}\n{}",
                    render_response(&started, cli.json)?,
                    render_response(&health, cli.json)?
                ));
            }
            started
        }

        Command::Stop { service, timeout } => {
            client
                .call(Request::StopService {
                    project,
                    service,
                    timeout_seconds: timeout,
                })
                .await?
        }

        Command::Restart { service, wait } => {
            let started = client
                .call(Request::RestartService {
                    project: project.clone(),
                    service: service.clone(),
                    started_by: Some("cli".to_string()),
                    session: None,
                })
                .await?;
            if wait {
                let health = client
                    .call(Request::WaitUntilHealthy {
                        project,
                        service,
                        timeout_seconds: None,
                    })
                    .await?;
                return Ok(format!(
                    "{}\n{}",
                    render_response(&started, cli.json)?,
                    render_response(&health, cli.json)?
                ));
            }
            started
        }

        Command::Logs {
            service,
            lines,
            follow,
        } => {
            if follow {
                return follow_logs(client, project, service, lines, cli.json).await;
            }
            client
                .call(Request::GetLogs {
                    project,
                    service,
                    max_lines: Some(lines),
                    since_seq: None,
                })
                .await?
        }

        Command::Health { service, wait } => match wait {
            Some(seconds) => {
                client
                    .call(Request::WaitUntilHealthy {
                        project,
                        service,
                        timeout_seconds: Some(seconds),
                    })
                    .await?
            }
            None => client.call(Request::GetHealth { project, service }).await?,
        },

        Command::Port(PortCommand::List) => client.call(Request::ListPorts).await?,
        Command::Port(PortCommand::Check { port }) => {
            client.call(Request::CheckPort { port }).await?
        }
        Command::Port(PortCommand::Reserve {
            service,
            port,
            on_conflict,
        }) => {
            client
                .call(Request::ReservePort {
                    project,
                    service,
                    port,
                    on_conflict,
                    started_by: Some("cli".to_string()),
                })
                .await?
        }
        Command::Port(PortCommand::Release { port }) => {
            client.call(Request::ReleasePort { port }).await?
        }

        Command::Worktree(WorktreeCommand::List { selector }) => {
            let selector = selector
                .or(project)
                .unwrap_or_else(|| ".".to_string());
            client.call(Request::ListWorktrees { selector }).await?
        }
        Command::Worktree(WorktreeCommand::Add { path }) => {
            let selector = project.unwrap_or_else(|| ".".to_string());
            let path = std::fs::canonicalize(&path)
                .map_err(|err| RuntimeError::Io(format!("{}: {err}", path.display())))?;
            client
                .call(Request::RegisterWorktree { selector, path })
                .await?
        }

        Command::Daemon(_) | Command::Doctor => unreachable!("handled above"),
    };

    render_response(&response, cli.json)
}

fn render_response(response: &ResponseBody, json: bool) -> Result<String> {
    if json {
        return serde_json::to_string_pretty(response)
            .map_err(|err| RuntimeError::internal(format!("failed to encode output: {err}")));
    }
    Ok(match response {
        ResponseBody::Pong { protocol_version } => format!("pong (protocol {protocol_version})"),
        ResponseBody::Info(info) => render::daemon_info(info),
        ResponseBody::Projects { items } => render::projects(items),
        ResponseBody::Project(view) => render::projects(std::slice::from_ref(view)),
        ResponseBody::Workspaces { items } => render::workspaces(items),
        ResponseBody::Workspace(item) => render::workspaces(std::slice::from_ref(item)),
        ResponseBody::Services { items } => render::services(items),
        ResponseBody::Service(view) => render::service_detail(view),
        ResponseBody::Started(outcome) => render::start_outcome(outcome),
        ResponseBody::Health(report) => render::health(report),
        ResponseBody::Port(status) => render::port_status(status),
        ResponseBody::Ports { items } => render::ports(items),
        ResponseBody::Reservation(reservation) => {
            let mut out = format!("reserved port {}", reservation.port);
            if reservation.reallocated {
                if let Some(preferred) = reservation.preferred_port {
                    out.push_str(&format!(" (preferred {preferred} was taken)"));
                }
            }
            out
        }
        ResponseBody::Logs { items } => render::logs(items),
        ResponseBody::Sessions { items } => items
            .iter()
            .map(|session| format!("{} {} ({})", session.id, session.client, session.provider))
            .collect::<Vec<_>>()
            .join("\n"),
        ResponseBody::Session(session) => session.id.to_string(),
        ResponseBody::Done { ok } => {
            if *ok {
                "done".to_string()
            } else {
                "no change".to_string()
            }
        }
    })
}

/// Stream new output until the user interrupts.
async fn follow_logs(
    mut client: Client,
    project: Option<String>,
    service: String,
    lines: usize,
    json: bool,
) -> Result<String> {
    let initial = client
        .call(Request::GetLogs {
            project: project.clone(),
            service: service.clone(),
            max_lines: Some(lines),
            since_seq: None,
        })
        .await?;

    let mut cursor = None;
    if let ResponseBody::Logs { items } = &initial {
        println!("{}", render::logs(items));
        cursor = items.last().map(|line| line.seq);
    }

    // Polling rather than subscribing keeps `--follow` working against an older
    // daemon that predates the event stream.
    loop {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let response = client
            .call(Request::GetLogs {
                project: project.clone(),
                service: service.clone(),
                max_lines: Some(200),
                since_seq: cursor,
            })
            .await?;
        if let ResponseBody::Logs { items: new_lines } = &response {
            if new_lines.is_empty() {
                continue;
            }
            cursor = new_lines.last().map(|line| line.seq);
            if json {
                println!("{}", serde_json::to_string(new_lines).unwrap_or_default());
            } else {
                println!("{}", render::logs(new_lines));
            }
        }
    }
}

// ---- daemon control ----------------------------------------------------

async fn connect() -> Result<Client> {
    if let Ok(client) = Client::connect_default().await {
        return Ok(client);
    }
    spawn_daemon()?;
    wait_for_daemon(Duration::from_secs(10)).await
}

async fn daemon_command(command: &DaemonCommand, json: bool) -> Result<String> {
    match command {
        DaemonCommand::Start => {
            if runtime_ipc::client::is_running().await {
                return Ok("daemon is already running".to_string());
            }
            spawn_daemon()?;
            let mut client = wait_for_daemon(Duration::from_secs(10)).await?;
            let info = client.call(Request::DaemonInfo).await?;
            render_response(&info, json)
        }
        DaemonCommand::Stop => {
            let mut client = Client::connect_default().await?;
            let response = client.call(Request::Shutdown).await?;
            render_response(&response, json)
        }
        DaemonCommand::Status => match Client::connect_default().await {
            Ok(mut client) => {
                let info = client.call(Request::DaemonInfo).await?;
                render_response(&info, json)
            }
            Err(_) => Ok("not running".to_string()),
        },
    }
}

/// Launch the daemon detached from this terminal.
fn spawn_daemon() -> Result<()> {
    let binary = daemon_binary()?;
    std::process::Command::new(&binary)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| {
            RuntimeError::Io(format!("failed to start {}: {err}", binary.display()))
        })?;
    Ok(())
}

/// Look for the daemon next to this binary first, so a checkout's `target/debug`
/// build never picks up a different installed copy.
fn daemon_binary() -> Result<PathBuf> {
    let name = if cfg!(windows) {
        "runtime-daemon.exe"
    } else {
        "runtime-daemon"
    };
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Ok(PathBuf::from(name))
}

async fn wait_for_daemon(timeout: Duration) -> Result<Client> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(client) = Client::connect_default().await {
            return Ok(client);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(RuntimeError::Io(format!(
                "the daemon did not come up within {}s; try running `runtime-daemon` directly to see why",
                timeout.as_secs()
            )));
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}

/// Verify the platform adapter can actually see this machine.
///
/// This is the check that matters most when porting: if process and port
/// discovery are wrong, nothing built on top of them can be right.
async fn doctor() -> Result<String> {
    let runtime = runtime_core::Runtime::in_memory()?;
    let adapter = runtime.adapter();

    let processes = adapter.process().list_processes()?;
    let with_cwd = processes.iter().filter(|p| p.cwd.is_some()).count();
    let ports = adapter.port().listening_ports()?;
    let self_pid = std::process::id();
    let self_info = adapter.process().process_info(self_pid)?;

    let mut out = String::new();
    out.push_str(&format!("platform          {}\n", adapter.name()));
    out.push_str(&format!("data directory    {}\n", paths::data_dir()?.display()));
    out.push_str(&format!("ipc endpoint      {}\n", paths::socket_path()?.display()));
    out.push_str(&format!("processes visible {}\n", processes.len()));
    out.push_str(&format!("  with a cwd      {with_cwd}\n"));
    out.push_str(&format!("listening ports   {}\n", ports.len()));
    out.push_str(&format!(
        "self lookup       {}\n",
        match &self_info {
            Some(info) => format!(
                "ok (pid {}, cwd {})",
                info.pid,
                info.cwd
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            ),
            None => "FAILED — the adapter cannot see this process".to_string(),
        }
    ));
    out.push_str(&format!(
        "daemon            {}\n",
        if runtime_ipc::client::is_running().await {
            "running"
        } else {
            "not running"
        }
    ));

    if self_info.is_none() || processes.is_empty() {
        out.push_str("\nProcess discovery is not working on this platform.\n");
    }
    Ok(out.trim_end().to_string())
}

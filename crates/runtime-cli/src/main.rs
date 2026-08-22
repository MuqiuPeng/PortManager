//! `runtime` — the command line interface.
//!
//! A thin client over the daemon. It starts the daemon on demand, so the first
//! command a user runs works without a separate install step.

mod hook;
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
    ///
    /// Narrowed by `--project` to one of them.
    List,

    /// Find projects on this machine automatically.
    ///
    /// Always reports what is listening right now, resolved back to its
    /// repository. Pass --path to also walk a directory tree for projects that
    /// are not running.
    Scan {
        /// Directory tree to search, repeatable.
        #[arg(long)]
        path: Vec<PathBuf>,
        /// Register everything found instead of only reporting it.
        #[arg(long)]
        add: bool,
    },

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

    /// Stop a service something else started, and start it here instead.
    ///
    /// The runtime never terminates what it did not start. This is the one way
    /// across, and it asks for the service by name so that nothing automatic
    /// can take it.
    TakeOver {
        service: String,
        /// Seconds to wait before forcing termination.
        #[arg(long)]
        timeout: Option<u64>,
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

    /// Switch entries another supervisor keeps, without taking them over.
    ///
    /// PM2 still decides what these are and whether they start at boot; this
    /// decides whether they are running now. Deleting an entry is not offered.
    Supervised {
        /// start | stop | restart
        action: String,
        name: String,
    },

    /// Switch containers on and off.
    ///
    /// Compose still owns what these services are; this owns whether they run.
    #[command(subcommand)]
    Container(ContainerCommand),

    /// Write the project's services out as a committable .runtime.json.
    Export {
        selector: Option<String>,
        /// Write it to the project root instead of printing it.
        #[arg(long)]
        write: bool,
    },

    /// Stacks: a named set of services this project is brought up as.
    #[command(subcommand)]
    Stack(StackCommand),

    /// Manage git worktrees of a project.
    #[command(subcommand)]
    Worktree(WorktreeCommand),

    /// Manage the daemon.
    #[command(subcommand)]
    Daemon(DaemonCommand),

    /// Record what agents and shells start, without getting in their way.
    #[command(subcommand)]
    Hook(HookCommand),

    /// Declare whatever is on a port, so it can be started again later.
    ///
    /// The command is taken from the running process, never from the project's
    /// scripts: `dev` and `start` can write to the same build directory, and
    /// adopting under the wrong one leaves the service unable to boot.
    Adopt {
        port: u16,
        /// Declare it even though another supervisor is keeping it alive.
        #[arg(long)]
        force: bool,
        /// Which stack to put it in. Its own, named after it, when unsaid.
        #[arg(long)]
        stack: Option<String>,
    },

    /// Show what is broken right now, and what each one said.
    ///
    /// The starting point when something is wrong and you do not yet know
    /// which service it was.
    Errors {
        /// Lines of output to show per service.
        #[arg(long, short = 'n', default_value_t = 8)]
        lines: usize,
    },

    /// Check that the runtime can see processes and ports on this machine.
    Doctor,
}

#[derive(Debug, Subcommand)]
enum StackCommand {
    /// List the stacks declared in a project.
    List,
    /// Declare or replace one. Steps run in the order given.
    Set {
        name: String,
        /// Service names, in order. Each brings up its own dependencies first.
        #[arg(required = true)]
        members: Vec<String>,
    },
    /// Remove a stack. Nothing running is touched.
    Remove { name: String },
    /// Bring up every step in order.
    Run { name: String },
    /// Stop everything it started, in the reverse order.
    Stop { name: String },
}

#[derive(Debug, Subcommand)]
enum HookCommand {
    /// Add the Claude Code hook for the current user.
    ///
    /// Applies to every project. The command Claude runs is not changed: the
    /// hook only tells the runtime what is about to run, so a service started
    /// outside it can still be restarted from the command that actually ran.
    Install,
    /// Take the hook back out.
    Uninstall,
    /// Report whether the hook is installed.
    Status,
    /// Read one hook payload on stdin. Called by Claude Code, not by hand.
    #[command(hide = true)]
    PreToolUse,
    /// Show what has been recorded recently.
    Log,
    /// Register the MCP server with Claude Code for every project.
    Mcp {
        /// The MCP server command. Defaults to `runtime-mcp` on PATH.
        #[arg(long, default_value = "runtime-mcp")]
        command: String,
    },
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

    /// Correct a service. Detection is a guess; this is how you fix it.
    Set {
        service: String,
        /// Port the service should use.
        #[arg(long)]
        port: Option<u16>,
        /// Forget the port entirely.
        #[arg(long, conflicts_with = "port")]
        no_port: bool,
        #[arg(long)]
        command: Option<String>,
        /// Working directory, relative to the workspace unless absolute.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// web | api | worker | database | cache | container | custom
        #[arg(long = "type")]
        service_type: Option<String>,
        #[arg(long)]
        rename: Option<String>,
        /// reuse | allocate-next | fail | ask | kill-existing
        #[arg(long)]
        on_conflict: Option<String>,
        /// KEY=VALUE, repeatable. Merged with the existing environment.
        #[arg(long = "env")]
        env: Vec<String>,
        /// Variable to drop, repeatable.
        #[arg(long = "unset-env")]
        unset_env: Vec<String>,
        /// Services here that must be up first. Repeat to list several;
        /// pass --no-depends-on to clear them.
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
        /// Drop every dependency.
        #[arg(long, conflicts_with = "depends_on")]
        no_depends_on: bool,
        /// Runs to completion instead of staying up.
        #[arg(long = "one-shot")]
        one_shot: Option<bool>,
    },

    /// Declare a service detection did not find.
    Add {
        name: String,
        #[arg(long)]
        command: String,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long = "type")]
        service_type: Option<String>,
        /// Services here that must be up first. Repeatable.
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
        /// Runs to completion instead of staying up: a migration, a seed.
        #[arg(long = "one-shot")]
        one_shot: bool,
    },

    /// Remove a declared service. Nothing running is touched.
    Remove { service: String },
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
enum ContainerCommand {
    /// Start a stopped container.
    Start { name: String },
    /// Stop a running container.
    Stop { name: String },
    Restart { name: String },
    /// Read a container's own output.
    Logs {
        name: String,
        #[arg(long, short = 'n', default_value_t = 100)]
        lines: usize,
    },
}

#[derive(Debug, Subcommand)]
enum WorktreeCommand {
    /// List a project's checkouts and their port offsets.
    List { selector: Option<String> },
    /// Register a checkout that git does not report yet.
    Add { path: PathBuf },
    /// Stop tracking a checkout. The directory itself is left alone.
    Remove {
        /// Its path, or its branch.
        checkout: String,
    },
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
    if let Command::Hook(command) = &cli.command {
        match command {
            // Never fails, never prints: Claude Code reads silence as
            // "run the command unchanged", which is the only safe outcome.
            HookCommand::PreToolUse => {
                hook::pre_tool_use().await;
                return Ok(String::new());
            }
            HookCommand::Install => {
                return hook::install().map_err(RuntimeError::invalid);
            }
            HookCommand::Uninstall => {
                return hook::uninstall().map_err(RuntimeError::invalid);
            }
            HookCommand::Status => return Ok(hook::status()),
            HookCommand::Mcp { command } => {
                return hook::install_mcp(command).map_err(RuntimeError::invalid);
            }
            HookCommand::Log => {
                let mut client = connect().await?;
                let response = client.call(Request::ListLaunches).await?;
                return render_response(&response, cli.json);
            }
        }
    }

    let mut client = connect().await?;
    let project = cli.project.clone();

    let response = match cli.command {
        // `--project` narrows this like it narrows everything else. Accepting
        // the flag and ignoring it is worse than rejecting it: the output looks
        // like an answer to the question that was asked.
        Command::Errors { lines } => {
            client.call(Request::ListFailures { detail_lines: lines }).await?
        }

        Command::List => match project.clone() {
            Some(selector) => client.call(Request::GetProject { selector }).await?,
            None => client.call(Request::ListProjects).await?,
        },

        Command::Stack(command) => {
            let selector = project.clone().unwrap_or_else(|| ".".to_string());
            match command {
                StackCommand::List => client.call(Request::ListStacks { selector }).await?,
                StackCommand::Set { name, members } => {
                    client.call(Request::SetStack { selector, name, members }).await?
                }
                StackCommand::Remove { name } => {
                    client.call(Request::RemoveStack { selector, name }).await?
                }
                StackCommand::Run { name } => {
                    client.call(Request::RunStack { selector, name }).await?
                }
                StackCommand::Stop { name } => {
                    client.call(Request::StopStack { selector, name }).await?
                }
            }
        }

        Command::Supervised { action, name } => {
            client.call(Request::ControlSupervised { name, action }).await?
        }

        Command::Adopt { port, force, stack } => {
            client.call(Request::AdoptPort { port, force, stack }).await?
        }

        Command::Scan { path, add } => {
            let mut paths = Vec::new();
            for candidate in path {
                paths.push(std::fs::canonicalize(&candidate).map_err(|err| {
                    RuntimeError::io(format!("{}: {err}", candidate.display()))
                })?);
            }
            client
                .call(Request::DiscoverProjects { paths, adopt: add })
                .await?
        }

        Command::Project(ProjectCommand::List) => client.call(Request::ListProjects).await?,
        Command::Project(ProjectCommand::Add { path, name }) => {
            let path = std::fs::canonicalize(&path)
                .map_err(|err| RuntimeError::io(format!("{}: {err}", path.display())))?;
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

        Command::Service(ServiceCommand::Set {
            service,
            port,
            no_port,
            command,
            cwd,
            service_type,
            rename,
            on_conflict,
            env,
            unset_env,
            depends_on,
            no_depends_on,
            one_shot,
        }) => {
            let patch = runtime_types::ServicePatch {
                name: rename,
                command,
                cwd,
                service_type: service_type
                    .as_deref()
                    .map(parse_service_type)
                    .transpose()?,
                // An empty list from the flag means "not mentioned"; clearing
                // is asked for explicitly, so a set of dependencies cannot be
                // lost by editing something else.
                depends_on: if no_depends_on {
                    Some(Vec::new())
                } else if depends_on.is_empty() {
                    None
                } else {
                    Some(depends_on)
                },
                one_shot,
                // `Some(None)` clears it; `None` leaves it alone.
                preferred_port: if no_port {
                    Some(None)
                } else {
                    port.map(Some)
                },
                health_check: None,
                auto_start: None,
                conflict_policy: on_conflict
                    .as_deref()
                    .map(parse_conflict_policy)
                    .transpose()?,
                env: parse_env(&env)?,
                remove_env: unset_env,
            };
            if patch.is_empty() {
                return Err(RuntimeError::invalid(
                    "nothing to change; pass --port, --command, --cwd, --type, --rename, --on-conflict or --env",
                ));
            }
            client
                .call(Request::UpdateService {
                    project,
                    service,
                    patch,
                })
                .await?
        }

        Command::Service(ServiceCommand::Add {
            name,
            command,
            port,
            cwd,
            service_type,
            depends_on,
            one_shot,
        }) => {
            let selector = project.unwrap_or_else(|| ".".to_string());
            client
                .call(Request::AddService {
                    selector,
                    name,
                    config: runtime_types::ServiceConfig {
                        command,
                        port,
                        cwd,
                        service_type: service_type
                            .as_deref()
                            .map(parse_service_type)
                            .transpose()?,
                        env: Default::default(),
                        health: None,
                        auto_start: false,
                        on_conflict: None,
                        depends_on,
                        one_shot,
                    },
                })
                .await?
        }

        Command::Service(ServiceCommand::Remove { service }) => {
            client
                .call(Request::RemoveService { project, service })
                .await?
        }

        Command::Export { selector, write } => {
            let selector = selector
                .or(project)
                .unwrap_or_else(|| ".".to_string());
            let response = client
                .call(Request::ExportConfig {
                    selector: selector.clone(),
                })
                .await?;
            if write {
                return write_config(&mut client, &selector, &response).await;
            }
            response
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

        Command::TakeOver { service, timeout } => {
            client
                .call(Request::TakeOverService {
                    project,
                    service,
                    timeout_seconds: timeout,
                })
                .await?
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

        Command::Container(command) => {
            let (name, action) = match &command {
                ContainerCommand::Start { name } => (name.clone(), "start"),
                ContainerCommand::Stop { name } => (name.clone(), "stop"),
                ContainerCommand::Restart { name } => (name.clone(), "restart"),
                ContainerCommand::Logs { name, lines } => {
                    return render_response(
                        &client
                            .call(Request::GetContainerLogs {
                                name: name.clone(),
                                max_lines: Some(*lines),
                            })
                            .await?,
                        cli.json,
                    );
                }
            };
            client
                .call(Request::ControlContainer {
                    name,
                    action: action.to_string(),
                })
                .await?
        }

        Command::Worktree(WorktreeCommand::Remove { checkout }) => {
            client
                .call(Request::RemoveWorktree {
                    selector: project,
                    checkout,
                })
                .await?
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
                .map_err(|err| RuntimeError::io(format!("{}: {err}", path.display())))?;
            client
                .call(Request::RegisterWorktree { selector, path })
                .await?
        }

        Command::Daemon(_) | Command::Doctor | Command::Hook(_) => {
            unreachable!("handled above")
        }
    };

    render_response(&response, cli.json)
}

fn parse_service_type(value: &str) -> Result<runtime_types::ServiceType> {
    serde_json::from_value(serde_json::Value::String(value.to_ascii_lowercase())).map_err(|_| {
        RuntimeError::invalid(format!(
            "unknown service type '{value}'; expected web, api, worker, database, cache, container or custom"
        ))
    })
}

fn parse_conflict_policy(value: &str) -> Result<runtime_types::ConflictPolicy> {
    runtime_types::ConflictPolicy::parse(value).ok_or_else(|| {
        RuntimeError::invalid(format!(
            "unknown conflict policy '{value}'; expected reuse, allocate-next, fail, ask or kill-existing"
        ))
    })
}

fn parse_env(entries: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
    entries
        .iter()
        .map(|entry| {
            entry
                .split_once('=')
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .ok_or_else(|| RuntimeError::invalid(format!("expected KEY=VALUE, got '{entry}'")))
        })
        .collect()
}

/// Write the exported config into the project root.
async fn write_config(
    client: &mut Client,
    selector: &str,
    response: &ResponseBody,
) -> Result<String> {
    let ResponseBody::Config(config) = response else {
        return Err(RuntimeError::internal("expected a config"));
    };
    // Ask the daemon where the project is rather than guessing from the cwd.
    let ResponseBody::Project(view) = client
        .call(Request::GetProject {
            selector: selector.to_string(),
        })
        .await?
    else {
        return Err(RuntimeError::internal("expected a project"));
    };

    let path = view.project.root_path.join(runtime_types::CONFIG_FILE_NAME);
    let body = serde_json::to_string_pretty(config)
        .map_err(|err| RuntimeError::internal(format!("cannot encode the config: {err}")))?;
    std::fs::write(&path, format!("{body}\n"))
        .map_err(|err| RuntimeError::io(format!("cannot write {}: {err}", path.display())))?;
    Ok(format!("wrote {}", path.display()))
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
        ResponseBody::Discoveries { items } => render::discoveries(items),
        ResponseBody::Project(view) => render::projects(std::slice::from_ref(view)),
        ResponseBody::Workspaces { items } => render::workspaces(items),
        ResponseBody::Workspace(item) => render::workspaces(std::slice::from_ref(item)),
        ResponseBody::Services { items } => render::services(items),
        ResponseBody::Container(view) => render::container_line(view),
        ResponseBody::Config(config) => serde_json::to_string_pretty(config)
            .map_err(|err| RuntimeError::internal(format!("cannot encode the config: {err}")))?,
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
        ResponseBody::Setting { value } => value.clone().unwrap_or_else(|| "(unset)".to_string()),
        ResponseBody::Stacks { items } => render::stacks(items),
        ResponseBody::StackRun { done } => {
            if done.is_empty() {
                "Nothing to do.".to_string()
            } else {
                done
                    .iter()
                    .map(|step| format!("\u{2713} {step}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        ResponseBody::Supervised(view) => {
            let mut out = format!("{} — {} [{}]\n", view.name, view.status, view.supervisor);
            if let Some(pid) = view.pid {
                out.push_str(&format!("  pid {pid}\n"));
            }
            if !view.ports.is_empty() {
                let ports = view
                    .ports
                    .iter()
                    .map(|port| format!(":{port}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                out.push_str(&format!("  {ports}\n"));
            }
            out.push_str(&format!("  {}\n", view.command));
            if let Some(warning) = &view.restart_warning {
                out.push_str(&format!("  ! {warning}\n"));
            }
            out.trim_end().to_string()
        }
        ResponseBody::Adopted(outcome) => {
            let name = &outcome.service.service.name;
            let mut out = match (&outcome.declared, &outcome.replaced_command) {
                (true, _) => format!("declared '{name}'\n"),
                (false, Some(replaced)) => {
                    format!("corrected '{name}'\n  was     {replaced}\n")
                }
                (false, None) => format!("'{name}' was already declared\n"),
            };
            out.push_str(&format!("  command {}\n", outcome.service.service.command));
            out.push_str(&format!("  cwd     {}\n", outcome.service.service.cwd.display()));
            // Where the command came from decides how much to trust it, so it
            // is said out loud rather than left for the caller to assume.
            out.push_str(match outcome.command_source {
                runtime_types::CommandSource::Recorded => {
                    "  taken from the launch that was recorded for it\n"
                }
                runtime_types::CommandSource::ProcessArgv => {
                    "  taken from the running process, not from package.json\n"
                }
                runtime_types::CommandSource::Supervisor => {
                    "  taken from the supervisor, which holds what it runs next\n"
                }
            });
            // Which stack it is in, because that is what decides whether it
            // can be started — and being startable later is what adopting is
            // for.
            if let Some(stack) = &outcome.stack {
                out.push_str(&format!("  in stack {stack}, so it can be started from here\n"));
            }
            if let Some(supervisor) = &outcome.supervisor {
                out.push_str(&format!(
                    "  still kept alive by {supervisor}: starting it here will fight with it\n"
                ));
            }
            out.trim_end().to_string()
        }
        ResponseBody::Failures { items } => {
            if items.is_empty() {
                "Nothing is failing.".to_string()
            } else {
                items
                    .iter()
                    .map(|failure| {
                        let code = failure
                            .exit_code
                            .map(|code| format!(" (exit {code})"))
                            .unwrap_or_default();
                        let mut out =
                            format!("✕ {} — {}{code}", failure.subject, render::status_label(failure.status));
                        for line in &failure.detail {
                            out.push_str(&format!("\n    {line}"));
                        }
                        if failure.detail.is_empty() {
                            out.push_str("\n    (it said nothing)");
                        }
                        out
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
        }
        ResponseBody::Findings { items } => {
            if items.is_empty() {
                "Nothing to report.".to_string()
            } else {
                items
                    .iter()
                    .map(|finding| {
                        format!(
                            "{} {}: {}",
                            if finding.certain { "!" } else { "?" },
                            finding.subject,
                            finding.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        ResponseBody::Launches { items } => {
            if items.is_empty() {
                "nothing recorded".to_string()
            } else {
                items
                    .iter()
                    .map(|entry| {
                        let mark = match entry.state {
                            runtime_types::LaunchState::Bound => "\u{25cf}",
                            runtime_types::LaunchState::Pending => "\u{25cb}",
                        };
                        let where_it_went = match (entry.port, entry.pid) {
                            (Some(port), Some(pid)) => format!(":{port} pid {pid}"),
                            _ => "waiting for a port".to_string(),
                        };
                        format!(
                            "{mark} {}\n    {}\n    {where_it_went}",
                            one_line(&entry.command),
                            entry.cwd.display()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
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
    runtime_ipc::client::connect_or_start().await
}

async fn daemon_command(command: &DaemonCommand, json: bool) -> Result<String> {
    match command {
        // Idempotent, and reports the daemon either way: other clients (the
        // MCP server) use this as their bootstrap and need the endpoint back
        // whether or not they were the ones to start it.
        DaemonCommand::Start => {
            let mut client = runtime_ipc::client::connect_or_start().await?;
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
            // Still valid JSON when asked for JSON, so a script does not have
            // to distinguish "down" by parse failure.
            Err(_) if json => Ok("{\n  \"running\": false\n}".to_string()),
            Err(_) => Ok("not running".to_string()),
        },
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

    // What is wired into the tools that would use this.
    out.push_str(&format!("\nclaude code hook  {}\n", hook::status()));
    out.push_str(&format!("mcp server        {}\n", hook::mcp_status()));

    // And what is wrong with what is declared — which needs the registry, so
    // only when there is a daemon to ask.
    if runtime_ipc::client::is_running().await {
        let mut client = Client::connect_default().await?;
        if let ResponseBody::Findings { items } = client.call(Request::Diagnose).await? {
            out.push_str("\ndeclared services\n");
            if items.is_empty() {
                out.push_str("  nothing to report\n");
            }
            for finding in &items {
                // The certain ones first in the eye: they will fail, where the
                // rest only might.
                out.push_str(&format!(
                    "  {} {}: {}\n",
                    if finding.certain { "!" } else { "?" },
                    finding.subject,
                    finding.message
                ));
            }
        }
    }

    Ok(out.trim_end().to_string())
}

/// A command as a single readable line.
///
/// Recorded commands are whatever was typed, and an agent's shell call is
/// routinely a whole script. Printing it verbatim breaks a list into
/// unattributable fragments, so the log shows the shape of the command and the
/// full text stays in `--json`.
fn one_line(command: &str) -> String {
    const WIDTH: usize = 96;
    let collapsed = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= WIDTH {
        return collapsed;
    }
    let kept: String = collapsed.chars().take(WIDTH - 1).collect();
    format!("{kept}\u{2026}")
}

#[cfg(test)]
mod render_tests {
    use super::one_line;

    #[test]
    fn a_script_becomes_one_line() {
        assert_eq!(one_line("cd web\nexport A=1\npnpm dev"), "cd web export A=1 pnpm dev");
    }

    #[test]
    fn a_long_command_is_cut_rather_than_wrapped() {
        let long = "pnpm dev ".repeat(40);
        let rendered = one_line(&long);
        assert_eq!(rendered.chars().count(), 96);
        assert!(rendered.ends_with('\u{2026}'));
    }

    #[test]
    fn a_short_command_is_untouched() {
        assert_eq!(one_line("pnpm dev"), "pnpm dev");
    }
}

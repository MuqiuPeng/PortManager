//! Terminal output.
//!
//! Two audiences: a human scanning a table, and a script reading `--json`.
//! Both render the same data from the daemon, never a locally recomputed view.

use runtime_core::discover::Discovery;
use runtime_types::{
    ContainerView, DaemonInfo, ExternalService, HealthReport, LogLine, PortOwner, PortStatus, ProjectView, ServiceStatus,
    ServiceView, StartOutcome, SupervisedView, StackView, Workspace,
};

/// A filled dot for live services, hollow for stopped — readable at a glance
/// in a list of twenty.
pub fn status_dot(status: ServiceStatus) -> &'static str {
    match status {
        ServiceStatus::Healthy => "●",
        ServiceStatus::Starting | ServiceStatus::Stopping => "◐",
        ServiceStatus::Unhealthy => "◍",
        ServiceStatus::Failed => "✕",
        ServiceStatus::Stopped | ServiceStatus::Unknown => "○",
    }
}

pub fn status_label(status: ServiceStatus) -> &'static str {
    match status {
        ServiceStatus::Starting => "starting",
        ServiceStatus::Healthy => "healthy",
        ServiceStatus::Unhealthy => "unhealthy",
        ServiceStatus::Stopping => "stopping",
        ServiceStatus::Stopped => "stopped",
        ServiceStatus::Failed => "failed",
        ServiceStatus::Unknown => "unknown",
    }
}

pub fn projects(views: &[ProjectView]) -> String {
    if views.is_empty() {
        return "No projects registered. Add one with `runtime project add <path>`.".to_string();
    }

    let mut out = String::new();
    for (index, view) in views.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        // Something being up is what the dot means, whoever started it.
        let live = view.running_services > 0 || view.external_services > 0;
        let external = if view.external_services > 0 {
            format!(", {} external", view.external_services)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{} {}  {}/{} running{external}\n",
            if live { "●" } else { "○" },
            view.project.name,
            view.running_services,
            view.total_services
        ));
        out.push_str(&format!("  {}\n", view.project.root_path.display()));

        for workspace in &view.workspaces {
            let branch = workspace
                .workspace
                .git_branch
                .as_deref()
                .unwrap_or("(detached)");
            let marker = if workspace.workspace.worktree {
                " [worktree]"
            } else {
                ""
            };
            out.push_str(&format!("  {branch}{marker}\n"));
            // A declared group is one thing. Its members are shown under it,
            // and not again in the loose list — the same rule the window
            // follows, so the two do not disagree about what exists.
            for stack in &workspace.stacks {
                out.push_str(&format!("    {}\n", stack_header(stack)));
                // Members in the order the group actually starts them, and
                // indented by how deep they wait — the list is the diagram.
                for node in &stack.flow {
                    let member = stack
                        .services
                        .iter()
                        .find(|view| view.service.name == node.name);
                    let indent = "  ".repeat(node.level);
                    match member {
                        Some(view) => {
                            out.push_str(&format!("      {indent}{}\n", service_line(view)))
                        }
                        None => out.push_str(&format!("      {indent}✕ {} (missing)\n", node.name)),
                    }
                }
            }
            let grouped: Vec<&str> = workspace
                .stacks
                .iter()
                .flat_map(|stack| stack.stack.members.iter().map(String::as_str))
                .collect();
            for service in &workspace.services {
                if grouped.contains(&service.service.name.as_str()) {
                    continue;
                }
                out.push_str(&format!("    {}\n", service_line(service)));
            }
            for entry in &workspace.supervised {
                out.push_str(&format!("    {}\n", supervised_line(entry)));
            }
            for container in &workspace.containers {
                out.push_str(&format!("    {}\n", container_line(container)));
            }
            for external in &workspace.external {
                out.push_str(&format!("    {}\n", external_line(external)));
            }
        }
    }
    out
}

/// A group's own header: the unit somebody declared, and how it is made.
pub fn stack_header(view: &StackView) -> String {
    let total = view.stack.members.len();
    let stays = view.flow.iter().filter(|node| !node.one_shot).count();
    let mark = if stays > 0 && view.running == stays {
        "\u{25cf}"
    } else if view.running > 0 {
        "\u{25d0}"
    } else {
        "\u{25cb}"
    };
    let missing = if view.missing.is_empty() {
        String::new()
    } else {
        format!("  ! missing {}", view.missing.join(", "))
    };
    // A one-shot has no steady state, so it is not part of "how much of this
    // is up" — it is counted separately, as a thing that is run rather than a
    // thing that stays. A stack of nothing but one-shots has no up-ness at all
    // and says so.
    let one_shots = view.flow.iter().filter(|node| node.one_shot).count();
    let stays_up = total.saturating_sub(one_shots);
    let ran = if one_shots == 0 {
        String::new()
    } else if one_shots == 1 {
        ", 1 one-shot".to_string()
    } else {
        format!(", {one_shots} one-shots")
    };
    let head = if stays_up == 0 {
        format!("{one_shots} to run")
    } else {
        format!("{}/{stays_up} up{ran}", view.running)
    };
    format!("{mark} {}  {head}{missing}", view.stack.name)
}

/// A group's shape: one line per level, members that can start together on it.
///
/// `a -> b -> c` could only ever describe a line, and a group is a graph — two
/// services that wait for the same thing and for nothing else start at the
/// same time, and saying so is most of what the shape is for.
pub fn stack_flow(view: &StackView) -> String {
    if view.flow.is_empty() {
        return format!("    {}", view.stack.members.join(", "));
    }
    let depth = view.flow.iter().map(|node| node.level).max().unwrap_or(0);
    (0..=depth)
        .map(|level| {
            let here: Vec<&str> = view
                .flow
                .iter()
                .filter(|node| node.level == level)
                .map(|node| node.name.as_str())
                .collect();
            let lead = if level == 0 { "    " } else { "    ↳ " };
            format!("{lead}{}", here.join("  "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn stacks(items: &[StackView]) -> String {
    if items.is_empty() {
        return "No stacks declared.".to_string();
    }
    items
        .iter()
        .map(|view| format!("{}\n{}", stack_header(view), stack_flow(view)))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn service_line(view: &ServiceView) -> String {
    let port = view
        .actual_port
        .map(|port| format!(":{port}"))
        .unwrap_or_else(|| "-".to_string());
    // A service found already listening cannot be stopped from here, and
    // saying so is more useful than a status that implies it can.
    let note = if view.status.is_live() && !view.managed {
        "  (started elsewhere)"
    } else {
        ""
    };
    format!(
        "{} {:<12} {:<8} {}{note}",
        status_dot(view.status),
        view.service.name,
        port,
        status_label(view.status)
    )
}

/// A container compose defines for this checkout.
pub fn supervised_line(view: &SupervisedView) -> String {
    let ports = if view.ports.is_empty() {
        "-".to_string()
    } else {
        view.ports
            .iter()
            .map(|port| format!(":{port}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    // The warning goes on the row, not in a detail pane. It is only useful
    // before somebody restarts the service, and by the time they open a detail
    // pane they have usually decided to.
    let warning = if view.restart_warning.is_some() {
        "  ! restarting this will fail"
    } else {
        ""
    };
    format!(
        "{} {:<12} {:<8} {}  [{} {}]{warning}",
        if view.status == "online" { "◈" } else { "◇" },
        view.name,
        ports,
        view.status,
        view.supervisor,
        view.name,
    )
}

pub fn container_line(view: &ContainerView) -> String {
    let ports = if view.ports.is_empty() {
        "-".to_string()
    } else {
        view.ports
            .iter()
            .map(|port| format!(":{port}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let health = view
        .health
        .as_deref()
        .map(|health| format!(" ({health})"))
        .unwrap_or_default();
    format!(
        "{} {:<12} {:<8} {}{health}  [container {}]",
        if view.is_running() { "▣" } else { "▢" },
        view.service.as_deref().unwrap_or(&view.name),
        ports,
        view.status,
        view.name
    )
}

/// A live port in this checkout that no declared service explains.
fn external_line(external: &ExternalService) -> String {
    let what = external
        .container
        .clone()
        .or_else(|| {
            external
                .command_line
                .as_ref()
                .map(|command| command.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
        })
        .unwrap_or_else(|| format!("pid {}", external.pid));
    let supervisor = external
        .supervisor
        .as_ref()
        .map(|kind| format!("  [{kind}]"))
        .unwrap_or_default();
    format!(
        "◆ {:<12} :{:<7} {}{supervisor}",
        "(external)", external.port, what
    )
}

pub fn services(views: &[ServiceView]) -> String {
    if views.is_empty() {
        return "No services.".to_string();
    }
    views
        .iter()
        .map(service_line)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn service_detail(view: &ServiceView) -> String {
    let mut out = format!(
        "{} {} — {}\n",
        status_dot(view.status),
        view.service.name,
        status_label(view.status)
    );
    out.push_str(&format!("  command   {}\n", view.service.command));
    out.push_str(&format!("  cwd       {}\n", view.service.cwd.display()));
    if let Some(port) = view.actual_port {
        out.push_str(&format!("  port      {port}\n"));
    }
    if !view.service.env.is_empty() {
        // Names only: a value here is usually a credential.
        let names: Vec<&str> = view.service.env.keys().map(String::as_str).collect();
        out.push_str(&format!("  env       {}\n", names.join(", ")));
    }
    if let Some(url) = &view.url {
        out.push_str(&format!("  url       {url}\n"));
    }
    // `instance` is the last run this runtime performed, which may have ended
    // — or may belong to a different process than the one now on the port.
    // Reporting its pid for an adopted service claims we started it.
    match (&view.instance, view.status.is_live(), view.managed) {
        (Some(instance), true, true) => {
            out.push_str(&format!("  pid       {}\n", instance.pid));
            out.push_str(&format!(
                "  started   {} by {}\n",
                instance.started_at.to_rfc3339(),
                serde_json::to_value(instance.started_by)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unknown".to_string())
            ));
        }
        (_, true, false) => {
            out.push_str("  owner     started outside the runtime; cannot be stopped here\n");
        }
        _ => {}
    }
    out
}

pub fn start_outcome(outcome: &StartOutcome) -> String {
    let mut out = if outcome.reused {
        format!("{} is already running", outcome.service.service.name)
    } else {
        format!("started {}", outcome.service.service.name)
    };
    if let Some(reservation) = &outcome.reservation {
        out.push_str(&format!(" on port {}", reservation.port));
        // Say so explicitly: a silently different port is the exact confusion
        // this tool exists to remove.
        if reservation.reallocated {
            if let Some(preferred) = reservation.preferred_port {
                out.push_str(&format!(" (preferred {preferred} was taken)"));
            }
        }
    }
    // "Already running" and "under management" are not the same thing, and
    // saying only the first is how somebody asks the runtime to start a
    // service, is told it is running, and finds later that stopping it from
    // here is refused. The start succeeded; it just did not take charge of
    // anything, and nothing else in this reply would say so.
    if outcome.reused && !outcome.service.managed {
        out.push_str(
            "\n  ! started outside the runtime, so it is not managed here; `take-over` to change that",
        );
    }
    // Before the status, because it is the part that will not announce itself:
    // the start succeeds either way, and what it broke shows up hours later on
    // somebody else's restart.
    if let Some(warning) = &outcome.warning {
        out.push_str(&format!("\n  ! {warning}"));
    }
    // The status, not just a URL: a service can be spawned and reserved a port
    // and still not be serving on it.
    out.push_str(&format!("\n  status {}", status_label(outcome.service.status)));
    if let Some(url) = &outcome.service.url {
        out.push_str(&format!("\n  {url}"));
    }
    if outcome.service.status == ServiceStatus::Starting {
        out.push_str("\n  (use --wait, or `runtime health`, to confirm it is serving)");
    }
    out
}

pub fn port_status(status: &PortStatus) -> String {
    if status.available {
        return format!("port {} is free", status.port);
    }
    let mut out = match &status.owner {
        Some(owner) => format!("port {} is in use ({})", status.port, owner.protocol),
        None => format!("port {} is in use", status.port),
    };
    if let Some(owner) = &status.owner {
        out.push('\n');
        out.push_str(&port_owner(owner));
    }
    if let Some(suggested) = status.suggested_port {
        out.push_str(&format!("\nsuggested port: {suggested}"));
    }
    out
}

pub fn port_owner(owner: &PortOwner) -> String {
    let mut out = String::new();
    // Containers have no branch, so a placeholder there is noise rather than
    // information; only name the parts that are actually known.
    let identity = [
        owner.project_name.as_deref(),
        owner.git_branch.as_deref(),
        owner.service_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("/");
    if identity.is_empty() {
        out.push_str("  unregistered process\n");
    } else {
        out.push_str(&format!("  {identity}\n"));
    }
    if let Some(container) = &owner.container {
        // The pid is Docker's, not the service's, so it is not the useful fact.
        out.push_str(&format!("  container {container}\n"));
    } else {
        out.push_str(&format!("  pid {}\n", owner.pid));
    }
    if let Some(cwd) = &owner.cwd {
        out.push_str(&format!("  cwd {}\n", cwd.display()));
    }
    if let Some(command) = &owner.command_line {
        out.push_str(&format!("  cmd {command}\n"));
    }
    if let Some(started_by) = owner.started_by {
        if let Some(label) = serde_json::to_value(started_by)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
        {
            out.push_str(&format!("  started by {label}\n"));
        }
    }
    if let Some(supervisor) = &owner.supervisor {
        // Before "not managed by the runtime", because it is the more useful
        // half: stopping this by hand achieves nothing while something else is
        // watching for it to go.
        out.push_str(&format!("  kept alive by {supervisor}\n"));
    }
    if !owner.managed {
        out.push_str("  not managed by the runtime\n");
    }
    out.trim_end().to_string()
}

pub fn ports(owners: &[PortOwner]) -> String {
    if owners.is_empty() {
        return "Nothing is listening.".to_string();
    }
    let mut out = format!(
        "{:<8} {:<5} {:<8} {:<34} {}\n",
        "PORT", "PROTO", "PID", "PROJECT/BRANCH/SERVICE", "CWD"
    );
    for owner in owners {
        // Include the branch: two worktrees of one project otherwise render as
        // the same row, which is exactly the ambiguity this table exists to
        // remove.
        // The branch is known for anything resolved through a workspace, even
        // when the runtime did not start it and so cannot name the service.
        let pid = match &owner.container {
            Some(_) => "docker".to_string(),
            None => owner.pid.to_string(),
        };
        // Everything known, in order; a container with no compose labels still
        // has a name, which beats an empty cell.
        let label = [
            owner.project_name.as_deref(),
            owner.git_branch.as_deref(),
            owner.service_name.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("/");
        let label = if label.is_empty() {
            "-".to_string()
        } else {
            label
        };
        out.push_str(&format!(
            "{:<8} {:<5} {:<8} {:<34} {}\n",
            owner.port,
            owner.protocol,
            pid,
            label,
            owner
                .cwd
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "-".to_string())
        ));
    }
    out.trim_end().to_string()
}

pub fn logs(lines: &[LogLine]) -> String {
    if lines.is_empty() {
        return "(no output captured)".to_string();
    }
    lines
        .iter()
        .map(|line| {
            let marker = match line.stream {
                runtime_types::LogStream::Stderr => "!",
                runtime_types::LogStream::System => "·",
                runtime_types::LogStream::Stdout => " ",
            };
            format!("{} {} {}", line.timestamp.format("%H:%M:%S"), marker, line.message)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn health(report: &HealthReport) -> String {
    let mut out = format!(
        "{} {}",
        status_dot(report.status),
        status_label(report.status)
    );
    if let Some(detail) = &report.detail {
        out.push_str(&format!(" — {detail}"));
    }
    out
}

pub fn workspaces(items: &[Workspace]) -> String {
    if items.is_empty() {
        return "No workspaces.".to_string();
    }
    items
        .iter()
        .map(|workspace| {
            format!(
                "{:<24} +{:<3} {}",
                workspace.git_branch.as_deref().unwrap_or("(detached)"),
                workspace.port_offset,
                workspace.path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn daemon_info(info: &DaemonInfo) -> String {
    format!(
        "running\n  version   {}\n  platform  {}\n  pid       {}\n  socket    {}\n  database  {}\n  uptime    {}s",
        info.version,
        info.platform,
        info.pid,
        info.socket_path.display(),
        info.database_path.display(),
        info.uptime_seconds
    )
}

pub fn discoveries(items: &[Discovery]) -> String {
    if items.is_empty() {
        return "Found nothing. Pass --path to scan a directory tree, or add a project explicitly with `runtime project add <path>`.".to_string();
    }

    let mut out = String::new();
    let (running, idle): (Vec<_>, Vec<_>) = items.iter().partition(|item| item.running);

    if !running.is_empty() {
        out.push_str("Running now\n");
        for item in &running {
            out.push_str(&discovery_line(item));
        }
    }
    if !idle.is_empty() {
        if !running.is_empty() {
            out.push('\n');
        }
        out.push_str("Found on disk\n");
        for item in &idle {
            out.push_str(&discovery_line(item));
        }
    }

    let new = items.iter().filter(|item| !item.registered).count();
    if new > 0 {
        out.push_str(&format!(
            "\n{new} not registered yet. Add them with `runtime scan --add`."
        ));
    }
    out.trim_end().to_string()
}

fn discovery_line(item: &Discovery) -> String {
    let ports = if item.ports.is_empty() {
        String::new()
    } else {
        format!(
            "  {}",
            item.ports
                .iter()
                .map(|port| format!(":{port}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let mark = if item.registered { "✓" } else { " " };
    let branch = item
        .git_branch
        .as_deref()
        .map(|branch| format!(" ({branch})"))
        .unwrap_or_default();

    let mut line = format!("{mark} {:<22}{}{}\n", item.name, ports, branch);
    line.push_str(&format!("    {}\n", item.root_path.display()));
    if !item.suggested_services.is_empty() {
        line.push_str(&format!(
            "    services: {}\n",
            item.suggested_services.join(", ")
        ));
    }
    line
}

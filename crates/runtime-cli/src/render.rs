//! Terminal output.
//!
//! Two audiences: a human scanning a table, and a script reading `--json`.
//! Both render the same data from the daemon, never a locally recomputed view.

use runtime_core::discover::Discovery;
use runtime_types::{
    DaemonInfo, HealthReport, LogLine, PortOwner, PortStatus, ProjectView, ServiceStatus,
    ServiceView, StartOutcome, Workspace,
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
        out.push_str(&format!(
            "{} {}  {}/{} running\n",
            if view.running_services > 0 { "●" } else { "○" },
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
            for service in &workspace.services {
                out.push_str(&format!("    {}\n", service_line(service)));
            }
        }
    }
    out
}

pub fn service_line(view: &ServiceView) -> String {
    let port = view
        .actual_port
        .map(|port| format!(":{port}"))
        .unwrap_or_else(|| "-".to_string());
    format!(
        "{} {:<12} {:<8} {}",
        status_dot(view.status),
        view.service.name,
        port,
        status_label(view.status)
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
    if let Some(url) = &view.url {
        out.push_str(&format!("  url       {url}\n"));
    }
    if let Some(instance) = &view.instance {
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
    if let Some(url) = &outcome.service.url {
        out.push_str(&format!("\n{url}"));
    }
    out
}

pub fn port_status(status: &PortStatus) -> String {
    if status.available {
        return format!("port {} is free", status.port);
    }
    let mut out = format!("port {} is in use", status.port);
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
    match (&owner.project_name, &owner.service_name) {
        (Some(project), Some(service)) => {
            let branch = owner.git_branch.as_deref().unwrap_or("-");
            out.push_str(&format!("  {project}/{branch}/{service}\n"));
        }
        (Some(project), None) => out.push_str(&format!("  {project}\n")),
        _ => out.push_str("  unregistered process\n"),
    }
    out.push_str(&format!("  pid {}\n", owner.pid));
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
    if !owner.managed {
        out.push_str("  not managed by the runtime\n");
    }
    out.trim_end().to_string()
}

pub fn ports(owners: &[PortOwner]) -> String {
    if owners.is_empty() {
        return "Nothing is listening.".to_string();
    }
    let mut out = format!("{:<8} {:<8} {:<34} {}\n", "PORT", "PID", "PROJECT/BRANCH/SERVICE", "CWD");
    for owner in owners {
        // Include the branch: two worktrees of one project otherwise render as
        // the same row, which is exactly the ambiguity this table exists to
        // remove.
        // The branch is known for anything resolved through a workspace, even
        // when the runtime did not start it and so cannot name the service.
        let label = match (&owner.project_name, &owner.git_branch, &owner.service_name) {
            (Some(project), Some(branch), Some(service)) => format!("{project}/{branch}/{service}"),
            (Some(project), Some(branch), None) => format!("{project}/{branch}"),
            (Some(project), None, Some(service)) => format!("{project}/{service}"),
            (Some(project), None, None) => project.clone(),
            _ => "-".to_string(),
        };
        out.push_str(&format!(
            "{:<8} {:<8} {:<34} {}\n",
            owner.port,
            owner.pid,
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

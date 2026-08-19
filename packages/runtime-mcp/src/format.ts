// Tool results are rendered as compact text rather than raw JSON.
//
// An agent reads "web :3004 healthy, started by claude-code" in a handful of
// tokens; the equivalent JSON costs an order of magnitude more and says no more.
// Anything a caller might need to act on — ids, ports, pids — is still present.

import type {
  HealthReport,
  LogLine,
  PortOwner,
  PortReservation,
  PortStatus,
  ProjectView,
  ServiceView,
  StartOutcome,
  Workspace,
} from "./protocol.js";

export function formatProjects(projects: ProjectView[]): string {
  if (projects.length === 0) {
    return "No projects are registered. Register one with the runtime CLI: `runtime project add <path>`.";
  }
  return projects
    .map(
      (project) =>
        `${project.name} — ${project.running_services}/${project.total_services} running\n` +
        `  ${project.root_path}`,
    )
    .join("\n");
}

export function formatProjectRuntime(project: ProjectView): string {
  const lines = [
    `${project.name} — ${project.running_services}/${project.total_services} running`,
    `  ${project.root_path}`,
  ];

  for (const workspace of project.workspaces) {
    const branch = workspace.git_branch ?? "(detached)";
    const marker = workspace.worktree ? ` [worktree +${workspace.port_offset}]` : "";
    lines.push(`\n${branch}${marker}`);
    if (workspace.services.length === 0) {
      lines.push("  (no services)");
      continue;
    }
    for (const service of workspace.services) {
      lines.push(`  ${formatService(service)}`);
    }
  }
  return lines.join("\n");
}

const LIVE: ReadonlySet<string> = new Set(["starting", "healthy", "unhealthy", "stopping"]);

export function formatService(service: ServiceView): string {
  // `instance` is the *last* run, which may have ended. Reporting its pid and
  // owner for a stopped service reads as though it were still running.
  const live = LIVE.has(service.status);
  const instance = live ? service.instance : undefined;

  const port = service.actual_port ? `:${service.actual_port}` : "no port";
  const owner =
    instance && instance.started_by !== "unknown" ? `, started by ${instance.started_by}` : "";
  const pid = instance ? `, pid ${instance.pid}` : "";
  // The id comes last so the line stays readable, but is always available for
  // a follow-up call that must be unambiguous.
  return `${service.name} ${port} ${service.status}${owner}${pid} [id ${service.id}]`;
}

export function formatServices(services: ServiceView[]): string {
  if (services.length === 0) return "No services.";
  return services.map((service) => formatService(service)).join("\n");
}

export function formatServiceDetail(service: ServiceView): string {
  const lines = [
    formatService(service),
    `  command  ${service.command}`,
    `  cwd      ${service.cwd}`,
  ];
  if (service.url) lines.push(`  url      ${service.url}`);
  if (service.preferred_port) lines.push(`  prefers  :${service.preferred_port}`);
  return lines.join("\n");
}

export function formatStart(outcome: StartOutcome): string {
  const verb = outcome.reused ? "already running" : "started";
  const lines = [`${outcome.service.name} ${verb}`];

  if (outcome.reservation) {
    const { port, preferred_port, reallocated } = outcome.reservation;
    lines.push(
      reallocated && preferred_port
        ? `  port ${port} (preferred ${preferred_port} was taken)`
        : `  port ${port}`,
    );
  }
  if (outcome.service.url) lines.push(`  ${outcome.service.url}`);
  lines.push(`  status ${outcome.service.status}`);
  return lines.join("\n");
}

export function formatHealth(report: HealthReport): string {
  return report.detail ? `${report.status} — ${report.detail}` : report.status;
}

export function formatPortStatus(status: PortStatus): string {
  if (status.available) return `Port ${status.port} is free.`;

  const lines = [`Port ${status.port} is in use.`];
  if (status.owner) lines.push(indent(formatPortOwner(status.owner)));
  if (status.suggested_port) {
    lines.push(`  Suggested alternative: ${status.suggested_port}`);
  }
  return lines.join("\n");
}

export function formatPortOwner(owner: PortOwner): string {
  const lines: string[] = [];
  if (owner.project_name) {
    const parts = [owner.project_name, owner.git_branch, owner.service_name].filter(Boolean);
    lines.push(parts.join("/"));
  } else {
    lines.push("unregistered process");
  }
  lines.push(`pid ${owner.pid}`);
  if (owner.cwd) lines.push(`cwd ${owner.cwd}`);
  if (owner.command_line) lines.push(`cmd ${truncate(owner.command_line, 160)}`);
  if (owner.started_by && owner.started_by !== "unknown") {
    lines.push(`started by ${owner.started_by}`);
  }
  // Say it plainly: this is the process the runtime will refuse to terminate.
  if (!owner.managed) lines.push("not managed by the runtime");
  return lines.join("\n");
}

export function formatPorts(ports: PortOwner[]): string {
  if (ports.length === 0) return "Nothing is listening.";
  return ports
    .map((port) => {
      const label = port.project_name
        ? [port.project_name, port.git_branch, port.service_name].filter(Boolean).join("/")
        : (port.executable?.split(/[/\\]/).pop() ?? "unknown");
      return `${port.port}\t${label}\tpid ${port.pid}${port.managed ? "" : "\t(unmanaged)"}`;
    })
    .join("\n");
}

export function formatReservation(reservation: PortReservation): string {
  const lines = [`Reserved port ${reservation.port}.`];
  if (reservation.reallocated && reservation.preferred_port) {
    lines.push(`  Preferred ${reservation.preferred_port} was taken.`);
  }
  if (reservation.conflict) {
    lines.push("  Current holder:");
    lines.push(indent(formatPortOwner(reservation.conflict), "    "));
  }
  return lines.join("\n");
}

export function formatLogs(lines: LogLine[]): string {
  if (lines.length === 0) return "(no output)";
  const body = lines
    .map((line) => {
      const marker = line.stream === "stderr" ? "!" : line.stream === "system" ? "·" : " ";
      return `${line.timestamp.slice(11, 19)} ${marker} ${line.message}`;
    })
    .join("\n");
  // The cursor lets a follow-up call ask only for what is new, which is what
  // keeps repeated log reads from filling the agent's context.
  return `${body}\n\n(next cursor: ${lines[lines.length - 1]!.seq})`;
}

export function formatWorktrees(workspaces: Workspace[]): string {
  if (workspaces.length === 0) return "No workspaces.";
  return workspaces
    .map((workspace) => {
      const branch = workspace.git_branch ?? "(detached)";
      const kind = workspace.worktree ? "worktree" : "primary";
      return `${branch}\t${kind}\tport offset +${workspace.port_offset}\t${workspace.path}`;
    })
    .join("\n");
}

function indent(text: string, prefix = "  "): string {
  return text
    .split("\n")
    .map((line) => prefix + line)
    .join("\n");
}

function truncate(text: string, max: number): string {
  return text.length <= max ? text : `${text.slice(0, max - 1)}…`;
}

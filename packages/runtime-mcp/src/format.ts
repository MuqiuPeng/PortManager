// Tool results are rendered as compact text rather than raw JSON.
//
// An agent reads "web :3004 healthy, started by claude-code" in a handful of
// tokens; the equivalent JSON costs an order of magnitude more and says no more.
// Anything a caller might need to act on — ids, ports, pids — is still present.

import type {
  AdoptOutcome,
  ContainerView,
  Discovery,
  HealthReport,
  LaunchObservation,
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

export function formatDiscoveries(items: Discovery[]): string {
  if (items.length === 0) {
    return "Found nothing. Pass paths to scan directory trees for projects that are not running.";
  }

  const lines = items.map((item) => {
    const ports = (item.ports ?? []).map((port) => `:${port}`).join(" ");
    const branch = item.git_branch ? ` (${item.git_branch})` : "";
    const state = item.registered ? "registered" : "not registered";
    const running = item.running ? ", running" : "";
    const services = (item.suggested_services ?? []).length
      ? `\n    services: ${(item.suggested_services ?? []).join(", ")}`
      : "";
    return `${item.name}${branch} ${ports}\n    ${item.root_path}\n    ${state}${running}${services}`;
  });

  const pending = items.filter((item) => !item.registered).length;
  if (pending > 0) {
    lines.push(`\n${pending} not registered. Call again with adopt: true to register them.`);
  }
  return lines.join("\n");
}

export function formatProjectRuntime(project: ProjectView): string {
  const external = project.external_services ?? 0;
  const lines = [
    `${project.name} — ${project.running_services}/${project.total_services} running` +
      (external > 0 ? `, ${external} started outside the runtime` : ""),
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
    for (const container of workspace.containers ?? []) {
      lines.push(`  ${formatContainer(container)}`);
    }
    // Named but not attributed: knowing something is on :3001 in this checkout
    // is useful; deciding which declared service it is would be a guess.
    for (const item of workspace.external ?? []) {
      const what = item.container ?? item.command_line?.split(/\s+/)[0] ?? `pid ${item.pid}`;
      lines.push(`  (external) :${item.port} ${what} — not started by the runtime`);
    }
  }
  return lines.join("\n");
}

const LIVE: ReadonlySet<string> = new Set(["starting", "healthy", "unhealthy", "stopping"]);

export function formatContainer(container: ContainerView): string {
  const ports = (container.ports ?? []).map((port) => `:${port}`).join(" ") || "no port";
  const health = container.health ? ` (${container.health})` : "";
  // The container name is what start/stop take, so it is always present.
  return `${container.service ?? container.name} ${ports} ${container.status}${health} [container ${container.name}]`;
}

export function formatService(service: ServiceView): string {
  // `instance` is the *last* run, which may have ended. Reporting its pid and
  // owner for a stopped service reads as though it were still running.
  const live = LIVE.has(service.status);
  const instance = live ? service.instance : undefined;

  const port = service.actual_port ? `:${service.actual_port}` : "no port";
  const owner =
    instance && instance.started_by !== "unknown" ? `, started by ${instance.started_by}` : "";
  const pid = instance ? `, pid ${instance.pid}` : "";
  // Worth saying plainly: stop_service cannot act on this one.
  const unmanaged = live && service.managed === false ? ", started outside the runtime" : "";
  // The id comes last so the line stays readable, but is always available for
  // a follow-up call that must be unambiguous.
  return `${service.name} ${port} ${service.status}${owner}${pid}${unmanaged} [id ${service.id}]`;
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

  // Names only. Confirming that a variable was set does not require putting its
  // value — which is usually a credential — into an agent's transcript.
  const names = Object.keys(service.env ?? {});
  if (names.length > 0) {
    lines.push(`  env      ${names.sort().join(", ")}`);
  }
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
  // For a container the pid belongs to Docker, not to the service.
  lines.push(owner.container ? `container ${owner.container}` : `pid ${owner.pid}`);
  if (owner.cwd) lines.push(`cwd ${owner.cwd}`);
  if (owner.command_line) lines.push(`cmd ${truncate(owner.command_line, 160)}`);
  if (owner.started_by && owner.started_by !== "unknown") {
    lines.push(`started by ${owner.started_by}`);
  }
  // The more useful half of "not managed": stopping this by hand achieves
  // nothing while something else is watching for it to go.
  if (owner.supervisor) lines.push(`kept alive by ${owner.supervisor}`);
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
      const holder = port.container ? `container ${port.container}` : `pid ${port.pid}`;
      return `${port.port}\t${label}\t${holder}${port.managed ? "" : "\t(unmanaged)"}`;
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

export function formatAdopted(outcome: AdoptOutcome): string {
  const service = outcome.service;
  const lines = [
    outcome.declared
      ? `Declared '${service.name}'.`
      : outcome.replaced_command
        ? `Corrected '${service.name}'. It was declared as ${truncate(outcome.replaced_command, 120)}.`
        : `'${service.name}' was already declared; nothing changed.`,
    `  command ${truncate(service.command, 160)}`,
    `  cwd     ${service.cwd}`,
  ];
  // Where the command came from decides how much to trust it, so say it rather
  // than leave the caller to assume.
  lines.push(
    outcome.command_source === "recorded"
      ? "  Taken from the launch recorded for it."
      : "  Taken from the running process, not from package.json.",
  );
  if (outcome.supervisor) {
    lines.push(
      `  Still kept alive by ${outcome.supervisor}: starting it from here will fight with it.`,
    );
  }
  return lines.join("\n");
}

export function formatLaunches(items: LaunchObservation[]): string {
  if (items.length === 0) return "Nothing has been recorded.";
  return items
    .map((item) => {
      const where =
        item.port && item.pid ? `:${item.port} pid ${item.pid}` : "no port yet";
      // Collapsed: a recorded command is whatever was typed, and an agent's
      // shell call is routinely a whole script.
      const command = truncate(item.command.split(/\s+/).join(" "), 160);
      return `${item.state === "bound" ? "*" : "-"} ${command}\n    ${item.cwd}\n    ${where}, from ${item.source}`;
    })
    .join("\n");
}

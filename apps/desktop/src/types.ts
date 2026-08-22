// Mirrors the serde representation of `runtime-types`. Kept hand-written and
// small rather than generated: the protocol is stable and a build step here
// would be more machinery than the surface justifies.

export type ServiceStatus =
  | "starting"
  | "healthy"
  | "unhealthy"
  | "stopping"
  | "stopped"
  | "failed"
  | "unknown";

export type ServiceType =
  | "web"
  | "api"
  | "worker"
  | "database"
  | "cache"
  | "container"
  | "custom";

export type StartedBy =
  | "manual"
  | "desktop"
  | "cli"
  | "claude-code"
  | "codex"
  | "cursor"
  | "unknown";

export interface Project {
  id: string;
  name: string;
  root_path: string;
  repository_url?: string;
  created_at: string;
  updated_at: string;
}

export interface Workspace {
  id: string;
  project_id: string;
  path: string;
  git_branch?: string;
  git_commit?: string;
  worktree: boolean;
  port_offset: number;
  created_at: string;
}

export interface RuntimeInstance {
  id: string;
  service_id: string;
  pid: number;
  process_start_time: number;
  status: ServiceStatus;
  port?: number;
  started_at: string;
  stopped_at?: string;
  exit_code?: number;
  started_by: StartedBy;
  owner_session?: string;
}

/** `Service` is flattened into `ServiceView` on the wire. */
export interface ServiceView {
  id: string;
  workspace_id: string;
  name: string;
  service_type: ServiceType;
  command: string;
  cwd: string;
  preferred_port?: number;
  env?: Record<string, string>;
  auto_start: boolean;
  /** Services here that must be up first. Absent when there are none. */
  depends_on?: string[];
  /** Runs to completion instead of staying up: a migration, a seed. */
  one_shot?: boolean;
  status: ServiceStatus;
  instance?: RuntimeInstance;
  actual_port?: number;
  url?: string;
  /** False when the runtime found it already running and cannot stop it. */
  managed?: boolean;
  /** Another supervisor keeping this alive: "pm2", "systemd". */
  supervisor?: string;
  /** That supervisor's own name for it — enough to stop it through them. */
  supervisor_entry?: string;
}

/** A live port in a checkout that no declared service explains. */
export interface ExternalService {
  port: number;
  pid: number;
  container?: string;
  cwd?: string;
  command_line?: string;
  url?: string;
  /** Another supervisor keeping this alive. */
  supervisor?: string;
}

/**
 * Changes to a service. Every field optional: correcting a port should not
 * mean restating the command.
 */
export interface ServicePatch {
  name?: string;
  command?: string;
  cwd?: string;
  service_type?: ServiceType;
  /** `null` clears the port; omitted leaves it alone. */
  preferred_port?: number | null;
  auto_start?: boolean;
  conflict_policy?: string;
  /** Merged with what is already set. */
  env?: Record<string, string>;
  /** Variables to drop, since `env` merges. */
  remove_env?: string[];
  /** Replaced whole, not merged. An empty list clears them. */
  depends_on?: string[];
  one_shot?: boolean;
}

/** A container compose defines for a checkout, running or not. */
export interface ContainerView {
  name: string;
  service?: string;
  image: string;
  status: string;
  health?: string;
  ports?: number[];
  url?: string;
}

export interface WorkspaceView extends Workspace {
  services: ServiceView[];
  external?: ExternalService[];
  containers?: ContainerView[];
  supervised?: SupervisedView[];
  /**
   * Groups declared over these services.
   *
   * A member listed here is still present in `services`; a surface showing
   * groups is expected to show each service once, under its group.
   */
  stacks?: StackView[];
}

export interface ProjectView extends Project {
  workspaces: WorkspaceView[];
  running_services: number;
  total_services: number;
  external_services?: number;
}

export interface PanelConfig {
  edge: "left" | "right";
  width: number;
  height_ratio: number;
  island_width: number;
  island_height: number;
  hover_margin: number;
  animation_ms: number;
  pinned: boolean;
}

/** Panel geometry plus everything else it remembers between launches. */
export interface PanelSettings extends PanelConfig {
  shortcut: string;
  /** Screen id to dock to; absent follows the pointer. */
  screen?: string;
}

export interface ScreenInfo {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
  scale_factor: number;
  primary: boolean;
}

/** The two sizes the panel lives at. */
export type PanelState = "island" | "expanded";

/** A project the runtime found on its own. */
export interface Discovery {
  root_path: string;
  name: string;
  /** True when something inside it is listening right now. */
  running: boolean;
  ports?: number[];
  markers?: string[];
  git_branch?: string;
  suggested_services?: string[];
  registered: boolean;
}

export interface PortOwner {
  port: number;
  pid: number;
  executable?: string;
  cwd?: string;
  command_line?: string;
  project_id?: string;
  project_name?: string;
  workspace_id?: string;
  git_branch?: string;
  service_id?: string;
  service_name?: string;
  started_by?: StartedBy;
  /** Container publishing this port, when it is not a plain process. */
  container?: string;
  /** Only a process the runtime started may ever be terminated automatically. */
  managed: boolean;
}

export interface LogLine {
  seq: number;
  service_id: string;
  stream: "stdout" | "stderr" | "system";
  timestamp: string;
  message: string;
}

export interface StartOutcome {
  service: ServiceView;
  reused: boolean;
  reservation?: {
    port: number;
    preferred_port?: number;
    reallocated: boolean;
    policy: string;
  };
  /** Something about this start worth knowing first. */
  warning?: string;
}

export interface DaemonInfo {
  version: string;
  pid: number;
  socket_path: string;
  database_path: string;
  platform: string;
  uptime_seconds: number;
}

export type RuntimeEvent =
  | { event: "project_added"; project_id: string; name: string }
  | { event: "project_removed"; project_id: string }
  | { event: "workspace_changed"; project_id: string; workspace_id: string }
  | {
      event: "service_changed";
      project_id: string;
      service_id: string;
      removed: boolean;
    }
  | {
      event: "service_status_changed";
      service_id: string;
      status: ServiceStatus;
      port?: number;
    }
  | { event: "service_exited"; service_id: string; exit_code?: number }
  | { event: "port_lease_changed"; port: number; service_id: string }
  | { event: "log"; seq: number; service_id: string; message: string };

/**
 * Whether an event means the window should re-ask what is broken.
 *
 * Everything but a log line. A failure that clears — or a service that is
 * removed — has to reach a window showing a failure toast, or the toast stays
 * up pointing at an id the daemon no longer knows. Log lines arrive per line
 * and would mean re-diagnosing the machine while a service is merely talking.
 *
 * Written as a function over the whole union so that adding an event forces a
 * decision here rather than silently defaulting to "ignore".
 */
export function affectsFailures(event: RuntimeEvent): boolean {
  switch (event.event) {
    case "log":
      return false;
    case "project_added":
    case "project_removed":
    case "workspace_changed":
    case "service_changed":
    case "service_status_changed":
    case "service_exited":
    case "port_lease_changed":
      return true;
  }
}

/** One service in a group, placed by what it waits for. */
export interface FlowNode {
  name: string;
  service_id?: string;
  after?: string[];
  level: number;
  status: ServiceStatus;
  one_shot?: boolean;
}

/**
 * Add newly-read lines to what is already shown, once each.
 *
 * A line is identified by its seq, which counts up per service, so anything
 * not past the last line held is something already held. Filtering on that
 * rather than trusting the request to have asked for the right window: the
 * cursor is only advanced once a reply arrives, so two reads in flight at
 * once both ask from the beginning, and the whole log arrives twice.
 */
export function mergeLogs(current: LogLine[], incoming: LogLine[], keep = 500): LogLine[] {
  const last = current.length > 0 ? current[current.length - 1].seq : -1;
  const fresh = incoming.filter((line) => line.seq > last);
  if (fresh.length === 0) return current;
  return [...current, ...fresh].slice(-keep);
}

/** True while the runtime believes a process should exist. */
export function isLive(status: ServiceStatus): boolean {
  return (
    status === "starting" ||
    status === "healthy" ||
    status === "unhealthy" ||
    status === "stopping"
  );
}

/** What adopting a port produced. */
export interface AdoptOutcome {
  service: ServiceView;
  /** Where the command came from. Never the project's scripts. */
  command_source: "recorded" | "process_argv" | "supervisor";
  /** False when the service was already declared and nothing changed. */
  declared: boolean;
  /** The command written down before, when adopting replaced it. */
  replaced_command?: string;
  supervisor?: string;
}

/** A service another supervisor keeps, that the runtime can switch. */
export interface SupervisedView {
  name: string;
  /** Which supervisor: "pm2", "systemd". */
  supervisor: string;
  status: string;
  pid?: number;
  command: string;
  restarts: number;
  /** Absent, not empty, when it holds no ports: the daemon omits empty lists. */
  ports?: number[];
  url?: string;
  /** Set when restarting this would fail, with the reason. */
  restart_warning?: string;
}

/** A named sequence of steps in a checkout. */
export interface Stack {
  id: string;
  workspace_id: string;
  name: string;
  /** Service names, in order. Each brings up its own dependencies first. */
  steps: string[];
}

/** Something wrong with what is declared, found without being asked. */
export interface Finding {
  /** Where it is, as a person would say it: `Loom/api`. */
  subject: string;
  message: string;
  /** True when it will fail rather than merely might. */
  certain: boolean;
}

/** A service that is not working, with the part of its output that says why. */
export interface Failure {
  service_id: string;
  /** `Loom/api`, as a person would say it. */
  subject: string;
  status: ServiceStatus;
  at: string;
  exit_code?: number;
  /** Absent, not empty, when it said nothing. */
  detail?: string[];
}

/** A stack with what its members are actually doing. */
export interface StackView extends Stack {
  /** Its members, in the order they start. */
  services: ServiceView[];
  /** How many of them are up. */
  running: number;
  /** Steps naming a service that no longer exists. */
  missing?: string[];
  /**
   * The group as a graph: what waits for what, and what can go at once.
   *
   * Derived by the daemon from the members' own dependencies, so the diagram
   * and the order it actually starts in cannot drift apart.
   */
  flow?: FlowNode[];
}

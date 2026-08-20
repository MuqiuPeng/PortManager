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
  | {
      event: "service_status_changed";
      service_id: string;
      status: ServiceStatus;
      port?: number;
    }
  | { event: "service_exited"; service_id: string; exit_code?: number }
  | { event: "port_lease_changed"; port: number; service_id: string }
  | { event: "log"; seq: number; service_id: string; message: string };

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
  command_source: "recorded" | "process_argv";
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
export interface Task {
  id: string;
  workspace_id: string;
  name: string;
  /** Service names, in order. Each brings up its own dependencies first. */
  steps: string[];
}

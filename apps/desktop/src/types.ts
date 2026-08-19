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
  auto_start: boolean;
  status: ServiceStatus;
  instance?: RuntimeInstance;
  actual_port?: number;
  url?: string;
  /** False when the runtime found it already running and cannot stop it. */
  managed?: boolean;
}

/** A live port in a checkout that no declared service explains. */
export interface ExternalService {
  port: number;
  pid: number;
  container?: string;
  cwd?: string;
  command_line?: string;
  url?: string;
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

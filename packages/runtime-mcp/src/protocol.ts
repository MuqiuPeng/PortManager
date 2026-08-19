// The subset of the daemon protocol this server speaks. Mirrors
// `crates/runtime-ipc/src/protocol.rs`; see `docs/architecture.md`.

export interface Project {
  id: string;
  name: string;
  root_path: string;
  repository_url?: string;
}

export interface Workspace {
  id: string;
  project_id: string;
  path: string;
  git_branch?: string;
  git_commit?: string;
  worktree: boolean;
  port_offset: number;
}

export type ServiceStatus =
  | "starting"
  | "healthy"
  | "unhealthy"
  | "stopping"
  | "stopped"
  | "failed"
  | "unknown";

export interface RuntimeInstance {
  id: string;
  service_id: string;
  pid: number;
  status: ServiceStatus;
  port?: number;
  started_at: string;
  exit_code?: number;
  started_by: string;
}

export interface ServiceView {
  id: string;
  workspace_id: string;
  name: string;
  service_type: string;
  command: string;
  cwd: string;
  preferred_port?: number;
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

export interface WorkspaceView extends Workspace {
  services: ServiceView[];
  external?: ExternalService[];
}

export interface ProjectView extends Project {
  workspaces: WorkspaceView[];
  running_services: number;
  total_services: number;
  external_services?: number;
}

/** A project the runtime found on its own. */
export interface Discovery {
  root_path: string;
  name: string;
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
  project_name?: string;
  git_branch?: string;
  service_name?: string;
  started_by?: string;
  /** Container publishing this port, when it is not a plain process. */
  container?: string;
  /** Only a process the runtime started may ever be terminated automatically. */
  managed: boolean;
}

export interface PortStatus {
  port: number;
  available: boolean;
  owner?: PortOwner;
  suggested_port?: number;
}

export interface PortReservation {
  port: number;
  preferred_port?: number;
  reallocated: boolean;
  policy: string;
  conflict?: PortOwner;
}

export interface HealthReport {
  service_id: string;
  status: ServiceStatus;
  detail?: string;
  checked_port?: number;
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
  reservation?: PortReservation;
}

export interface AgentSession {
  id: string;
  provider: string;
  client: string;
}

export interface DaemonInfo {
  version: string;
  pid: number;
  platform: string;
  socket_path: string;
  database_path: string;
  uptime_seconds: number;
}

/** A request as it appears on the wire: `{ method, params }`. */
export interface Request {
  method: string;
  params?: Record<string, unknown>;
}

export type ResponseBody =
  | { type: "pong"; protocol_version: number }
  | ({ type: "info" } & DaemonInfo)
  | { type: "projects"; items: ProjectView[] }
  | { type: "discoveries"; items: Discovery[] }
  | ({ type: "project" } & ProjectView)
  | { type: "workspaces"; items: Workspace[] }
  | ({ type: "workspace" } & Workspace)
  | { type: "services"; items: ServiceView[] }
  | ({ type: "service" } & ServiceView)
  | ({ type: "started" } & StartOutcome)
  | ({ type: "health" } & HealthReport)
  | ({ type: "port" } & PortStatus)
  | { type: "ports"; items: PortOwner[] }
  | ({ type: "reservation" } & PortReservation)
  | { type: "logs"; items: LogLine[] }
  | { type: "sessions"; items: AgentSession[] }
  | ({ type: "session" } & AgentSession)
  | { type: "done"; ok: boolean };

export interface RuntimeErrorPayload {
  code: string;
  [key: string]: unknown;
}

export type Frame =
  | { kind: "request"; id: number; request: Request }
  | { kind: "response"; id: number; result: ResponseBody }
  | { kind: "error"; id: number; error: RuntimeErrorPayload; message: string }
  | { kind: "event"; event: Record<string, unknown> };

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
  env?: Record<string, string>;
  status: ServiceStatus;
  instance?: RuntimeInstance;
  actual_port?: number;
  url?: string;
  /** False when the runtime found it already running and cannot stop it. */
  managed?: boolean;
  /** Services in the same checkout that must be up first. */
  depends_on?: string[];
  /** Runs to completion rather than staying up. */
  one_shot?: boolean;
  /** What a graceful stop sends, when SIGTERM is the wrong word for it. */
  stop_signal?: "term" | "int" | "quit" | "hup";
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

export interface ProjectConfig {
  name?: string;
  services: Record<string, unknown>;
}

export interface PortOwner {
  port: number;
  /** One number can be held by both a TCP and a UDP socket. */
  protocol: "tcp" | "udp";
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
  /** Another supervisor keeping this alive: "pm2", "systemd". */
  supervisor?: string;
  /** Only a process the runtime started may ever be terminated automatically. */
  managed: boolean;
}

/** A launch the runtime was told about but did not perform. */
export interface LaunchObservation {
  id: string;
  /** Exactly as given, never a script name inferred from it. */
  command: string;
  cwd: string;
  source: string;
  session?: string;
  observed_at: string;
  state: "pending" | "bound";
  port?: number;
  pid?: number;
  service_id?: string;
}

/** A service another supervisor keeps, that the runtime can switch. */
export interface SupervisedView {
  name: string;
  supervisor: string;
  status: string;
  pid?: number;
  command: string;
  restarts: number;
  /** Absent, not empty, when it holds none. */
  ports?: number[];
  url?: string;
  /** Set when restarting this would fail, with the reason. */
  restart_warning?: string;
}

/** A service that is not working, with the part of its output that says why. */
export interface Failure {
  service_id: string;
  /** `Loom/api`, as a person would say it. */
  subject: string;
  status: string;
  at: string;
  exit_code?: number;
  /** Absent, not empty, when it said nothing. */
  detail?: string[];
}

/** Something wrong with what is declared, found without being asked. */
export interface Finding {
  /** Where it is, as a person would say it: `Loom/api`. */
  subject: string;
  message: string;
  /** True when it will fail rather than merely might. */
  certain: boolean;
}

/** A named sequence of members in a checkout. */
export interface Stack {
  id: string;
  workspace_id: string;
  name: string;
  members: string[];
}

/** A stack with what its members are actually doing. */
export interface FlowNode {
  name: string;
  service_id?: string;
  after?: string[];
  /** How many waits deep it is. Everything on one level starts at once. */
  level: number;
  status: ServiceStatus;
  one_shot?: boolean;
}

export interface StackView extends Stack {
  services: ServiceView[];
  running: number;
  /** Members naming a service that no longer exists. */
  missing?: string[];
  /** The stack as a graph, worked out by the daemon from the members' own
   *  dependencies so every surface reads one shape. */
  flow?: FlowNode[];
}

export interface AdoptOutcome {
  /** The stack it was put in, so it can be started afterwards. */
  stack?: string;
  service: ServiceView;
  /** Where the command came from. Never the project's scripts. */
  command_source: "recorded" | "process_argv" | "supervisor";
  /** False when the service was already declared and nothing changed. */
  declared: boolean;
  /** The command written down before, when adopting replaced it. */
  replaced_command?: string;
  supervisor?: string;
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
  /** Something about this start worth knowing first. */
  warning?: string;
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
  | ({ type: "config" } & ProjectConfig)
  | ({ type: "container" } & ContainerView)
  | ({ type: "service" } & ServiceView)
  | ({ type: "started" } & StartOutcome)
  | ({ type: "health" } & HealthReport)
  | ({ type: "port" } & PortStatus)
  | { type: "ports"; items: PortOwner[] }
  | ({ type: "reservation" } & PortReservation)
  | { type: "logs"; items: LogLine[] }
  | { type: "launches"; items: LaunchObservation[] }
  | ({ type: "supervised" } & SupervisedView)
  | { type: "findings"; items: Finding[] }
  | { type: "failures"; items: Failure[] }
  | { type: "stacks"; items: StackView[] }
  | { type: "stack_run"; done: string[] }
  | ({ type: "adopted" } & AdoptOutcome)
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

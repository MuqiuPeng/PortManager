# MCP server

`packages/runtime-mcp` exposes the runtime to coding agents over MCP. It is a
client of the daemon, exactly like the CLI and the desktop app — so a service an
agent starts is visible in the GUI immediately and outlives the agent's session.

## Install

```bash
cd packages/runtime-mcp
pnpm install && pnpm build
```

Then register it with your agent. For Claude Code:

```bash
claude mcp add local-runtime -- node /absolute/path/to/packages/runtime-mcp/dist/index.js --client claude-code
```

The `runtime` CLI must be on `PATH`: the server asks it for the daemon's
endpoint rather than reimplementing the data-directory rules in a second
language, and that call also starts the daemon if it is not running. Set
`LOCAL_RUNTIME_SOCKET` to bypass the CLI entirely.

## Agent ownership

The server registers a session with the daemon on start, so the runtime can
attribute every service to the agent that started it:

```
● api :8000
  feature/refund
  started by claude-code
```

The client name comes from `--client` (or `RUNTIME_MCP_CLIENT`). Without it the
server sniffs environment markers the known clients set — `CLAUDECODE`,
`CURSOR_TRACE_ID`, `CODEX_HOME` — and falls back to `unknown`. MCP has no
standard way for a server to identify its client, so pass `--client` explicitly
if the attribution matters to you.

## Tools

### Runtime

| Tool | Purpose |
|---|---|
| `list_projects` | Every registered project and how much of it is running |
| `discover_projects` | Find projects without being told where they are; optionally register them |
| `get_project_runtime` | One project's workspaces, services, ports and owners |
| `list_services` | Services with live status, optionally scoped to a project |
| `get_service` | One service's command, cwd, status, port and URL |
| `update_service` | Correct an inferred port, command, cwd or type |
| `export_config` | The project's services as a committable `.runtime.json` |

### Lifecycle

| Tool | Purpose |
|---|---|
| `start_service` | Start, reserving a port first; returns the running service if it is already up |
| `stop_service` | Stop the service and every process it spawned |
| `restart_service` | Stop, wait for the tree to exit, start again |

### Health

| Tool | Purpose |
|---|---|
| `get_health` | Probe now |
| `wait_until_healthy` | Block until it answers, or time out |

### Ports

| Tool | Purpose |
|---|---|
| `check_port` | Who owns a port, resolved to project/branch/service, and what to use instead |
| `list_ports` | Everything listening, including containers and processes the runtime did not start |
| `reserve_port` | Claim a port before starting something yourself |
| `release_port` | Drop a lease (does not stop anything) |

### Containers

| Tool | Purpose |
|---|---|
| `control_container` | Start, stop or restart a container by name |
| `get_container_logs` | A container's own output, from Docker |

### Logs and git

| Tool | Purpose |
|---|---|
| `get_logs` | Captured stdout/stderr, with a cursor for incremental reads |
| `list_worktrees` | A project's checkouts and their port offsets; registers new ones |
| `register_worktree` | Register a checkout and give it a stable port offset |

## What it deliberately does not expose

There is no `execute_shell`, no `kill_pid`, no `run_command`. The daemon's
protocol does not offer them, so this server cannot expose them even by
accident. An agent can `restart_service("api")`; it cannot ask the runtime to
run arbitrary code, and it cannot terminate a process the runtime did not start
— not even through `on_conflict: "kill-existing"`, which the daemon refuses for
unmanaged processes.

## Typical exchanges

**"What am I running?" (nothing registered yet)**

```
list_projects              -> No projects are registered.
discover_projects()        -> dossh (demo/payment-walkthrough) :3000 :5555
                              loom (multi-user-isolation) :3001 :8001
                              not registered
discover_projects(adopt: true)
```

**"Start this project's frontend and API."**

```
get_project_runtime(project: "dossh")
start_service(service: "web")     -> web started, port 3000
start_service(service: "api")     -> api started, port 8000
wait_until_healthy(service: "api")
```

**"Why is localhost:3000 unavailable?"**

```
check_port(port: 3000)
-> Port 3000 is in use.
     dossh/main/web
     pid 41288
     cwd /Users/dev/code/dossh
     started by cli
   Suggested alternative: 3003
```

**"I made a worktree for this branch — run it without clobbering main."**

```
register_worktree(project: "dossh", path: "/Users/dev/code/dossh-refund")
-> feature/refund  worktree  port offset +1
start_service(service: "feature/refund/web")
-> web started, port 3001
```

## Output shape

Tools return compact text, not JSON. `web :3004 healthy, started by claude-code,
pid 27722 [id 4179…]` costs a handful of tokens; the equivalent JSON costs an
order of magnitude more and says no more. Ids, ports and pids are still present
for follow-up calls that must be unambiguous.

Log reads default to 100 lines and are capped at 500, and every response ends
with a cursor so a follow-up call fetches only what is new.

## Testing

```bash
pnpm test          # pure-function tests, no daemon needed
```

For an end-to-end check, drive the server over stdio with a real daemon running
and a project registered: send `initialize`, then `tools/call` for
`start_service`, `wait_until_healthy`, `check_port` and `get_logs`, and confirm
the CLI (`runtime list`) shows the same state.

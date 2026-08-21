# MCP server

`packages/runtime-mcp` exposes the runtime to coding agents over MCP. It is a
client of the daemon, exactly like the CLI and the desktop app — so a service an
agent starts is visible in the GUI immediately and outlives the agent's session.

## Install

```bash
cd packages/runtime-mcp
pnpm install && pnpm build
runtime hook mcp        # registers it for every project, over stdio
```

`hook mcp` wraps `claude mcp add --scope user`. User scope because the runtime is
a property of the machine rather than of a checkout, and a tool that has to be
added again in each repository is one an agent will mostly not have.

Stdio rather than a local HTTP endpoint: a port manager that needs a port of its
own to be reachable has an obvious failure mode on the day it is most needed.

The `runtime` CLI must be on `PATH`: the server asks it for the daemon's
endpoint rather than reimplementing the data-directory rules in a second
language, and that call also starts the daemon if it is not running. Set
`LOCAL_RUNTIME_SOCKET` to bypass the CLI entirely.

Worth knowing when writing a launcher script by hand: the server is started by
whatever launches the agent, whose `PATH` has neither a node version manager nor
`~/.local/bin` on it. Both the interpreter and the CLI have to be findable, or
the server starts and then cannot reach anything.

### The other half

MCP gives an agent the operations. It does not stop the agent reaching for a
shell — an agent that decides to run `pnpm dev` directly is not doing anything
wrong, and no tool description prevents it. `runtime hook install` covers that
case by recording what gets started, without changing it. See the README.

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
| `update_service` | Correct how a service starts: command, environment, port, cwd, type, conflict policy |
| `add_service` | Declare a service detection did not find |
| `remove_service` | Forget a service definition (stops nothing) |
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

### Other supervisors

* `control_supervised` — `start`, `stop` or `restart` a service PM2 or systemd
  keeps. Use this rather than `start_service`/`stop_service` when a service
  reports a supervisor: the runtime did not start it, and a stop issued any
  other way is undone the moment that supervisor notices. There is no delete.

### Debugging

* `recent_errors` — every service that is failing or unhealthy, newest first,
  each with the last thing it said. The starting point when something is wrong
  and the service is not known yet; `get_logs` afterwards for one of them in
  full. Preferring stderr matters: a service that is serving traffic and failing
  at something else will otherwise show a page of access log.

### Checking

* `diagnose` — everything wrong with the declared services that has not caused
  a failure yet. Worth calling before starting things in an unfamiliar project:
  each of these is quiet until the moment it is expensive, and several of them
  fail somewhere other than where the cause is.

### Taking things over

* `adopt_port` — declare whatever is already listening, so it can be started
  again later. Never guessed from `package.json`: the supervisor is asked first,
  then a recorded launch, then the process — and a process that reports a name
  rather than a command is declined rather than written down. Mode-selecting
  variables come with it, since `NODE_ENV` is the whole difference between a
  project's two servers. Refuses when a supervisor holds it, unless forced.
* `list_launches` — what an agent or a terminal started, with the command
  exactly as given, and the port and pid it turned into.

### Ordering

* `list_tasks`, `set_task` — named step sequences over a project's services.
* `run_task` — bring up every step in order, waiting for each to report healthy.
  A step that runs to completion must succeed or the task stops there.

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

**"It won't start."**

```
start_service(service: "billing-scheduler")
-> 'billing-scheduler' exited immediately (code 1): Error: [db] DATABASE_URL is not set

update_service(service: "billing-scheduler",
               env: { DATABASE_URL: "postgres://localhost:5432/app" })
start_service(service: "billing-scheduler")
```

Variables passed this way are merged with the ones already set, and win over any
`.env` file. `update_service` reports the *names* it holds, never the values —
confirming a variable was set does not require putting a credential in the
transcript.

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

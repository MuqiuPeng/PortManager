# Local Runtime

> One place to see and control everything running on localhost.

A local development runtime manager for developers **and** coding agents. It
answers "who owns `:3000`?" with a project, a branch and a service — not a pid —
and lets a human or an agent start, stop, restart and inspect those services
through the same runtime.

Port management is the substrate. The product is the shared runtime state.

See [`local-dev-runtime-manager-plan.md`](local-dev-runtime-manager-plan.md) for
the full product plan.

## Status

| Phase | Scope | State |
|---|---|---|
| 0 | Cross-platform process/port PoC | **done** |
| 1 | Daemon, IPC, SQLite, registry, lifecycle, CLI | **done** |
| 2 | Tauri desktop MVP | **done** |
| 3 | MCP server | **done** |
| 4 | Native edge sidebar | **done** (macOS) |
| 5 | Project intelligence | partly done (detection, worktrees, stable ports) |
| 6 | Agent-aware runtime | partly done (`started_by`, sessions, kill safety) |

macOS is implemented natively. Windows has a working baseline adapter with the
native work marked — see [docs/windows.md](docs/windows.md). Linux runs on the
portable adapter.

## Quick start

```bash
cargo build --release
```

The binaries land in `target/release`:

- `runtime` — the CLI
- `runtime-daemon` — the daemon (every client starts it on demand)
- `runtime-desktop` — the desktop app

```bash
# check the platform adapter can see this machine
runtime doctor

# find your projects — no configuration, no paths to type
runtime scan --add

# start it and wait until it answers
runtime start web --wait

# who has :3000?
runtime port check 3000

# everything, everywhere
runtime list
runtime port list
```

## Desktop app

```bash
cd apps/desktop
pnpm install
pnpm tauri dev          # or: pnpm tauri build
```

`tauri build` bundles the daemon inside the `.app`, so the result is
self-contained: an app launched from Finder inherits a minimal `PATH` and would
otherwise have no way to find it. A plain `cargo build` does not need this — the
daemon sits beside the executable in `target/` — but if you assemble a bundle by
hand, stage the sidecar first with `pnpm --dir apps/desktop prepare-sidecar`.

Always launch it through the Tauri CLI. `cargo build` produces a **dev** binary
whatever the profile — `tauri-build` takes that from the CLI's environment, not
from cargo — and a dev binary loads its frontend from the Vite server rather
than from itself, so running `target/debug/runtime-desktop` directly opens a
blank window. It prints an explanation to stderr when that happens.

On first run the window scans for projects instead of showing an empty list and
asking you to register them. Beyond that it shows projects in a sidebar and, per
workspace, the services with their live status, port and owner. Selecting a service streams its output; the
Ports tab lists everything listening on the machine, registered or not.

It holds no state of its own — closing it stops nothing, and a service started
from the CLI or by an agent appears in the window immediately through the
daemon's event stream.

### The panel

An edge-docked panel gives the common case — glance at what is running, start or
stop one thing — without switching windows. Four ways in, one state machine:

At rest it is a slim tab against the screen edge showing one dot per running
service. It is click-through, so it never swallows a click meant for the window
underneath.

| | |
|---|---|
| Pointer reaches the tab | Expands **without taking focus** from your editor |
| `⌘⌥L` | Expands it **focused**, so the keyboard works |
| Menu bar icon | Same, on left click |
| Pin | Keeps it expanded, no auto-collapse |

Edge, size, animation and the shortcut are all editable under **Settings** in the
main window. They are stored by the daemon, so they survive reinstalling the app.

The app is a menu-bar accessory while the main window is closed — no Dock icon,
no place in ⌘-Tab — and a normal foreground app while it is open, because macOS
withholds full-screen support from accessory apps. Closing the main window
hides it and drops the Dock icon; the panel and the tray stay.

Clicking the panel must never steal focus, which on macOS means a real
`NSPanel` rather than the `NSWindow` Tauri creates — see
[docs/architecture.md](docs/architecture.md#the-edge-panel). Windows is not
implemented yet; [docs/windows.md](docs/windows.md) has the plan.

## Coding agents

Two halves, and they do different jobs.

```bash
cd packages/runtime-mcp && pnpm install && pnpm build
runtime hook mcp        # register the MCP server for every project
runtime hook install    # record what gets started outside it
```

The **MCP server** gives an agent the operations: "start this project's frontend
and API and wait until they are healthy", "why is localhost:3000 unavailable?",
"run the dev task". There is no `execute_shell`, no `kill_pid` and no
`run_command` — the daemon's protocol does not offer them, so the server cannot
expose them. See [docs/mcp.md](docs/mcp.md).

The **hook** covers what the MCP server cannot: an agent that runs `pnpm dev` in
a terminal anyway. A `PreToolUse` hook records the command before it runs and
changes nothing about it — the command Claude sees approved is the command that
runs. What that buys is the one fact a running process cannot be asked for
afterwards: how to start it again. Inferring that from a project's scripts is
how a checkout ends up with its production build overwritten by a development
one.

Every failure path in the hook exits 0 in silence. A runtime that is down must
not be able to wedge a shell command.

The runtime records which agent started what, so the desktop app shows
`● api :8000  feature/refund  started by claude-code`.

## Commands

```
runtime list                        every project, workspace and service
runtime scan [--path DIR] [--add]   find projects automatically
runtime project add|list|show|remove
runtime service list|show|set|add|remove
runtime export [project] [--write]  services as a committable .runtime.json
runtime start <service> [--port N] [--on-conflict P] [--wait]
runtime stop <service> [--timeout S]
runtime restart <service> [--wait]
runtime logs <service> [-n N] [--follow]   captured output, kept across restarts
runtime health <service> [--wait S]
runtime port list|check|reserve|release
runtime adopt <port> [--force]      declare what is already on a port
runtime supervised start|stop|restart <name>   drive PM2, without taking it over
runtime task list|set|remove|run    named step sequences
runtime container start|stop|restart|logs
runtime worktree list|add
runtime hook install|uninstall|status|mcp|log
runtime daemon start|stop|status
runtime doctor
```

`adopt` writes down what is running so the runtime can start it again, never
guessing the command from `package.json`. It asks the supervisor first, then a
recorded launch, and only then the process itself — and declines when the
process reports something it cannot execute, which is what a self-renaming one
does: Next calls itself `next-server (v14.2.35)`, an accurate description and
not a command. It also captures the handful of variables that select a *mode*,
since `node server.mjs` and `NODE_ENV=production node server.mjs` are the same
process listing and not the same server.

`supervised` is the other half: PM2 still owns what the service is and whether
it starts at boot; this owns whether it is running now. There is no `delete`.

`doctor` reports what is wrong with the registry before it costs anything — a
dependency naming a service that does not exist, services that depend on each
other, a command that will not resolve from the daemon, a build directory two
services would overwrite for each other. The same list is in the app, above the
projects, and available to an agent as `diagnose`.

A service is named `web`, or `<branch>/<name>` to reach a git worktree:

```bash
runtime start feature/refund/web
```

Worktrees carry the project's services on their own port range, so two branches
can be served at once. `worktree add` also tops up a checkout that was
registered before a service existed, and leaves any copy you have edited alone.
A task is declared once for the project and runs in whichever checkout you name
— which is the point: one definition, two branches, different ports.

Every command takes `--json` for scripting.

## Configuration

Inference covers the common cases. To declare services explicitly, commit a
`.runtime.json` at the project root — it takes precedence over anything
inferred:

```json
{
  "name": "dossh",
  "services": {
    "migrate": { "command": "pnpm db:migrate", "one_shot": true },
    "web": { "command": "pnpm dev", "port": 3000, "depends_on": ["api"] },
    "api": {
      "command": "pnpm api:dev",
      "port": 8000,
      "type": "api",
      "depends_on": ["migrate"],
      "health": { "kind": "http", "path": "/health", "expect_status": [200] }
    }
  }
}
```

`depends_on` names services in the same checkout. Starting one brings them up in
order and waits for each to report healthy — and leaves alone any that is
already running, whoever started it.

`one_shot` marks a step that runs to completion rather than staying up. It is
not a service that exits quickly: a server that exits has failed and a migration
that keeps running has hung, so the two cannot share one test for success. A
one-shot that fails stops what would have followed it.

Omitting `health` is not the weakest option. A service declared as `web` or
`api` is asked for an HTTP response, since a TCP connect cannot tell a working
server from one that holds the port and answers nothing. Any response counts —
302, 404 and 200 all mean it is alive.

See [config/runtime.example.json](config/runtime.example.json) for every field.

`.env` and `.env.local` are loaded from the project root and the service's own
directory before it starts — the same convention Compose, Next and Vite follow —
with the service's own declared variables taking precedence. Which files were
read is written to the service's log, so a service never behaves differently
because of a file nobody mentioned.

Inference is a starting point, not a verdict. Correct it in place and write the
result out:

```bash
runtime service set web --port 3007 --command "pnpm run dev:local"
runtime service set web --env DATABASE_URL=postgres://localhost:5432/app
runtime export --write            # writes .runtime.json at the project root
```

The same edits are available from the desktop app (**Edit** on any service) and
from MCP, so an agent that reads "DATABASE_URL is not set" can set it.

Correcting the port is also what lets the runtime recognise an already-running
service *as* that service, rather than listing it as an unexplained port.

## Design notes

Three properties are worth knowing before reading the code:

**Projects are found, not declared.** Every listening socket resolves to a pid,
a working directory and from there a repository root, so the runtime can list
the projects on a machine without being told where any of them are — and
without false positives, since every one it reports is running something right
now. `--path` adds a directory walk for projects that happen to be stopped.

**Containers are part of the same picture.** Every container publishes through
one Docker process, so a port table built on pids shows five services as five
identical `com.docker.docker` rows. Compose labels resolve them to their project
and service, and the compose file's directory makes them discoverable like any
other project. Compose still owns *what* they are; the runtime owns whether they run —
`runtime container stop stockviewer-db`, or a button in the panel.

**Running is running, whoever started it.** A service found already listening on
its port is reported as running and marked unmanaged — claiming otherwise while
the port table shows it up is the contradiction this tool exists to remove. Live
ports that no declared service explains are listed as external rather than
guessed at.

That has to hold for every operation, not just the display. An operation that
reads the runtime's own instance record alone cannot see a service somebody else
started, which on a working machine is most of them — and the failures are not
symmetrical. `stop` and `health` merely said "not running" about something
visibly running. `start` launched a second copy beside it, which for a project
whose dev and production servers share a build directory is how the running one
gets broken.

**Something else may already be in charge.** PM2 and systemd start services too,
and a stop issued anywhere else is undone the moment they notice. Where one is
found, the runtime says so and can drive it — `start`, `stop`, `restart`, the
supervisor's own reversible verbs. It will not `delete`: removing an entry is
usually also what stops it starting at boot, which is a decision about the
machine rather than about this registry.

**Ask to be told, do not infer.** The command that would restart a service
cannot be recovered from the running process, and guessing it from
`package.json` is how a checkout is left unable to boot. So the Claude Code hook
records commands before they run — and records the environment too, since `node
server.mjs` is the development server or the production one depending on
`NODE_ENV` alone.

**A port is a lease, not an observation.** A service claims its port before the
process starts, so a conflict is reported as an answer rather than discovered as
a failed boot. Worktrees get a stable offset from the primary checkout, so
`main` keeps 3000 while `feature/refund` reliably takes 3001.

**Nothing is terminated by pid alone.** Every kill path carries a
`(pid, process_start_time)` identity that is re-verified immediately before
signalling, and a process the runtime did not start is never terminated
automatically — not even under `--on-conflict kill-existing`.

**The daemon is the only authority.** The CLI, the desktop app and any number of
agents are clients. Closing one does not change what is running, and on start
the daemon reconciles its database against the live process table rather than
trusting either alone.

## Repository layout

```
crates/
  runtime-types/     domain model, config, errors — no I/O
  runtime-adapter/   OS traits + a portable sysinfo/netstat2 implementation
  adapter-macos/     libproc, sysctl, process groups
  adapter-windows/   baseline + native Win32 work (see docs/windows.md)
  runtime-core/      registry, ports, lifecycle, logs, health, reconciliation
  runtime-ipc/       protocol and transport (Unix socket / named pipe)
  runtime-daemon/    the daemon binary
  runtime-cli/       the `runtime` binary
docs/
apps/desktop/        Tauri 2 + React desktop app
packages/runtime-mcp/  MCP server for coding agents
```

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
pnpm --dir apps/desktop build          # type-check, test and bundle the frontend
pnpm --dir packages/runtime-mcp test   # MCP server
# type-check the whole stack for Windows, from anywhere
cargo check --workspace --exclude runtime-desktop --all-targets \
  --no-default-features --target x86_64-pc-windows-msvc
```

The frontend tests render every row against fixtures written by the Rust types
themselves — including the shape where every optional field is absent, which is
what `skip_serializing_if` produces and what once took the window down: a
component read `.length` off a field the daemon had omitted, React unmounted the
tree, and the result was a blank window with nothing in the console. Regenerate
the fixtures with `pnpm --dir apps/desktop fixtures` after changing a view type.

`--no-default-features` turns off `bundled-sqlite`, which compiles SQLite from
source and needs a C toolchain for the target. Everything else still type-checks,
including tests — which is where the platform assumptions hide. Nothing here is
built for Windows, so it catches what will not compile, not what will not work.

Point the runtime at a scratch directory to avoid touching real state:

```bash
export LOCAL_RUNTIME_DATA_DIR=/tmp/runtime-scratch
```

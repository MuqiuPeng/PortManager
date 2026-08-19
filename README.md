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
| 4 | Native edge sidebar | not started |
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

The edge-docked side panel is Phase 4 and is deliberately not built yet.

## Coding agents

```bash
cd packages/runtime-mcp
pnpm install && pnpm build
claude mcp add local-runtime -- node "$PWD/dist/index.js" --client claude-code
```

An agent can then answer "start this project's frontend and API and wait until
they are healthy" or "why is localhost:3000 unavailable?" without a shell — and
the runtime records which agent started what, so the desktop app shows
`● api :8000  feature/refund  started by claude-code`.

There is no `execute_shell`, no `kill_pid` and no `run_command`: the daemon's
protocol does not offer them, so the MCP server cannot expose them. See
[docs/mcp.md](docs/mcp.md) for the tool list.

## Commands

```
runtime list                        every project, workspace and service
runtime scan [--path DIR] [--add]   find projects automatically
runtime project add|list|show|remove
runtime service list|show
runtime start <service> [--port N] [--on-conflict P] [--wait]
runtime stop <service> [--timeout S]
runtime restart <service> [--wait]
runtime logs <service> [-n N] [--follow]
runtime health <service> [--wait S]
runtime port list|check|reserve|release
runtime worktree list|add
runtime daemon start|stop|status
runtime doctor
```

A service is named `web`, or `<branch>/<name>` to reach a git worktree:

```bash
runtime start feature/refund/web
```

Every command takes `--json` for scripting.

## Configuration

Inference covers the common cases. To declare services explicitly, commit a
`.runtime.json` at the project root — it takes precedence over anything
inferred:

```json
{
  "name": "dossh",
  "services": {
    "web": { "command": "pnpm dev", "port": 3000 },
    "api": {
      "command": "pnpm api:dev",
      "port": 8000,
      "type": "api",
      "health": { "kind": "http", "path": "/health", "expect_status": [200] }
    }
  }
}
```

See [config/runtime.example.json](config/runtime.example.json) for every field.

## Design notes

Three properties are worth knowing before reading the code:

**Projects are found, not declared.** Every listening socket resolves to a pid,
a working directory and from there a repository root, so the runtime can list
the projects on a machine without being told where any of them are — and
without false positives, since every one it reports is running something right
now. `--path` adds a directory walk for projects that happen to be stopped.

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
pnpm --dir apps/desktop build          # type-check and bundle the frontend
pnpm --dir packages/runtime-mcp test   # MCP server
# type-check the Windows adapter without a Windows machine
# (the whole workspace cannot cross-build: bundled SQLite needs a C toolchain)
cargo check -p adapter-windows --target x86_64-pc-windows-msvc
```

Point the runtime at a scratch directory to avoid touching real state:

```bash
export LOCAL_RUNTIME_DATA_DIR=/tmp/runtime-scratch
```

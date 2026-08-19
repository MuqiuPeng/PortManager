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
| 2 | Tauri desktop MVP | not started |
| 3 | MCP server | not started |
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

The two binaries land in `target/release`:

- `runtime` — the CLI
- `runtime-daemon` — the daemon (the CLI starts it on demand)

```bash
# check the platform adapter can see this machine
runtime doctor

# register a project; services are inferred from package.json, pyproject.toml, …
runtime project add ~/code/dossh

# start it and wait until it answers
runtime start web --wait

# who has :3000?
runtime port check 3000

# everything, everywhere
runtime list
runtime port list
```

## Commands

```
runtime list                        every project, workspace and service
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
apps/                (Phase 2: Tauri desktop)
packages/            (Phase 3: MCP server)
```

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
# type-check the Windows adapter without a Windows machine
# (the whole workspace cannot cross-build: bundled SQLite needs a C toolchain)
cargo check -p adapter-windows --target x86_64-pc-windows-msvc
```

Point the runtime at a scratch directory to avoid touching real state:

```bash
export LOCAL_RUNTIME_DATA_DIR=/tmp/runtime-scratch
```

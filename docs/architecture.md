# Architecture

```
           Desktop              CLI                  MCP
                │                 │                    │
                └────────┬────────┴──────────┬─────────┘
                         │  newline-JSON over UDS / named pipe
                         ▼
                  runtime-daemon          ← the only authority for state
                         │
                  runtime-core            ← registry, ports, lifecycle, health
                         │
                  runtime-adapter         ← traits only
                    ┌────┴────┐
             adapter-macos   adapter-windows
```

## Crates

| Crate | Responsibility | Depends on an OS? |
|---|---|---|
| `runtime-types` | Domain model, `.runtime.json`, wire errors | no |
| `runtime-adapter` | `ProcessProvider`, `PortProvider`, `SpawnProvider` + a portable implementation | no |
| `adapter-macos` | `libproc`, `sysctl`, process groups | macOS |
| `adapter-windows` | Process groups, tree termination | Windows |
| `runtime-core` | Everything the product does that is not a UI | via traits |
| `runtime-ipc` | Protocol + transport, client and server | no |
| `runtime-daemon` | The daemon binary | no |
| `runtime-cli` | The `runtime` binary | no |

`runtime-core` selects an adapter in exactly one place —
`runtime-core/src/platform.rs`. Adding Linux means adding a crate and one arm
there.

## Data model

```
Project            one repository, as the user thinks of it
 └── Workspace     one checkout (primary or git worktree), owns a port offset
      └── Service  a declared runnable unit
           └── RuntimeInstance   one start of it: pid + start time + status
```

Ports are not part of that tree. A `PortLease` points at a service and moves
through `reserved -> active -> released`, with reservations expiring so a
crashed agent cannot hold a port forever.

## Three properties the design rests on

### A port is leased before the process starts

`PortResolver::reserve` runs before spawn. A conflict is therefore an *answer*
("3000 belongs to dossh/main/web, use 3003") rather than a failed boot to
diagnose afterwards. The policies are `reuse`, `allocate-next`, `fail`, `ask`
and `kill-existing`, defaulting to `allocate-next`.

Worktree ports are stable rather than merely free: each workspace holds a
`port_offset` assigned once and never reused, so `main` keeps 3000 and
`feature/refund` takes 3001 across every restart and every machine.

### Process identity, never a bare pid

Every termination path takes `ProcessIdentity { pid, process_start_time }` and
re-verifies it immediately before signalling. Start times are compared with a
1.5s tolerance because platforms report them at different resolutions.

A process the runtime did not start is never terminated automatically. On macOS
services are spawned into their own process group so the whole tree — `pnpm` ->
`node` -> whatever it forked — is signalled as a unit; the fallback walks the
tree explicitly.

### The OS is the authority for what is running

The database holds *declarations*: projects, services, leases. Whether something
is running is answered by the process table. On start the daemon reconciles the
two, closing out instances whose process is gone and releasing their leases.
Without that step the state drifts a little further from reality after every
crash.

## Port -> project resolution

```
port -> socket table -> pid -> ancestors -> managed instance?  -> service
                            \-> cwd -> longest matching workspace -> project
```

The ancestor walk matters: the runtime launches `sh -c "pnpm dev"`, and the
process that binds the socket is usually a grandchild of that shell. Matching
only the recorded pid would report every managed service as unregistered.

The cwd path is what identifies services the runtime did *not* start — anything
launched from a terminal inside a registered repository still resolves to its
project.

## What the protocol deliberately does not expose

There is no `exec`, no `kill_pid`, no `run_command`. A caller can
`restart_service("api")`; it cannot ask the daemon to run arbitrary code. That
boundary is what makes it safe to put an MCP server in front of the daemon and
hand it to an agent.

## IPC

Newline-delimited JSON over a Unix domain socket (macOS, Linux) or a named pipe
(Windows). Both are authenticated by the OS, so there is no token to store and
nothing bound to a TCP port that another machine could reach.

Unix socket paths are limited to ~104 bytes; when the data directory is deep the
runtime falls back to a short name under the system temp directory, hashed with
FNV-1a. The algorithm is specified rather than convenient (`DefaultHasher`'s is
unspecified and changes between Rust versions) so a non-Rust client can compute
the same name; `crates/runtime-core/src/paths.rs` carries reference vectors.

In practice clients do not need to: the MCP server asks `runtime daemon start
--json` for the endpoint, which keeps one implementation of the path rules.

Error frames carry both the structured error and its rendered message, so a
client in another language does not have to reimplement the wording of every
variant to show something usable.

Frames are explicitly tagged (`kind`) rather than untagged — an ambiguous frame
that silently deserialises as the wrong variant is far worse to debug than a
slightly more verbose envelope. Collections travel in a named `items` field
because serde cannot put an internal tag on a sequence.

## Desktop app

`apps/desktop` is a Tauri 2 shell around a React frontend. Its Rust side holds a
`DaemonHandle` — a self-repairing IPC connection — and every `#[tauri::command]`
is a one-to-one translation of an IPC request. No state and no logic live in the
app: anything computed there would be something the CLI and MCP do not get.

A second connection carries the event subscription, so a long-lived stream never
interleaves with a command the user just clicked. Events are re-emitted to the
frontend on the `runtime://event` channel, which is what makes a service started
from a terminal appear in the window without polling.

Two things still poll, because the daemon has no event for them: the ports table
(the socket table changes without the runtime's involvement) and log tailing,
which uses the `since_seq` cursor so each tick transfers only new lines.

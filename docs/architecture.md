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

## Containers

Every containerised service publishes its ports through one Docker process, so
`port -> pid -> cwd -> project` dead-ends at a single pid whose working
directory is Docker's own: five services become five identical rows reading
`com.docker.docker`. Docker is not a gap in coverage, it is a structural blind
spot in the mechanism the whole product rests on.

Compose labels supply the missing link. `com.docker.compose.project.working_dir`
is the directory the compose file lives in, which is a project root by the same
definition applied to processes — so containers join both port attribution and
discovery through the paths that already exist.

This is read-only on purpose. Compose starts and stops these services well, and
its file is a contract shared with CI and teammates; the value on offer is
putting containers and native processes in one picture, not becoming a second
orchestrator. `RuntimeProvider` remains the seam if lifecycle is added later.

Containers started with plain `docker run` carry no labels, and are reported by
name and image without a project. `loom-postgres` obviously belongs to Loom, and
guessing that is exactly the false positive discovery is built to avoid.

The listing is cached for three seconds so enumerating every port costs one
`docker` invocation rather than dozens, and an absent Docker is cached for a
minute rather than paid for on each lookup. The CLI is located the same way the
daemon is — a daemon started by the bundled app inherits a minimal PATH that
contains no `docker`.

## Discovery

The same chain that answers "who owns :3000" answers "what projects are on this
machine":

```
listening socket -> pid -> cwd -> git root (or nearest manifest) -> project
```

Nothing is configured and nothing is guessed: every project reported this way
is demonstrably running something. A directory walk (bounded to three levels,
skipping `node_modules`, `target`, `.venv` and friends) covers projects that are
stopped, and is opt-in because it touches directories the user did not name.

Two filters do the real work. System prefixes (`/System`, `/Library`,
`/usr`, `%ProgramFiles%`) and sandbox fragments (`/Library/Containers/`) are
never projects — without them a chat app's container directory looks exactly
like one, because it has a working directory and it is listening on a port. And
a candidate directory is never descended into, so a vendored dependency with its
own `package.json` is not reported as a separate project.

Registration is idempotent and records only what is already there; it starts and
stops nothing.

## Declared, adopted, and observed

A service the runtime started is one of three things it can report, and
conflating them is how a project view ends up saying `0/3 running` while the
port table shows three of its ports live.

* **Managed** — the runtime started it, holds its process identity, and can
  stop or restart it.
* **Adopted** — the port the service declares is listening, held from inside
  that service's own checkout. Reported as running, marked unmanaged: real, but
  not ours to stop.
* **External** — a live port in the checkout that no declared service explains.
  Listed separately rather than pinned to whichever service looks closest,
  because that would be a guess, and a service shown as running when something
  else holds its port is worse than an honest gap.

Adoption is deliberately strict: the declared port must match *and* be held
from inside the workspace. Either half alone attributes unrelated processes to
a service. In practice most already-running services land in the third bucket,
because an inferred default port (Next.js 3000) rarely matches what is actually
running (3007) — which is the honest answer, not a shortcoming to paper over.

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

## Windows and activation

The activation policy is not fixed: it follows the main window. `Regular` while
the window is on screen, `Accessory` — no Dock icon, no ⌘-Tab entry — once it is
closed.

Pinning it to `Accessory` seems right for a menu-bar app but is wrong in a way
that is hard to attribute: macOS withholds full-screen support from accessory
apps, so the window launched at startup had no full-screen button while the
same window reopened from the tray did, because reopening switched to `Regular`
on the way. Two windows that were never actually two windows.

Closing the main window **hides** it rather than destroying it. Tauri's default
is to destroy, after which `get_webview_window("main")` returns `None` and the
tray's "Open main window" silently does nothing — the window is gone for the
rest of the session.

Reopening switches the activation policy to `Regular` **before** showing and
focusing. An accessory application cannot bring a window to the front, so
focusing first leaves it behind whatever the user was in. Hiding switches back,
so the Dock icon does not outlive the window that justified it.

## The edge panel

The panel's defining property is that clicking it does not take focus from the
editor. On macOS that is `NSWindowStyleMask::NonactivatingPanel`, which the
window server honours only for `NSPanel` — and Tauri creates an `NSWindow`. So
`adapter-macos` swaps the class at runtime, then verifies the style mask took;
a silently failed adoption looks fine at startup and only surfaces later as a
panel that steals focus on every click.

It also joins all Spaces and sits at `NSStatusWindowLevel`, so it does not
vanish when the user switches Space or opens a full-screen app.

The panel is never absent. At rest it is a slim tab against the screen edge, so
expanding is a *resize* rather than an appearance — which makes it discoverable
(an invisible hover strip is something you have to be told about) and gives the
expansion something to animate, since there is already a window on screen.

```
island ──pointer reaches the tab──▶ expanded (passive: keeps the editor's focus)
  ▲                                    │
  └────pointer leaves the panel────────┘
island ──shortcut / menu bar──▶ expanded (focused: keyboard works)
pinned ────────────────────────▶ expanded always
```

The tab is click-through while resting (`setIgnoresMouseEvents`), so a permanent
strip at the screen edge never swallows a click meant for the window underneath.
That costs nothing, because proximity is found by polling the pointer rather
than by receiving events.

The window itself is transparent and the panel draws its own rounded background;
without that the window paints an opaque rectangle and the rounded corners show
as white squares. On macOS that needs Tauri's `macos-private-api` feature, which
rules out the Mac App Store — not a distribution channel for a tool that manages
local processes anyway.

The distinction between passive and focused is the reason the panel exists in
this form: a pointer reveal must not disturb what you are typing into, while a
deliberate keystroke should, or you cannot type into what you just summoned.
A panel revealed by hovering is also not "already open" as far as the shortcut
is concerned — pressing it focuses rather than dismisses, or the key appears to
do nothing.

Edge detection polls the pointer every 90ms on the main thread. The alternative,
a global `CGEventTap`, would demand Accessibility permission for something this
small; polling asks for nothing and is imperceptible.

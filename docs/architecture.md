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

Containers can also be switched on and off, which does not contradict the rule
that the runtime never terminates a process it did not start. That rule exists
because signalling an arbitrary pid is dangerous and pids are recycled;
`docker stop` is neither — it is a graceful operation on a named, restartable
object, and exactly what the developer would otherwise type.

What this deliberately does not do is replace compose. Building images,
dependency ordering, networks and volumes stay where they are: compose owns
*what* these services are, the runtime owns whether they run.

Stopped containers are listed too — a switch that only turns things off is half
a switch — but one directory can hold several stacks, and a dormant `-prod`
stack's dead containers would bury the one in use. A stopped container is shown
when its own compose project has something running, and when nothing at all is
running they all appear, because otherwise there would be nothing to switch on.

Reading Docker drains the command's output on a separate thread while waiting.
Polling for exit without reading deadlocks once the output passes the 64KB pipe
buffer, which `docker inspect` does at a handful of containers — and the symptom
is not a hang but silence, because the command is killed at the deadline and
every container quietly disappears.

Containers started with plain `docker run` carry no labels, and are reported by
name and image without a project. `loom-postgres` obviously belongs to Loom, and
guessing that is exactly the false positive discovery is built to avoid.

The listing is cached for three seconds so enumerating every port costs one
`docker` invocation rather than dozens, and an absent Docker is cached for a
minute rather than paid for on each lookup. The CLI is located the same way the
daemon is — a daemon started by the bundled app inherits a minimal PATH that
contains no `docker`.

## Environment

`.env` and `.env.local` are read from the workspace root and then the service's
own directory, so a package in a monorepo can override the repository. The
service's declared variables are applied on top: an explicit value in the
registry is a correction, and a correction has to win.

Compose, Next and Vite all do this, and a developer running the command by hand
usually has direnv or a `source` in the way — so spawning it without them starts
a *different* process than the one they would have started, and the difference
surfaces as a missing variable deep inside the service.

Which files were read is written to the service's log. Loading environment
silently would make behaviour depend on a file nobody mentioned.

Parsing is deliberately plain `KEY=VALUE` with no interpolation. A file meaning
something subtler is doing more than this should guess at, and the service can
be given explicit variables instead.

## Starting

Spawning is not starting. A process can be created and be dead a moment later —
a missing command, a port already taken, a syntax error — and reporting success
the instant a pid exists leaves the truth only in the logs. `start_service`
therefore watches the process for a short grace period and fails with the last
thing it printed if it is already gone.

That last output has to survive, which it did not: the exit watcher used to drop
the whole supervisor entry, aborting the log pumps along with it. The faster a
service died, the more likely its explanation was thrown away. Ending a service
now stops tracking it without touching the pumps, which finish by themselves
when the pipes close.

A service that stays alive but never binds the port it was reserved is the other
common shape — a service that does not read `$PORT` and hardcodes its own. The
health detail says so instead of reporting a bare connection refused, because
"nothing is listening on 3005 although the process is running" is the fact that
leads somewhere.

## Logs

Services write into capture files, not into a pipe.

A pipe has a read end, and that read end belongs to the daemon. When the daemon
dies the pipe breaks, and the next thing a service prints kills it with
`SIGPIPE` — so capturing output quietly made every service's life depend on the
daemon's, which is the one thing the daemon must not be for. It went unnoticed
because the first services tested were Node servers, and Node ignores `SIGPIPE`;
a shell loop dies immediately.

The daemon tails those files (`<service id>.out` and `.err`) from wherever they
already end, turning new lines into entries. Output written while the daemon was
down stays in the capture file but is not ingested — the daemon resumes at the
end rather than replaying.

A bounded ring buffer per service answers reads; a file beside it is what makes
the answer survive a daemon restart. That matters because "why did it die?" is
asked *after* the thing died — often after the daemon was restarted too, which
is precisely when memory-only logs are gone.

Lines are JSON, one per line, under `<data dir>/logs/<service id>.log`. JSON
because the stream and the sequence number have to survive; one line each
because a torn write during a crash then costs one line rather than the file.
On restore the sequence continues where it left off, so a cursor held across a
restart returns what is new rather than replaying what the caller already has.

Files rotate at 4 MB keeping one generation, and files for deleted services are
pruned when the daemon starts.

A service the runtime did not start has no captured output at all. Asking for
its logs says so, rather than returning nothing — "(no output)" reads as "it
printed nothing", which is a different and misleading claim.

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

In a monorepo the packages are the runnable units, and the root manifest is not
enough. Often it holds nothing but `build` and `lint` while every dev server
lives in `packages/*`. Even when the root does forward — `api:dev` running
`pnpm --filter @acme/payments dev` — the service that produces is named after
the script and rooted at the repository, so its working directory never matches
the process that actually runs and it can never be recognised as already
running. Workspace globs are read from `pnpm-workspace.yaml` or the `workspaces`
field, and a root script whose body names a member (or runs them all through
turbo, nx or lerna) is dropped in favour of the member itself.

Two filters do the real work. System prefixes (`/System`, `/Library`,
`/usr`, `%ProgramFiles%`) and sandbox fragments (`/Library/Containers/`) are
never projects — without them a chat app's container directory looks exactly
like one, because it has a working directory and it is listening on a port. And
a candidate directory is never descended into, so a vendored dependency with its
own `package.json` is not reported as a separate project.

Registration is idempotent and records only what is already there; it starts and
stops nothing. Detection runs when a project is first added and never again:
re-adding happens easily — the Discover tab, a scan, `project add` run twice —
and must not undo curation. A service the user deleted coming back, or a
corrected command being overwritten by the guess it replaced, is the shape of a
tool that does not believe them.

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
* **Supervised** — something else starts and restarts it: PM2, systemd. Not a
  fourth kind of ownership so much as an answer to "why can I not stop this?",
  and one the runtime can act on — see [Other supervisors](#other-supervisors).

Every operation has to read all of these, not just the runtime's own instance
record. That is one bug, found four times: `stop` reported "not running" for a
service the view showed serving; `health` said the same; `start` launched a
second copy beside the one already up; `restart` skipped the stop and did the
same. On a machine where most services were started elsewhere, an operation
that consults only the instance table is wrong about most of the machine.

Which is why services are editable. Detection guesses a framework's default
port, and correcting it is usually all that stands between "stopped, plus an
unexplained port" and "running". `export_config` then turns a corrected registry
into a `.runtime.json` the repository can carry.

A patch distinguishes an absent field from an explicit `null`: serde folds both
into `None` by default, which silently turns "clear this port" into "leave it
alone" — an option that appears to work and does nothing.

Adoption is deliberately strict: the declared port must match *and* be held
from inside the workspace. Either half alone attributes unrelated processes to
a service.

It is also refused when two services declare the same port. One package often
has two modes — a `dev` script and a `dev:local` one — of which only ever one
runs; adopting the listener into both would report two services as up when at
most one is, and nothing in the process says which. Reporting neither leaves the
port visible as unexplained, which is true. In practice most already-running services land in the third bucket,
because an inferred default port (Next.js 3000) rarely matches what is actually
running (3007) — which is the honest answer, not a shortcoming to paper over.

## Other supervisors

PM2, systemd and launchd start services too, and a runtime that only reports
them is missing the useful half. The split is the one containers already get:
**the supervisor owns what the service is and whether it comes back after a
reboot; this owns whether it is running right now.** So `start`, `stop` and
`restart` are wrapped — named, reversible operations the supervisor offers
itself, which leave its registry untouched — and `delete` is not. Removing an
entry from PM2 is usually also what stops it starting at boot, and that is a
decision about the machine rather than about this registry.

Detection walks the ancestor chain rather than reading the command, since a dev
server started by PM2 looks exactly like one started from a terminal; the
difference is above it in the tree. It needs each ancestor's own argv, which the
bulk process listing does not carry on macOS — and PM2 renames its process, so
in that listing it is just another `node`. The chain above one port is a handful
of processes, so they are read individually.

Knowing the supervisor turns two answers actionable. A service row can offer a
Stop that sticks, by routing it through whoever is holding the process. And a
restart that would fail can be predicted rather than discovered: an entry
running in production mode whose `.next` holds a development build keeps serving
until something restarts it, and then cannot start at all. Both facts are
already known; saying them together is the whole feature.

## Recorded launches

A running process gives up its pid, its port and its directory. The one thing it
cannot be asked for is the command that would start it again — which is exactly
what a runtime needs to be useful, and exactly what inference gets wrong. A
project whose `dev` and `start` scripts write to the same build directory is
left unable to boot by adopting it under the wrong one.

So the runtime asks to be told. A Claude Code `PreToolUse` hook records the
command before it runs and returns nothing, which Claude reads as "proceed
unchanged". Deliberately a recorder and not a rewriter:

* The command shown for approval stays the command that runs, so allow-lists
  keep matching and a transcript still says what happened.
* The daemon executes only registered service definitions. A token indirection
  that let the hook enqueue arbitrary commands would make it a general-purpose
  execution service for anything that can reach the socket.
* Every failure path exits 0 in silence. A runtime that is down must not be able
  to wedge a shell command.

Nothing is claimed on the strength of the recording alone. A note is matched
only to a port that appeared after it, from a directory beneath the one it was
announced in; everything else expires unclaimed, which is the right outcome for
the `git status` calls that make up most of what gets recorded. Paths are
canonicalised on both sides — a shell in `/tmp/x` is reported by the process
table as `/private/tmp/x`, and comparing them as text quietly matches nothing.

What survives that test is evidence rather than a guess, so its children are
attributed to it too.

### The environment is part of how a service runs

Reading the command off the process is necessary and was not sufficient. `node
server.mjs` is a project's development server or its production one depending on
`NODE_ENV` alone, and they overwrite each other's build output. Adopting
captures the environment as well — filtered to the dozen variables that select a
*mode* (`NODE_ENV`, `RAILS_ENV`, `DJANGO_SETTINGS_MODULE`, …). The rest of an
environment is credentials, and a registry that copies it wholesale has written
them to disk. For a supervised service the values come from the supervisor,
which also has them for an entry that is currently stopped.

## Ordering

Dependencies are named services in the same checkout, resolved into a plan
before anything starts. A cycle is reported with the cycle in it; a missing
dependency names what is missing.

A dependency already running is left exactly as it is. Restarting it would take
a working service down to reach the state it was already in, and where something
else supervises it, would lose a race for the port as well.

A **one-shot** step runs to completion instead of staying up, and it is not a
service that happens to exit quickly — the two have opposite tests for success.
A server that exits has failed; a migration that keeps running has hung. They
cannot share one definition of "started", which is why `one_shot` is a property
of the service rather than a shorter health check. A failed one-shot stops what
would have followed it: starting an API against a database whose migration
failed is worse than not starting it.

Its run is recorded even though there is nothing left to stop. "Did the
migration work?" is the only question this kind of step raises, and without a
record the answer is whatever the last attempt left behind — which is how a run
that succeeded goes on reporting the failure before it.

A **task** is a named sequence over both. Dependencies say what one service
needs; a task says what *you* want up, which is often not one service's chain.
Each step brings up its own dependencies, so a step already covered by an
earlier one does nothing.

## Health

The default depends on what the service says it is:

| Declared as | Checked by |
| --- | --- |
| Web, API | An HTTP `GET /`, any response |
| Anything else with a port | A TCP connect |
| No port | The process is alive |

A TCP connect proves something holds the port, which is exactly what a wedged
dev server does — one on this machine accepted connections, answered none of
them, and was reported healthy for an unknown length of time. Anything declared
as serving HTTP is asked to answer.

*Any* response counts. Real services reply to a bare `GET /` with 302, 307 and
404 as often as with 200, and none of that means anything is wrong; the question
is whether it is alive, not whether it agrees about the path. An empty
`expect_status` is what asks for that, so it cannot happen by accident.

Only for the types that claim HTTP. A database holding a port would fail an HTTP
check while being perfectly well, and a check wrong in that direction is worse
than a weak one: it teaches the reader to skip it.

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

Registry edits are announced too, not just lifecycle changes. Adding, correcting
or removing a service used to publish nothing, so a service an agent declared
through MCP did not appear in an open window until something unrelated happened
— which reads exactly like the edit not having worked.

A second connection carries the event subscription, so a long-lived stream never
interleaves with a command the user just clicked. Events are re-emitted to the
frontend on the `runtime://event` channel, which is what makes a service started
from a terminal appear in the window without polling.

Two things still poll, because the daemon has no event for them: the ports table
(the socket table changes without the runtime's involvement) and log tailing,
which uses the `since_seq` cursor so each tick transfers only new lines.

## Dialogs

The webview implements none of the JavaScript panel callbacks, so `alert`,
`confirm` and `prompt` do nothing and return null or false. Three controls were
built on them and were silently dead: adding a service, scanning a folder, and
removing a service — the last being a destructive action whose button appeared
to work and did not.

Anything that needs an answer from the user is an in-app sheet. Confirmation is
a two-step button rather than a dialog: the first click arms it, the second
performs it.

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

Settings live in the daemon's `settings` table as an opaque JSON blob. The
geometry means nothing to the daemon, but keeping it there is what makes it
survive reinstalling the bundle, and leaves one answer to "where is the state"
instead of two. A blob it cannot parse — an older layout — falls back to
defaults with a warning rather than blocking the settings screen.

Rebinding the shortcut registers the new accelerator *before* releasing the old
one, so a combination another app already owns is refused with the previous one
still in force.

Edge detection polls the pointer every 80ms on the main thread. The alternative,
a global `CGEventTap`, would demand Accessibility permission for something this
small; polling asks for nothing and is imperceptible.

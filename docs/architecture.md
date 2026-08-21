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

## Reading order

* This file — the daemon: crates, data model, the properties everything else
  rests on, and how a request becomes a running process.
* [ownership.md](ownership.md) — what is running and whose it is: declared,
  adopted, supervised, containers, worktrees, and what the runtime is willing to
  claim about each.
* [desktop.md](desktop.md) — the app, its dialogs, and the edge panel.
* [mcp.md](mcp.md) — the agent-facing surface.
* [windows.md](windows.md) — the port, and what is still missing there.

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

## The properties this design rests on

The first three were decided before there was much to decide them about.
The last two were learned, each from something that broke.

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

### One fact, one source

An operation that reads the runtime's own instance record cannot see a service
somebody else started, and on a working machine that is most of them.

This was one bug, found five times. `stop` reported "not running" for something
the view showed serving. `health` said the same. `start` launched a second copy
beside the one already up. `restart` skipped the stop and did the same again.
Registering a worktree copied the primary checkout's services when discovery did
it and not when a person asked for it. Every one was a second code path for a
question that already had an answer somewhere else.

The failures are not symmetrical, which is why this is worth a rule rather than
vigilance. The first two merely said something false about a running service.
`start` broke one: a duplicate arrives with different arguments than the process
already serving, and for a project whose two servers share a build directory,
the second overwrites what the first is running from.

So when a fix lands, the question is not whether it is correct but where else
that fact is read. Four of the five above were found by asking it.

### A check that cannot tell must not answer

The liveness test in one of these tests ran `ps -p` and treated a failure to run
it as "the process is gone" — which is that test's failure condition. On a
platform without `ps` it reported the exact bug it exists to catch. It would
have passed for the wrong reason just as readily.

The same shape, three more times in one week. `dir.join("PM2").is_file()` says
yes on a case-insensitive filesystem because of an unrelated `pm2`. A frontend
type said a field was always present; the daemon omits it when empty; reading
`.length` off the gap took the window down. `runtime list` accepted `--project`
and ignored it, which is worse than rejecting it, because the output looks like
an answer to the question that was asked.

An answer given without the information to give it is indistinguishable from a
real one, which is precisely what makes it expensive. `ProcessProvider::environment`
returns `Option` for this reason: the default is "cannot tell", and a caller
that reads it as "empty" is wrong in a way the type will not let it be.

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

## Checking what is declared

Every problem `diagnose` looks for is already knowable and stays quiet until the
moment it is expensive:

| Found | Why it waits |
| --- | --- |
| A dependency naming nothing | Fails halfway through a start, with everything before it already up |
| Services depending on each other | Hangs |
| A task step that was removed | Same, one layer out |
| A command that will not resolve here | A command is written in the shell that had it working and run by a daemon whose `PATH` is whatever launched the app |
| A build directory two services share | Breaks whichever of them is not looking, on its next restart, hours after the cause |

It is silent when it cannot tell. A command with shell syntax in it goes through
`sh -c` and may resolve in ways this cannot follow; a warning that fires on
working services teaches the reader to skip it, and then it is worse than none.

The same list appears in the app above the projects — not behind a tab, because
a warning nobody goes looking for is a warning that does not exist.

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

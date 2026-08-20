# What is running, and whose it is

The question this tool exists to answer, and the one it is easiest to answer
wrongly. A process on a port is real whoever started it; saying otherwise
contradicts the port table, and claiming more than is known is how a service
gets restarted under a command nobody ran.

The mechanics of starting and watching things are in
[architecture.md](architecture.md).

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

### Where the command comes from

Three sources, in decreasing order of how much they know:

1. **The supervisor**, when one holds it. PM2 stores what it will run *next
   time*, which is the actual question, and it answers for an entry that is
   currently stopped.
2. **A recorded launch**, which is what somebody asked for before the shell and
   the package manager got to it.
3. **The process itself**, as a last resort.

The last one has a failure mode worth naming: a process may rename itself, and
the good ones do. Next reports its argv as `next-server (v14.2.35)` and PM2 as
`PM2 v6.0.14: God Daemon` — far more useful in a process listing than the paths
they replaced, and not commands. Writing one into a definition produces a
service that looks correctly declared and cannot start, so an argv that names
nothing runnable is refused instead.

Testing whether a bare word is runnable means looking at `PATH`, and on a
case-insensitive filesystem `dir.join("PM2").is_file()` answers yes because of
an unrelated `pm2`. Directory entries are compared by name.

### The environment is part of how a service runs

Reading the command off the process is necessary and was not sufficient. `node
server.mjs` is a project's development server or its production one depending on
`NODE_ENV` alone, and they overwrite each other's build output. Adopting
captures the environment as well — filtered to the dozen variables that select a
*mode* (`NODE_ENV`, `RAILS_ENV`, `DJANGO_SETTINGS_MODULE`, …). The rest of an
environment is credentials, and a registry that copies it wholesale has written
them to disk. For a supervised service the values come from the supervisor,
which also has them for an entry that is currently stopped.

## Worktrees

Each checkout is a workspace with its own port offset, carrying the project's
services so a second branch can be served without redeclaring anything.

Registering one **tops up** rather than replaces. Adding a project registers
every worktree it finds, usually at a moment when the project has no services
at all, so registering again later is how a service declared after the branch
was made reaches it — and a copy that has been edited on its own terms is left
alone rather than quietly overwritten.

Dependencies are names within a workspace, so a copy resolves against its own
siblings rather than reaching back into the checkout it came from. A **task** is
the other way round: declared once for the project, since every checkout has the
same service names, and run in whichever checkout the caller names. Two branches
served at once, on different ports, from one definition.

Resolving a path has to look at checkouts and not just project roots. A git
worktree lives outside the repository it was branched from, so the directory
somebody is standing in matches no project root at all.

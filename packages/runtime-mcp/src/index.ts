#!/usr/bin/env node
/**
 * MCP server for the local development runtime manager.
 *
 * Every tool is a semantic operation — `restart_service("api")`, not
 * `exec("kill -9 8291")`. There is deliberately no shell, no arbitrary
 * command execution and no kill-by-pid: the daemon's protocol does not offer
 * them, so this server cannot expose them even by accident. That boundary is
 * what makes it safe to hand to an agent.
 *
 * The server holds no runtime state. It is a client of the daemon, exactly like
 * the CLI and the desktop app, so a service an agent starts here is visible in
 * the GUI immediately and survives this process exiting.
 */

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";

import { DaemonClient, resolveEndpoint } from "./client.js";
import {
  formatAdopted,
  formatContainer,
  formatDiscoveries,
  formatFailures,
  formatFindings,
  formatHealth,
  formatLaunches,
  formatLogs,
  formatPortStatus,
  formatPorts,
  formatProjectRuntime,
  formatProjects,
  formatReservation,
  formatServiceDetail,
  formatServices,
  formatSupervised,
  formatStacks,
  formatStart,
  formatWorktrees,
  formatService,
} from "./format.js";
import type { ResponseBody } from "./protocol.js";
import { detectAgent, registerSession, type AgentIdentity } from "./session.js";

/** Log reads are capped so one call cannot flood an agent's context. */
const MAX_LOG_LINES = 500;
const DEFAULT_LOG_LINES = 100;

const SERVICE_DESCRIPTION =
  "Service name (e.g. 'web'), 'branch/name' for a git worktree (e.g. 'feature/refund/web'), or a service id.";
const PROJECT_DESCRIPTION =
  "Project id, name, or a path inside it. Optional when the service name is unambiguous.";

async function main(): Promise<void> {
  const identity = detectAgent(process.argv.slice(2));
  const client = new DaemonClient(resolveEndpoint());
  const session = await registerSession(client, identity);

/**
 * What a model needs before its first call.
 *
 * Written as the rules that would otherwise be discovered by being refused —
 * a refusal is a fine teacher but an expensive one, and some of these (killing
 * something the runtime did not start) are the kind that should never be
 * reached by trial.
 */
const INSTRUCTIONS = `Local Runtime manages the services a developer runs on
localhost — dev servers, databases, workers, containers — so that a person at
the desktop app and an agent here see and change the same state. A service you
start is visible in their window immediately, and survives you exiting.

How to work with it:

Look before acting. \`list_projects\` for what exists, \`list_stacks\` for how a
project is brought up, \`diagnose\` for what is already wrong with the
declarations. \`diagnose\` is worth calling in an unfamiliar project: it reports
dependencies naming services that do not exist, two services asking for one
port, and commands that will not resolve — each quiet until the moment it is
expensive.

**A stack is the only thing you can start.** \`run_stack\` brings up every member
in dependency order, waiting for each to report healthy. \`start_service\` exists
but refuses on its own and tells you which stack to run instead. This is not a
limitation to work around: a service started outside the group it belongs to is
missing whatever it waits for, and the failure surfaces somewhere else.

A stack stops whole. If one member cannot start, the run stops there and what
is already up stays up — half a stack is a state nobody chose.

Ports. A service may declare one, or declare none. A declared port is treated
as meant: if it is taken, the start stops and names the holder rather than
moving quietly to another port. A service with no port declared takes any free
one, freshly at each start. When a start fails on a port, read who holds it
before deciding anything — it is often another project of the user's.

**Never terminate what the runtime did not start.** There is no kill-by-pid
here and there will not be. A process another supervisor owns (pm2, docker,
launchd) is reported with the supervisor's name and its own handle, so the
honest fix is to go through that supervisor. \`take_over_service\` and
\`run_stack\` with freed ports are the two doors, and both refuse a process
somebody else is watching.

When something fails to start, \`get_logs\` says what it said and
\`recent_errors\` collects them. Most failures are a missing environment
variable or the wrong command — \`update_service\` is how you correct either,
and correcting the port is also what lets the runtime recognise an
already-running process as that service.

\`healthy\` is not always a statement about a process. For a service the
runtime started, a dead process is \`stopped\` no matter what the port says.
But a service reported as started elsewhere has no process the runtime owns,
and its health is only "something answers on that port" — while its logs are
still the ones captured from the process the runtime used to run, which has
since exited. Read that qualifier before believing anything out of
\`get_logs\`: a token, a URL or a port lifted from a dead instance's output is
stale, and it fails somewhere far from here.

Environment variables: set what a service needs through \`update_service\`, and
do not read their values back out of the user's shell to put them somewhere
else. Names are safe to discuss; values are usually credentials.

There is also a \`runtime\` CLI, and its syntax is not these tool names. When
the user wants to run something themselves, give them the command rather than
a translation of a tool call: \`run_stack\` is
\`runtime stack run <name> --project <project>\`, \`list_services\` is
\`runtime service list --project <project>\`, and \`--project\` and \`--json\`
are accepted by every command. Do not derive one vocabulary from the other —
they are close enough to guess wrong, and a guessed command in a code block is
a command somebody runs.`

  const server = new McpServer(
    {
      name: "local-runtime",
      version: "0.1.0",
    },
    // Handed to the model at handshake, before it has called anything.
    //
    // The tool descriptions say what each call does; none of them can say what
    // this runtime is *for*, or which of its rules will refuse a call that
    // looks perfectly reasonable. An agent that has only the list will try
    // `start_service` first — that is the obvious name — and be told no.
    { instructions: INSTRUCTIONS },
  );

  registerTools(server, client, identity, session?.id);

  const transport = new StdioServerTransport();
  await server.connect(transport);

  const shutdown = () => {
    client.close();
    process.exit(0);
  };
  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
}

function registerTools(
  server: McpServer,
  client: DaemonClient,
  identity: AgentIdentity,
  sessionId: string | undefined,
): void {
  /** Run a call and render it, turning daemon errors into tool errors. */
  const run = async (
    method: string,
    params: Record<string, unknown> | undefined,
    render: (body: ResponseBody) => string,
  ) => {
    try {
      const body = await client.call(method, params);
      return { content: [{ type: "text" as const, text: render(body) }] };
    } catch (error) {
      // Daemon errors are actionable ("port 3000 is in use by dossh/main/web"),
      // so they reach the agent as text rather than a transport failure.
      return {
        content: [{ type: "text" as const, text: (error as Error).message }],
        isError: true,
      };
    }
  };

  /** Attribution sent with every state-changing call. */
  const attribution = {
    started_by: identity.client,
    session: sessionId ?? null,
  };

  // ---- runtime -------------------------------------------------------

  server.registerTool(
    "list_projects",
    {
      title: "List projects",
      description:
        "List every project the runtime knows about, with how many of its services are running.",
      inputSchema: {},
    },
    async () =>
      run("list_projects", undefined, (body) =>
        body.type === "projects" ? formatProjects(body.items) : unexpected(body),
      ),
  );

  server.registerTool(
    "discover_projects",
    {
      title: "Discover projects",
      description:
        "Find projects on this machine without being told where they are. Always reports what is listening right now, resolved back to its repository; pass paths to also search directory trees for projects that are stopped. Use this when list_projects is empty or the project you need is not registered.",
      inputSchema: {
        paths: z
          .array(z.string())
          .optional()
          .describe("Absolute directory trees to search, in addition to running processes."),
        adopt: z
          .boolean()
          .optional()
          .describe(
            "Register everything found instead of only reporting it. Registration records what is already there; it never starts or stops anything.",
          ),
      },
    },
    async ({ paths, adopt }) =>
      run("discover_projects", { paths: paths ?? [], adopt: adopt ?? false }, (body) =>
        body.type === "discoveries" ? formatDiscoveries(body.items) : unexpected(body),
      ),
  );

  server.registerTool(
    "add_project",
    {
      title: "Register a project",
      description:
        "Register a directory as a project, detecting the services in it. Detection runs once, when it is first added: adding it again will not undo a command somebody corrected or bring back a service they removed. A directory that is a second clone of a repository already registered becomes a checkout of it rather than a second project.",
      inputSchema: {
        path: z.string().describe("Absolute path to the project directory."),
        name: z.string().optional().describe("What to call it. Detected when unsaid."),
      },
    },
    async ({ path, name }) =>
      run("add_project", { path, name: name ?? null }, (body) =>
        body.type === "project" ? formatProjectRuntime(body) : unexpected(body),
      ),
  );

  server.registerTool(
    "remove_project",
    {
      title: "Forget a project",
      description:
        "Stop tracking a project, its checkouts and everything declared in them. No directory is touched and nothing running is stopped — this is the runtime forgetting, not a deletion. Use it when a project's root directory has moved or gone, since the registration is what still points at the old one.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
      },
    },
    async ({ project }) =>
      run("remove_project", { selector: project }, (body) =>
        body.type === "done"
          ? body.ok
            ? `${project} is no longer tracked`
            : `${project} was not tracked`
          : unexpected(body),
      ),
  );

  server.registerTool(
    "get_project_runtime",
    {
      title: "Get project runtime",
      description:
        "Show one project's workspaces (including git worktrees) and the live status, port and owner of each service. Start here when asked what a project is running.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
      },
    },
    async ({ project }) =>
      run("get_project", { selector: project }, (body) =>
        body.type === "project" ? formatProjectRuntime(body) : unexpected(body),
      ),
  );

  server.registerTool(
    "remove_worktree",
    {
      title: "Forget a checkout",
      description:
        "Stop tracking a checkout of a project. The directory is not touched — this is the runtime forgetting it, not git losing it. Refused for the project's own checkout, which is remove_project, and refused while anything in it is running, since the services go with the registration and the processes would not.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
        checkout: z.string().describe("Its path, or its branch."),
      },
    },
    async ({ project, checkout }) =>
      run("remove_worktree", { selector: project, checkout }, (body) =>
        body.type === "done"
          ? body.ok
            ? `${checkout} is no longer tracked`
            : `${checkout} was not tracked`
          : unexpected(body),
      ),
  );

  server.registerTool(
    "list_services",
    {
      title: "List services",
      description: "List services with their live status and port, optionally for one project.",
      inputSchema: {
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
      },
    },
    async ({ project }) =>
      run("list_services", { project: project ?? null }, (body) =>
        body.type === "services" ? formatServices(body.items) : unexpected(body),
      ),
  );

  server.registerTool(
    "get_service",
    {
      title: "Get service",
      description: "Show one service's command, working directory, status, port and URL.",
      inputSchema: {
        service: z.string().describe(SERVICE_DESCRIPTION),
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
      },
    },
    async ({ service, project }) =>
      run("get_service", { service, project: project ?? null }, (body) =>
        body.type === "service" ? formatServiceDetail(body) : unexpected(body),
      ),
  );

  server.registerTool(
    "update_service",
    {
      title: "Correct a service",
      description:
        "Change how a service starts: its command, port, working directory, environment or conflict policy. Service definitions come from inference, which guesses — a framework's default port is often not the one a project uses, and a repository with `dev` and `dev:local` scripts may need the second one. This is the tool for acting on a failed start: set the variable it said was missing, or point it at the command that works. Correcting the port is also what lets the runtime recognise an already-running service as that service.",
      inputSchema: {
        service: z.string().describe(SERVICE_DESCRIPTION),
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
        command: z
          .string()
          .optional()
          .describe("The command that starts it, e.g. 'pnpm run dev:local'."),
        env: z
          .record(z.string())
          .optional()
          .describe(
            "Environment variables, merged with the ones already set — passing one does not drop the others. These win over any .env file. Use this when a start fails on a missing variable.",
          ),
        unset_env: z
          .array(z.string())
          .optional()
          .describe("Variables to drop. Needed because `env` merges rather than replaces."),
        port: z
          .number()
          .int()
          .min(1)
          .max(65535)
          .optional()
          .describe("The port this service should use."),
        clear_port: z
          .boolean()
          .optional()
          .describe("Forget the port entirely, for a service that has none."),
        cwd: z.string().optional().describe("Absolute, or relative to the workspace."),
        rename: z.string().optional(),
        service_type: z
          .enum(["web", "api", "worker", "database", "cache", "container", "custom"])
          .optional(),
        on_conflict: z
          .enum(["reuse", "allocate-next", "fail", "ask", "kill-existing"])
          .optional()
          .describe(
            "What to do when the port is taken. 'fail' is right for a service that hardcodes its port, where being moved to another one cannot work.",
          ),
        auto_start: z.boolean().optional(),
      },
    },
    async ({
      service,
      project,
      command,
      env,
      unset_env,
      port,
      clear_port,
      cwd,
      rename,
      service_type,
      on_conflict,
      auto_start,
    }) => {
      const patch: Record<string, unknown> = {};
      // An absent key means "leave it alone" and an explicit null means "clear
      // it", so the two have to be distinguishable here as well.
      if (clear_port) patch.preferred_port = null;
      else if (port !== undefined) patch.preferred_port = port;
      if (command !== undefined) patch.command = command;
      if (env !== undefined) patch.env = env;
      if (unset_env !== undefined) patch.remove_env = unset_env;
      if (cwd !== undefined) patch.cwd = cwd;
      if (rename !== undefined) patch.name = rename;
      if (service_type !== undefined) patch.service_type = service_type;
      if (on_conflict !== undefined) patch.conflict_policy = on_conflict;
      if (auto_start !== undefined) patch.auto_start = auto_start;

      if (Object.keys(patch).length === 0) {
        return {
          content: [{ type: "text" as const, text: "Nothing to change." }],
          isError: true,
        };
      }
      return run(
        "update_service",
        { service, project: project ?? null, patch },
        (body) => (body.type === "service" ? formatServiceDetail(body) : unexpected(body)),
      );
    },
  );

  server.registerTool(
    "add_service",
    {
      title: "Declare a service",
      description:
        "Declare a service that detection did not find — a script it could not infer, a second process a project needs. Registers the definition only; nothing is started.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
        name: z.string().describe("What to call it, e.g. 'worker'."),
        command: z.string().describe("The command that starts it."),
        port: z.number().int().min(1).max(65535).optional(),
        cwd: z
          .string()
          .optional()
          .describe("Absolute, or relative to the project root. Defaults to the root."),
        service_type: z
          .enum(["web", "api", "worker", "database", "cache", "container", "custom"])
          .optional(),
      },
    },
    async ({ project, name, command, port, cwd, service_type }) =>
      run(
        "add_service",
        {
          selector: project,
          name,
          command,
          port: port ?? null,
          cwd: cwd ?? null,
          type: service_type ?? null,
        },
        (body) => (body.type === "service" ? formatServiceDetail(body) : unexpected(body)),
      ),
  );

  server.registerTool(
    "remove_service",
    {
      title: "Remove a service",
      description:
        "Remove a service definition. Nothing running is stopped — this forgets how to start it, not the process itself. Use it for a service detection invented that the project does not have.",
      inputSchema: {
        service: z.string().describe(SERVICE_DESCRIPTION),
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
      },
    },
    async ({ service, project }) =>
      run("remove_service", { service, project: project ?? null }, (body) =>
        body.type === "done"
          ? body.ok
            ? "Removed."
            : "There was no such service."
          : unexpected(body),
      ),
  );

  server.registerTool(
    "export_config",
    {
      title: "Export .runtime.json",
      description:
        "Return the project's services as a committable .runtime.json. Inference is a starting point; this is how a corrected set of services becomes something the repository carries and a teammate gets without repeating the work.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
      },
    },
    async ({ project }) =>
      run("export_config", { selector: project }, (body) =>
        body.type === "config"
          ? JSON.stringify({ name: body.name, services: body.services }, null, 2)
          : unexpected(body),
      ),
  );

  // ---- lifecycle -----------------------------------------------------

  server.registerTool(
    "start_service",
    {
      title: "Start service",
      description:
        "Start a service — refused on its own. Services run as part of a stack, so run_stack is what brings one up; this reports which stack to run. Already-running services are returned as-is rather than started twice. If the preferred port is taken by another project, the runtime allocates the next free one and says so.",
      inputSchema: {
        service: z.string().describe(SERVICE_DESCRIPTION),
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
        port: z.number().int().min(1).max(65535).optional().describe("Override the service's configured port."),
        on_conflict: z
          .enum(["reuse", "allocate-next", "fail", "ask", "kill-existing"])
          .optional()
          .describe(
            "What to do if the port is taken. Defaults to the service's own policy (usually allocate-next). 'kill-existing' only ever affects processes the runtime itself started.",
          ),
      },
    },
    async ({ service, project, port, on_conflict }) =>
      run(
        "start_service",
        {
          service,
          project: project ?? null,
          port: port ?? null,
          on_conflict: on_conflict ?? null,
          ...attribution,
        },
        (body) => (body.type === "started" ? formatStart(body) : unexpected(body)),
      ),
  );

  server.registerTool(
    "take_over_service",
    {
      title: "Take over service",
      description:
        "Stop a service that something else started, and start it here instead, so it can be managed from now on. The runtime never terminates what it did not start; this is the one exception, and it needs a declared service that is holding its port right now. Refused when another supervisor keeps the service alive — that supervisor would start it again a second later, so switch it off there or drive it through control_supervised.",
      inputSchema: {
        service: z.string().describe(SERVICE_DESCRIPTION),
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
        timeout_seconds: z
          .number()
          .int()
          .min(1)
          .max(120)
          .optional()
          .describe("How long to wait before forcing termination. Defaults to 8."),
      },
    },
    async ({ service, project, timeout_seconds }) =>
      run(
        "take_over_service",
        { service, project: project ?? null, timeout_seconds: timeout_seconds ?? null },
        (body) => (body.type === "service" ? formatService(body) : unexpected(body)),
      ),
  );

  server.registerTool(
    "stop_service",
    {
      title: "Stop service",
      description:
        "Stop a service — refused on its own. A stack comes down through stop_stack, in reverse; taking one member down out from under the others leaves the rest running while every list reads as though the set is up. Stops every process the service spawned. Terminates gracefully first, then forcefully if it does not exit in time.",
      inputSchema: {
        service: z.string().describe(SERVICE_DESCRIPTION),
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
        timeout_seconds: z
          .number()
          .int()
          .min(1)
          .max(120)
          .optional()
          .describe("How long to wait before forcing termination. Defaults to 8."),
      },
    },
    async ({ service, project, timeout_seconds }) =>
      run(
        "stop_service",
        { service, project: project ?? null, timeout_seconds: timeout_seconds ?? null },
        (body) =>
          body.type === "service" ? `${body.name} stopped` : unexpected(body),
      ),
  );

  server.registerTool(
    "restart_service",
    {
      title: "Restart service",
      description:
        "Restart a service — refused on its own. A restart ends with the service up, so it is a way of starting one, and services run as part of a stack: stop_stack then run_stack brings the set round. This reports which stack the service is in.",
      inputSchema: {
        service: z.string().describe(SERVICE_DESCRIPTION),
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
      },
    },
    async ({ service, project }) =>
      run(
        "restart_service",
        { service, project: project ?? null, ...attribution },
        (body) => (body.type === "started" ? formatStart(body) : unexpected(body)),
      ),
  );

  // ---- containers ----------------------------------------------------

  server.registerTool(
    "control_container",
    {
      title: "Start or stop a container",
      description:
        "Switch a container on or off by name. Works for containers the runtime did not create: unlike signalling a process, `docker stop` is a graceful operation on a named, restartable object. Compose still owns what these services are — this owns whether they run. Container names come from get_project_runtime.",
      inputSchema: {
        name: z.string().describe("Container name, e.g. 'stockviewer-db'."),
        action: z.enum(["start", "stop", "restart"]),
      },
    },
    async ({ name, action }) =>
      run("control_container", { name, action }, (body) =>
        body.type === "container" ? formatContainer(body) : unexpected(body),
      ),
  );

  server.registerTool(
    "get_container_logs",
    {
      title: "Get container logs",
      description:
        "Read a container's own output. The runtime never captured it — Docker did — so this asks Docker for it.",
      inputSchema: {
        name: z.string().describe("Container name."),
        max_lines: z.number().int().min(1).max(MAX_LOG_LINES).optional(),
      },
    },
    async ({ name, max_lines }) =>
      run(
        "get_container_logs",
        { name, max_lines: Math.min(max_lines ?? DEFAULT_LOG_LINES, MAX_LOG_LINES) },
        (body) =>
          body.type === "logs"
            ? body.items.map((line) => line.message).join("\n") || "(no output)"
            : unexpected(body),
      ),
  );

  // ---- health --------------------------------------------------------

  server.registerTool(
    "get_health",
    {
      title: "Get service health",
      description:
        "Probe a service right now. Distinguishes 'the process exists' from 'the service answers'.",
      inputSchema: {
        service: z.string().describe(SERVICE_DESCRIPTION),
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
      },
    },
    async ({ service, project }) =>
      run("get_health", { service, project: project ?? null }, (body) =>
        body.type === "health" ? formatHealth(body) : unexpected(body),
      ),
  );

  server.registerTool(
    "wait_until_healthy",
    {
      title: "Wait until healthy",
      description:
        "Block until a service reports healthy or the timeout expires. Use this after starting or restarting instead of polling get_health.",
      inputSchema: {
        service: z.string().describe(SERVICE_DESCRIPTION),
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
        timeout_seconds: z
          .number()
          .int()
          .min(1)
          .max(300)
          .optional()
          .describe("Defaults to 60."),
      },
    },
    async ({ service, project, timeout_seconds }) =>
      run(
        "wait_until_healthy",
        { service, project: project ?? null, timeout_seconds: timeout_seconds ?? null },
        (body) => (body.type === "health" ? formatHealth(body) : unexpected(body)),
      ),
  );

  // ---- ports ---------------------------------------------------------

  server.registerTool(
    "check_port",
    {
      title: "Check a port",
      description:
        "Find out what is listening on a port, resolved to a project, branch and service where possible, and what port to use instead. Use this when a localhost URL is unexpectedly unavailable or already taken.",
      inputSchema: {
        port: z.number().int().min(1).max(65535),
      },
    },
    async ({ port }) =>
      run("check_port", { port }, (body) =>
        body.type === "port" ? formatPortStatus(body) : unexpected(body),
      ),
  );

  server.registerTool(
    "list_ports",
    {
      title: "List listening ports",
      description:
        "List everything listening on this machine, including processes the runtime did not start.",
      inputSchema: {},
    },
    async () =>
      run("list_ports", undefined, (body) =>
        body.type === "ports" ? formatPorts(body.items) : unexpected(body),
      ),
  );

  server.registerTool(
    "recent_errors",
    {
      title: "What is broken right now",
      description:
        "List every service that is failing or unhealthy, newest first, each with the last thing it said — preferring stderr, since a busy service's access log will otherwise bury the reason. Start here when something is wrong and you do not know which service it was: the alternative is guessing a service name and then reading its whole log. Use get_logs afterwards for the full output of one of them.",
      inputSchema: {
        lines: z
          .number()
          .int()
          .min(1)
          .max(50)
          .optional()
          .describe("Lines of explanation per service. Defaults to 8."),
      },
    },
    async ({ lines }) =>
      run("list_failures", { detail_lines: lines ?? 8 }, (body) =>
        body.type === "failures" ? formatFailures(body.items) : unexpected(body),
      ),
  );

  server.registerTool(
    "diagnose",
    {
      title: "Check what is declared",
      description:
        "List everything wrong with the declared services that has not caused a failure yet: a dependency naming a service that does not exist, services depending on each other, a stack step that was removed, a command that will not resolve from the daemon, and a build directory two services would overwrite for each other. Worth calling before starting things in an unfamiliar project — each of these is quiet until the moment it is expensive, and several of them fail somewhere other than where the cause is.",
      inputSchema: {},
    },
    async () =>
      run("diagnose", undefined, (body) =>
        body.type === "findings" ? formatFindings(body.items) : unexpected(body),
      ),
  );

  server.registerTool(
    "control_supervised",
    {
      title: "Switch a supervised service",
      description:
        "Start, stop or restart a service that another supervisor (PM2, systemd) keeps. Use this rather than start_service or stop_service when a service reports a supervisor: the runtime did not start it, and a stop issued any other way is undone the moment that supervisor notices. The supervisor's own registry is untouched, so what starts at boot does not change. Deleting an entry is deliberately not offered.",
      inputSchema: {
        name: z
          .string()
          .describe("The supervisor's own name for it, as reported on the service."),
        action: z.enum(["start", "stop", "restart"]),
      },
    },
    async ({ name, action }) =>
      run("control_supervised", { name, action }, (body) =>
        body.type === "supervised" ? formatSupervised(body) : unexpected(body),
      ),
  );

  // ---- stacks ---------------------------------------------------------

  server.registerTool(
    "list_stacks",
    {
      title: "List stacks",
      description:
        "List the stacks declared in a project. A stack is a named set of services brought up together; what waits for what comes from the members' own dependencies, so the answer shows which of them start at the same time.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
      },
    },
    async ({ project }) =>
      run("list_stacks", { selector: project }, (body) =>
        body.type === "stacks" ? formatStacks(body.items) : unexpected(body),
      ),
  );

  server.registerTool(
    "set_stack",
    {
      title: "Declare a stack",
      description:
        "Declare or replace a stack: a named set of services brought up together. Members are service names. Order is not given here — it comes from the members' own dependencies, and members that wait for nothing start at the same time. Every member is checked now rather than when it runs.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
        name: z.string(),
        members: z.array(z.string()).describe("Service names, in order."),
        auto_start: z
          .boolean()
          .optional()
          .describe(
            "Bring this stack up when the daemon starts. Omit to leave it as it is — changing the members says nothing about boot.",
          ),
      },
    },
    async ({ project, name, members, auto_start }) =>
      run("set_stack", { selector: project, name, members, auto_start }, (body) =>
        body.type === "stacks" ? formatStacks(body.items) : unexpected(body),
      ),
  );

  server.registerTool(
    "remove_stack",
    {
      title: "Remove a stack",
      description:
        "Undeclare a stack. Its members are left alone — this removes the grouping, not the services, and nothing running is stopped. A service that ends up in no stack cannot be started by name until it is in one again.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
        name: z.string(),
      },
    },
    async ({ project, name }) =>
      run("remove_stack", { selector: project, name }, (body) =>
        body.type === "done"
          ? body.ok
            ? `${name} is no longer a stack`
            : `there is no stack called ${name} here`
          : unexpected(body),
      ),
  );

  server.registerTool(
    "run_stack",
    {
      title: "Run a stack",
      description:
        "Bring up every step of a stack in order, waiting for each to report healthy before the next. A step that runs to completion must succeed or the stack stops there — starting an API against a database whose migration failed is worse than not starting it. A service already running is left alone rather than restarted.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
        name: z.string(),
      },
    },
    async ({ project, name }) =>
      run("run_stack", { selector: project, name }, (body) =>
        body.type === "stack_run"
          ? body.done.map((step) => `* ${step}`).join("\n") || "Nothing to do."
          : unexpected(body),
      ),
  );

  server.registerTool(
    "stop_stack",
    {
      title: "Stop a group",
      description:
        "Stop everything a stack started, in the reverse of the order it started — a front end before the API it talks to, and that before the database under both. A member the runtime did not start is left alone, and one already stopped is not an error.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
        name: z.string(),
      },
    },
    async ({ project, name }) =>
      run("stop_stack", { selector: project, name }, (body) =>
        body.type === "stack_run"
          ? body.done.map((step) => `* ${step}`).join("\n") || "Nothing to do."
          : unexpected(body),
      ),
  );

  server.registerTool(
    "adopt_port",
    {
      title: "Take control of a port",
      description:
        "Declare whatever is already listening on a port as a service, so it can be stopped and started from here afterwards. The command is read off the running process, never guessed from package.json — a project whose dev and start scripts share a build directory is left unable to boot if it is adopted under the wrong one. It is put in a stack so that it can be, since a service in no stack cannot be started by name. Nothing is stopped or restarted. Refuses when another supervisor (PM2, systemd) is keeping the service alive, because taking it over means removing it from there, which usually changes what starts at boot; pass force to declare it anyway.",
      inputSchema: {
        port: z.number().int().min(1).max(65535),
        stack: z
          .string()
          .optional()
          .describe(
            "Which stack to put it in, so it can be started afterwards. Its own, named after it, when unsaid.",
          ),
        force: z
          .boolean()
          .optional()
          .describe("Declare it even though another supervisor keeps it alive."),
      },
    },
    async ({ port, stack, force }) =>
      run("adopt_port", { port, stack: stack ?? null, force: force ?? false }, (body) =>
        body.type === "adopted" ? formatAdopted(body) : unexpected(body),
      ),
  );

  server.registerTool(
    "list_launches",
    {
      title: "List recorded launches",
      description:
        "List the service launches the runtime was told about but did not perform — what an agent or a terminal started, with the command exactly as it was given. A launch that turned into a listening port carries the port and pid it became. Use this to find out how something now running was actually started.",
      inputSchema: {},
    },
    async () =>
      run("list_launches", undefined, (body) =>
        body.type === "launches" ? formatLaunches(body.items) : unexpected(body),
      ),
  );

  server.registerTool(
    "reserve_port",
    {
      title: "Reserve a port",
      description:
        "Claim a port for a service before starting it yourself. Returns the port to actually use, which may differ from the preferred one. Prefer start_service, which reserves automatically.",
      inputSchema: {
        service: z.string().describe(SERVICE_DESCRIPTION),
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
        port: z.number().int().min(1).max(65535).optional(),
        on_conflict: z
          .enum(["reuse", "allocate-next", "fail", "ask", "kill-existing"])
          .optional(),
      },
    },
    async ({ service, project, port, on_conflict }) =>
      run(
        "reserve_port",
        {
          service,
          project: project ?? null,
          port: port ?? null,
          on_conflict: on_conflict ?? null,
          started_by: identity.client,
        },
        (body) => (body.type === "reservation" ? formatReservation(body) : unexpected(body)),
      ),
  );

  server.registerTool(
    "release_port",
    {
      title: "Release a port lease",
      description:
        "Drop the runtime's lease on a port. This does not stop any process — use stop_service for that.",
      inputSchema: {
        port: z.number().int().min(1).max(65535),
      },
    },
    async ({ port }) =>
      run("release_port", { port }, (body) =>
        body.type === "done" ? (body.ok ? "Lease released." : "No lease on that port.") : unexpected(body),
      ),
  );

  // ---- logs ----------------------------------------------------------

  server.registerTool(
    "get_logs",
    {
      title: "Get service logs",
      description:
        "Read a service's captured stdout and stderr. Pass since_seq with the cursor from a previous call to fetch only new lines.",
      inputSchema: {
        service: z.string().describe(SERVICE_DESCRIPTION),
        project: z.string().optional().describe(PROJECT_DESCRIPTION),
        max_lines: z
          .number()
          .int()
          .min(1)
          .max(MAX_LOG_LINES)
          .optional()
          .describe(`Most recent lines to return. Defaults to ${DEFAULT_LOG_LINES}.`),
        since_seq: z
          .number()
          .int()
          .min(0)
          .optional()
          .describe("Return only lines newer than this cursor."),
      },
    },
    async ({ service, project, max_lines, since_seq }) =>
      run(
        "get_logs",
        {
          service,
          project: project ?? null,
          max_lines: Math.min(max_lines ?? DEFAULT_LOG_LINES, MAX_LOG_LINES),
          since_seq: since_seq ?? null,
        },
        (body) => (body.type === "logs" ? formatLogs(body.items) : unexpected(body)),
      ),
  );

  // ---- git -----------------------------------------------------------

  server.registerTool(
    "list_worktrees",
    {
      title: "List worktrees",
      description:
        "List a project's checkouts and their port offsets. New git worktrees are discovered and registered by this call, which is what gives a branch its own stable ports.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
      },
    },
    async ({ project }) =>
      run("list_worktrees", { selector: project }, (body) =>
        body.type === "workspaces" ? formatWorktrees(body.items) : unexpected(body),
      ),
  );

  server.registerTool(
    "register_worktree",
    {
      title: "Register a worktree",
      description:
        "Register a checkout the runtime has not seen, assigning it a stable port offset. Use after creating a git worktree so its services do not collide with the main checkout.",
      inputSchema: {
        project: z.string().describe(PROJECT_DESCRIPTION),
        path: z.string().describe("Absolute path to the checkout."),
      },
    },
    async ({ project, path }) =>
      run("register_worktree", { selector: project, path }, (body) =>
        body.type === "workspace"
          ? formatWorktrees([body])
          : unexpected(body),
      ),
  );
}

function unexpected(body: ResponseBody): string {
  return `Unexpected response from the runtime daemon: ${body.type}`;
}

main().catch((error: unknown) => {
  process.stderr.write(`local-runtime MCP server failed to start: ${String(error)}\n`);
  process.exit(1);
});

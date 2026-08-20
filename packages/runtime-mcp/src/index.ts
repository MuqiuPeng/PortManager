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
  formatStart,
  formatWorktrees,
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

  const server = new McpServer({
    name: "local-runtime",
    version: "0.1.0",
  });

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
        "Start a service. Already-running services are returned as-is rather than started twice. If the preferred port is taken by another project, the runtime allocates the next free one and says so.",
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
    "stop_service",
    {
      title: "Stop service",
      description:
        "Stop a service and every process it spawned. Terminates gracefully first, then forcefully if it does not exit in time.",
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
        "Stop a service, wait for the whole process tree to exit, then start it again. Follow with wait_until_healthy to confirm it is serving.",
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
    "adopt_port",
    {
      title: "Take control of a port",
      description:
        "Declare whatever is already listening on a port as a service, so it can be stopped and started from here afterwards. The command is read off the running process, never guessed from package.json — a project whose dev and start scripts share a build directory is left unable to boot if it is adopted under the wrong one. Nothing is stopped or restarted. Refuses when another supervisor (PM2, systemd) is keeping the service alive, because taking it over means removing it from there, which usually changes what starts at boot; pass force to declare it anyway.",
      inputSchema: {
        port: z.number().int().min(1).max(65535),
        force: z
          .boolean()
          .optional()
          .describe("Declare it even though another supervisor keeps it alive."),
      },
    },
    async ({ port, force }) =>
      run("adopt_port", { port, force: force ?? false }, (body) =>
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

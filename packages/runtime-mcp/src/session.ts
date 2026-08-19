import type { AgentSession, ResponseBody } from "./protocol.js";
import type { DaemonClient } from "./client.js";

/**
 * Which agent is on the other end of this stdio pipe.
 *
 * This is what lets the runtime show "● api :8000 feature/refund — Codex" and
 * lets two agents share a machine without fighting over ports. MCP has no
 * standard way to identify the client, so this is best-effort: explicit
 * configuration first, then environment markers the known clients set.
 */
export interface AgentIdentity {
  provider: string;
  client: string;
}

export function detectAgent(argv: string[]): AgentIdentity {
  const explicitClient = readFlag(argv, "--client") ?? process.env.RUNTIME_MCP_CLIENT;
  const explicitProvider = readFlag(argv, "--provider") ?? process.env.RUNTIME_MCP_PROVIDER;

  const client = explicitClient ?? sniffClient();
  return {
    client,
    provider: explicitProvider ?? providerFor(client),
  };
}

function sniffClient(): string {
  if (process.env.CLAUDECODE || process.env.CLAUDE_CODE_ENTRYPOINT) return "claude-code";
  if (process.env.CURSOR_TRACE_ID || process.env.CURSOR_AGENT) return "cursor";
  if (process.env.CODEX_SANDBOX || process.env.CODEX_HOME) return "codex";
  return "unknown";
}

function providerFor(client: string): string {
  switch (client) {
    case "claude-code":
      return "anthropic";
    case "codex":
      return "openai";
    default:
      return "unknown";
  }
}

function readFlag(argv: string[], flag: string): string | undefined {
  const index = argv.indexOf(flag);
  if (index !== -1 && argv[index + 1]) return argv[index + 1];
  const inline = argv.find((arg) => arg.startsWith(`${flag}=`));
  return inline?.slice(flag.length + 1);
}

/**
 * Register this connection with the daemon.
 *
 * Failure is not fatal: ownership attribution is valuable, but an agent that
 * cannot register should still be able to restart a service.
 */
export async function registerSession(
  client: DaemonClient,
  identity: AgentIdentity,
): Promise<AgentSession | null> {
  try {
    const response = (await client.call("register_session", {
      provider: identity.provider,
      client: identity.client,
      cwd: process.cwd(),
    })) as ResponseBody;
    return response.type === "session" ? response : null;
  } catch (error) {
    process.stderr.write(
      `local-runtime: could not register agent session (${(error as Error).message})\n`,
    );
    return null;
  }
}

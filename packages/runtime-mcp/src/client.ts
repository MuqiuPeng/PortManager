import { spawnSync } from "node:child_process";
import net from "node:net";

import type { Frame, Request, ResponseBody } from "./protocol.js";

/**
 * A client for the runtime daemon's local IPC.
 *
 * Newline-delimited JSON over a Unix domain socket, or a named pipe on Windows.
 * Node speaks both through `net.connect`, so there is nothing platform-specific
 * here beyond the endpoint name.
 */
export class DaemonClient {
  private socket: net.Socket | null = null;
  private buffer = "";
  private nextId = 1;
  private readonly pending = new Map<
    number,
    { resolve: (body: ResponseBody) => void; reject: (error: Error) => void }
  >();

  constructor(private readonly endpoint: string) {}

  async call(method: string, params?: Record<string, unknown>): Promise<ResponseBody> {
    const socket = await this.connect();
    const id = this.nextId++;
    const request: Request = params ? { method, params } : { method };

    return new Promise<ResponseBody>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      socket.write(`${JSON.stringify({ kind: "request", id, request })}\n`, (error) => {
        if (error) {
          this.pending.delete(id);
          reject(error);
        }
      });
    });
  }

  close(): void {
    this.socket?.destroy();
    this.socket = null;
  }

  private async connect(): Promise<net.Socket> {
    if (this.socket && !this.socket.destroyed) return this.socket;

    const socket = await new Promise<net.Socket>((resolve, reject) => {
      const pending = net.connect({ path: this.endpoint });
      pending.once("connect", () => resolve(pending));
      pending.once("error", reject);
    });

    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => this.consume(chunk));
    // A dropped connection must fail every in-flight call; otherwise a tool
    // invocation hangs until the agent gives up.
    socket.on("close", () => this.failAll(new Error("the runtime daemon closed the connection")));
    socket.on("error", (error) => this.failAll(error));

    this.socket = socket;
    return socket;
  }

  private consume(chunk: string): void {
    this.buffer += chunk;
    let newline = this.buffer.indexOf("\n");
    while (newline !== -1) {
      const line = this.buffer.slice(0, newline).trim();
      this.buffer = this.buffer.slice(newline + 1);
      if (line) this.dispatch(line);
      newline = this.buffer.indexOf("\n");
    }
  }

  private dispatch(line: string): void {
    let frame: Frame;
    try {
      frame = JSON.parse(line) as Frame;
    } catch {
      return; // not our frame; ignore rather than tearing the connection down
    }

    if (frame.kind === "response") {
      this.pending.get(frame.id)?.resolve(frame.result);
      this.pending.delete(frame.id);
    } else if (frame.kind === "error") {
      // The daemon renders the message; the structured form is there for
      // clients that want to branch on `error.code`.
      this.pending.get(frame.id)?.reject(new Error(frame.message || describeError(frame.error)));
      this.pending.delete(frame.id);
    }
    // Event frames are ignored: this server never subscribes.
  }

  private failAll(error: Error): void {
    for (const { reject } of this.pending.values()) reject(error);
    this.pending.clear();
    this.socket = null;
  }
}

/**
 * The daemon serialises its errors as a tagged object; render the useful
 * fields rather than dumping JSON at the agent.
 */
function describeError(error: Record<string, unknown>): string {
  const parts = Object.entries(error)
    .filter(([key]) => key !== "code")
    .map(([key, value]) => `${key}: ${value}`);
  const code = String(error.code ?? "error");
  return parts.length > 0 ? `${code} (${parts.join(", ")})` : code;
}

/**
 * Find the daemon's endpoint, starting it if necessary.
 *
 * Rather than reimplementing the data-directory and socket-path rules in a
 * second language — where they would drift — this asks the `runtime` CLI,
 * which shares the code that decides them. `runtime daemon start` is
 * idempotent and reports the endpoint either way.
 */
export function resolveEndpoint(cliPath = "runtime"): string {
  const override = process.env.LOCAL_RUNTIME_SOCKET?.trim();
  if (override) return override;

  const result = spawnSync(cliPath, ["daemon", "start", "--json"], {
    encoding: "utf8",
    timeout: 20_000,
  });

  if (result.error || result.status !== 0) {
    const detail = result.stderr?.trim() || result.error?.message || "unknown error";
    throw new Error(
      `cannot reach the local runtime daemon: ${detail}\n` +
        `Install the runtime CLI and make sure \`${cliPath}\` is on PATH, ` +
        `or set LOCAL_RUNTIME_SOCKET to the daemon's endpoint.`,
    );
  }

  const info = JSON.parse(result.stdout) as { socket_path?: string };
  if (!info.socket_path) {
    throw new Error("the runtime CLI did not report a socket path");
  }
  return info.socket_path;
}

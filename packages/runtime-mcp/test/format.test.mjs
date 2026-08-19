// Pure-function tests. The IPC path is exercised end to end against a real
// daemon (see docs/mcp.md); what is worth pinning here is the rendering, since
// that is what an agent actually reads.

import assert from "node:assert/strict";
import { test } from "node:test";

import { formatLogs, formatPortStatus, formatService, formatStart } from "../dist/format.js";
import { detectAgent } from "../dist/session.js";

test("a running service reports its port, owner and pid", () => {
  const line = formatService({
    id: "svc-1",
    workspace_id: "ws-1",
    name: "web",
    service_type: "web",
    command: "pnpm dev",
    cwd: "/repo",
    status: "healthy",
    actual_port: 3004,
    instance: { id: "i", service_id: "svc-1", pid: 42, status: "healthy", started_at: "", started_by: "claude-code" },
  });
  assert.match(line, /web :3004 healthy, started by claude-code, pid 42/);
  assert.match(line, /\[id svc-1\]/);
});

test("a stopped service does not report the last run's pid or owner", () => {
  const line = formatService({
    id: "svc-1",
    workspace_id: "ws-1",
    name: "web",
    service_type: "web",
    command: "pnpm dev",
    cwd: "/repo",
    status: "stopped",
    // The daemon still returns the previous instance; showing it would read as
    // though the service were running.
    instance: { id: "i", service_id: "svc-1", pid: 42, status: "stopped", started_at: "", started_by: "cli" },
  });
  assert.equal(line, "web no port stopped [id svc-1]");
});

test("a reallocated port says which port was wanted", () => {
  const text = formatStart({
    reused: false,
    service: { id: "s", workspace_id: "w", name: "web", service_type: "web", command: "", cwd: "", status: "starting" },
    reservation: { port: 3004, preferred_port: 3000, reallocated: true, policy: "allocate-next" },
  });
  assert.match(text, /port 3004 \(preferred 3000 was taken\)/);
});

test("an occupied port names its holder and an alternative", () => {
  const text = formatPortStatus({
    port: 3000,
    available: false,
    suggested_port: 3005,
    owner: { port: 3000, pid: 129, managed: false, cwd: "/repo", project_name: "dossh", git_branch: "main", service_name: "web" },
  });
  assert.match(text, /dossh\/main\/web/);
  assert.match(text, /Suggested alternative: 3005/);
  assert.match(text, /not managed by the runtime/);
});

test("logs end with a cursor for the next call", () => {
  const text = formatLogs([
    { seq: 4, service_id: "s", stream: "stdout", timestamp: "2026-08-19T10:28:57Z", message: "listening on 3004" },
    { seq: 5, service_id: "s", stream: "stderr", timestamp: "2026-08-19T10:28:58Z", message: "warn" },
  ]);
  // stdout lines carry a blank marker, so the message sits three spaces in.
  assert.match(text, /10:28:57 {3}listening on 3004/);
  assert.match(text, /! warn/);
  assert.match(text, /\(next cursor: 5\)/);
});

test("an explicit --client flag wins over environment sniffing", () => {
  assert.deepEqual(detectAgent(["--client", "codex"]), { client: "codex", provider: "openai" });
  assert.deepEqual(detectAgent(["--client=cursor", "--provider=acme"]), {
    client: "cursor",
    provider: "acme",
  });
});

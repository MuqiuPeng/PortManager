import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

import type {
  DaemonInfo,
  LogLine,
  PortOwner,
  ProjectView,
  RuntimeEvent,
  ServiceView,
  StartOutcome,
} from "./types";

/** The channel `lib.rs` re-emits daemon events on. */
const EVENT_CHANNEL = "runtime://event";

export const api = {
  listProjects: () => invoke<ProjectView[]>("list_projects"),

  addProject: (path: string, name?: string) =>
    invoke<ProjectView>("add_project", { path, name: name || null }),

  removeProject: (selector: string) =>
    invoke<boolean>("remove_project", { selector }),

  getService: (service: string) => invoke<ServiceView>("get_service", { service }),

  startService: (service: string) =>
    invoke<StartOutcome>("start_service", { service }),

  stopService: (service: string) =>
    invoke<ServiceView>("stop_service", { service }),

  restartService: (service: string) =>
    invoke<StartOutcome>("restart_service", { service }),

  getLogs: (service: string, maxLines = 300, sinceSeq?: number) =>
    invoke<LogLine[]>("get_logs", {
      service,
      maxLines,
      sinceSeq: sinceSeq ?? null,
    }),

  listPorts: () => invoke<PortOwner[]>("list_ports"),

  daemonInfo: () => invoke<DaemonInfo>("daemon_info"),
};

/**
 * Subscribe to runtime events.
 *
 * This is what makes a service started from the CLI, or by an agent, appear
 * here without the window polling for it.
 */
export function onRuntimeEvent(handler: (event: RuntimeEvent) => void) {
  return listen<RuntimeEvent>(EVENT_CHANNEL, (message) => handler(message.payload));
}

/** Open a service's URL in the user's browser, not in the app's webview. */
export function openExternal(url: string) {
  return openUrl(url);
}

/** Tauri surfaces command errors as unknown; normalise them for display. */
export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

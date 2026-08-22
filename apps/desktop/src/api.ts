import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";

import type {
  AdoptOutcome,
  ContainerView,
  DaemonInfo,
  Discovery,
  Failure,
  Finding,
  LogLine,
  PanelSettings,
  PanelState,
  PortOwner,
  ProjectView,
  RuntimeEvent,
  ScreenInfo,
  ServicePatch,
  ServiceView,
  StartOutcome,
  Workspace,
  SupervisedView,
  StackView,
} from "./types";

/** The channel `lib.rs` re-emits daemon events on. */
const EVENT_CHANNEL = "runtime://event";

/** The channel the panel controller announces its size on. */
const PANEL_STATE_CHANNEL = "panel://state";

export const api = {
  listProjects: () => invoke<ProjectView[]>("list_projects"),

  discoverProjects: (paths: string[] = [], adopt = false) =>
    invoke<Discovery[]>("discover_projects", { paths, adopt }),

  addProject: (path: string, name?: string) =>
    invoke<ProjectView>("add_project", { path, name: name || null }),

  removeProject: (selector: string) =>
    invoke<boolean>("remove_project", { selector }),

  getService: (service: string) => invoke<ServiceView>("get_service", { service }),

  adoptPort: (port: number, force = false) =>
    invoke<AdoptOutcome>("adopt_port", { port, force }),

  controlSupervised: (name: string, action: "start" | "stop" | "restart") =>
    invoke<SupervisedView>("control_supervised", { name, action }),

  diagnose: () => invoke<Finding[]>("diagnose"),

  listFailures: (lines = 8) => invoke<Failure[]>("list_failures", { lines }),

  registerWorktree: (selector: string, path: string) =>
    invoke<Workspace>("register_worktree", { selector, path }),

  listStacks: (project: string) => invoke<StackView[]>("list_tasks", { project }),

  setStack: (project: string, name: string, steps: string[]) =>
    invoke<StackView[]>("set_task", { project, name, steps }),

  removeStack: (project: string, name: string) =>
    invoke<boolean>("remove_task", { project, name }),

  runStack: (project: string, name: string) =>
    invoke<string[]>("run_task", { project, name }),

  stopStack: (project: string, name: string) =>
    invoke<string[]>("stop_task", { project, name }),

  updateService: (service: string, patch: ServicePatch) =>
    invoke<ServiceView>("update_service", { service, patch }),

  addService: (project: string, name: string, command: string, port?: number, cwd?: string) =>
    invoke<ServiceView>("add_service", {
      project,
      name,
      command,
      port: port ?? null,
      cwd: cwd || null,
    }),

  removeService: (service: string) => invoke<boolean>("remove_service", { service }),

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

  controlContainer: (name: string, action: "start" | "stop" | "restart") =>
    invoke<ContainerView>("control_container", { name, action }),

  listPorts: () => invoke<PortOwner[]>("list_ports"),

  daemonInfo: () => invoke<DaemonInfo>("daemon_info"),

  getPanelSettings: () => invoke<PanelSettings>("get_panel_settings"),

  setPanelSettings: (settings: PanelSettings) =>
    invoke<void>("set_panel_settings", { settings }),

  listScreens: () => invoke<ScreenInfo[]>("list_screens"),

  hidePanel: () => invoke<void>("hide_panel"),

  openMainWindow: () => invoke<void>("open_main_window"),
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

/**
 * Follow the panel between its tab and expanded sizes.
 *
 * Pushed rather than polled: the native window is resizing either way, and the
 * content has to change in the same frame to avoid a flash of the wrong layout.
 */
export function onPanelState(handler: (state: PanelState) => void) {
  return listen<PanelState>(PANEL_STATE_CHANNEL, (message) => handler(message.payload));
}

/**
 * Open a service's URL in the user's browser, not in the app's webview.
 *
 * Rejects when the URL falls outside the capability's scope, which callers must
 * surface — a silently swallowed rejection here reads as a dead button.
 */
export function openExternal(url: string) {
  return openUrl(url);
}

/**
 * Ask for a folder through the system picker.
 *
 * Null when the person closed it without choosing. A path typed by hand is a
 * path nobody has checked: this way the folder is known to exist, and its
 * owner could see where they were while picking it.
 */
export async function chooseFolder(startingAt?: string): Promise<string | null> {
  const chosen = await openDialog({ directory: true, multiple: false, defaultPath: startingAt });
  return typeof chosen === "string" ? chosen : null;
}

/** Tauri surfaces command errors as unknown; normalise them for display. */
export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

/**
 * Put text on the clipboard.
 *
 * Through the plugin rather than `navigator.clipboard`: the webview serves the
 * app from a custom scheme, which is not a secure context, and the browser
 * clipboard API is simply absent there. That is the same shape as `prompt`,
 * which this app already learned about the hard way — the call does not fail,
 * it is not there at all.
 */
export async function copyText(text: string): Promise<void> {
  await writeText(text);
}

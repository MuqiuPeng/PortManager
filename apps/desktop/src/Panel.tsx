import { useCallback, useEffect, useMemo, useState } from "react";

import { api, copyText, errorMessage, onPanelState, onRuntimeEvent, openExternal } from "./api";
import {
  isLive,
  type Failure,
  type PanelState,
  type ProjectView,
  type ServiceView,
  type TaskView,
} from "./types";

/**
 * The edge panel, in both of its sizes.
 *
 * At rest it is a tab: a few status dots, readable in the corner of your eye.
 * Expanded it is one glance and one click — what is running, on which port, and
 * start / stop / open. Logs, ports and project management stay in the main
 * window; a panel that grew a second screen would just be a small main window.
 */
export interface PanelGroup {
  project: ProjectView;
  branch: string;
  task: TaskView;
}

export interface PanelService {
  project: ProjectView;
  branch: string;
  service: ServiceView;
}

/**
 * Split what the panel shows into groups and everything else.
 *
 * Membership is per checkout: a group belongs to the checkout it was declared
 * in, and a service is only inside it if it is named there. Two checkouts of
 * the same project have services of the same name, so asking the question
 * across a project would file one branch's service under the other's group.
 *
 * Running first, because the panel is opened to check on something that is up
 * or to bring something up, and by name after that so it does not shuffle
 * under the pointer while things start.
 */
export function partition(projects: ProjectView[]): {
  groups: PanelGroup[];
  loose: PanelService[];
} {
  const groups: PanelGroup[] = [];
  const loose: PanelService[] = [];

  for (const project of projects) {
    for (const workspace of project.workspaces) {
      const branch = workspace.git_branch ?? "";
      const tasks = workspace.tasks ?? [];
      for (const task of tasks) {
        groups.push({ project, branch, task });
      }
      const grouped = new Set(tasks.flatMap((task) => task.steps));
      for (const service of workspace.services) {
        if (grouped.has(service.name)) continue;
        loose.push({ project, branch, service });
      }
    }
  }

  groups.sort((a, b) => {
    const live = Number(b.task.running > 0) - Number(a.task.running > 0);
    return live !== 0 ? live : a.task.name.localeCompare(b.task.name);
  });
  loose.sort((a, b) => {
    const live = Number(isLive(b.service.status)) - Number(isLive(a.service.status));
    return live !== 0 ? live : a.service.name.localeCompare(b.service.name);
  });
  return { groups, loose };
}

export default function Panel() {
  const [state, setState] = useState<PanelState>("island");
  const [projects, setProjects] = useState<ProjectView[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pinned, setPinned] = useState(false);
  const [edge, setEdge] = useState<"left" | "right">("right");

  const refresh = useCallback(async () => {
    try {
      setProjects(await api.listProjects());
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
    void api.getPanelSettings().then((settings) => {
      setPinned(settings.pinned);
      setEdge(settings.edge);
    });
  }, [refresh]);

  useEffect(() => {
    const unlisten = onRuntimeEvent(() => void refresh());
    return () => void unlisten.then((stop) => stop());
  }, [refresh]);

  useEffect(() => {
    const unlisten = onPanelState(setState);
    return () => void unlisten.then((stop) => stop());
  }, []);

  // Escape collapses the panel, the same as moving the pointer away.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") void api.hidePanel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  /**
   * Groups first, then whatever belongs to none of them.
   *
   * Flattened across projects, because the panel is a glance at the machine
   * rather than a project tree — but a declared group is one thing on it, the
   * same as everywhere else. Five services somebody grouped into one stack
   * were five rows and five clicks here, which is the arithmetic the group was
   * declared to stop having to do.
   */
  const { groups, loose } = useMemo(() => partition(projects), [projects]);

  /** Every service, whichever way it is filed — for the resting dots. */
  const rows = useMemo(
    () =>
      projects.flatMap((project) =>
        project.workspaces.flatMap((workspace) =>
          workspace.services.map((service) => ({
            project,
            branch: workspace.git_branch ?? "",
            service,
          })),
        ),
      ),
    [projects],
  );

  const running = rows.filter((row) => isLive(row.service.status));

  /** Groups whose members are showing. */
  const [opened, setOpened] = useState<string[]>([]);

  // Fetched but not displayed. The panel is a glance, and an error message is
  // not glanceable — what it is good for here is being carried somewhere else,
  // so the row offers to copy it and says nothing more.
  const [failures, setFailures] = useState<Failure[]>([]);
  const [copied, setCopied] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const broken = await api.listFailures(40);
        if (!cancelled) setFailures(broken);
      } catch {
        if (!cancelled) setFailures([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projects]);

  async function copyFailure(failure: Failure) {
    const code = failure.exit_code === undefined ? "" : ` (exit ${failure.exit_code})`;
    const text = [
      `${failure.subject} — ${failure.status}${code}`,
      ...(failure.detail ?? []),
    ].join("\n");
    try {
      await copyText(text);
      setCopied(failure.service_id);
      window.setTimeout(() => setCopied(null), 1500);
    } catch {
      // Nothing worth saying in a panel this size, and nowhere to say it.
    }
  }

  async function act(id: string, action: () => Promise<unknown>) {
    setBusy(id);
    setError(null);
    try {
      await action();
      await refresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(null);
    }
  }

  async function togglePinned() {
    const next = !pinned;
    setPinned(next);
    const settings = await api.getPanelSettings();
    await api.setPanelSettings({ ...settings, pinned: next });
  }

  /** One service, the same row whether it is loose or inside a group. */
  function serviceRow(project: ProjectView, branch: string, service: ServiceView) {
    const live = isLive(service.status);
    const id = service.id;
    const failed = failures.find((failure) => failure.service_id === id);
    return (
      <div className="panel-row" key={id}>
        <span className={`dot status-${service.status}`} aria-hidden />

        <div className="panel-row-body">
          <div className="panel-row-title">
            <span className="panel-service">{service.name}</span>
            <span className="panel-project">{project.name}</span>
          </div>
          <div className="panel-row-meta">
            {service.actual_port ? `:${service.actual_port}` : service.status}
            {branch && ` · ${branch}`}
          </div>
        </div>

        <div className="panel-row-actions">
          {failed && (
            <button
              className="icon-button"
              title={copied === id ? "Copied" : "Copy why this failed"}
              onClick={() => void copyFailure(failed)}
            >
              {copied === id ? "✓" : "⧉"}
            </button>
          )}
          {live && service.url && (
            <button
              className="icon-button"
              title={`Open ${service.url}`}
              onClick={() => act(id, () => openExternal(service.url as string))}
            >
              ↗
            </button>
          )}
          <button
            className="icon-button"
            disabled={busy === id}
            title={live ? "Stop" : "Start"}
            onClick={() => act(id, () => (live ? api.stopService(id) : api.startService(id)))}
          >
            {live ? "■" : "▶"}
          </button>
        </div>
      </div>
    );
  }

  if (state === "island") {
    return <Island running={running.length} total={rows.length} edge={edge} />;
  }

  return (
    <div className="panel expanded">
      <header className="panel-head">
        <span className="panel-title">Local Runtime</span>
        <span className="panel-count">{running.length} running</span>
        <button
          className={pinned ? "icon-button active" : "icon-button"}
          onClick={() => void togglePinned()}
          title={pinned ? "Unpin" : "Keep open"}
        >
          {pinned ? "◉" : "○"}
        </button>
      </header>

      {error && <div className="panel-error">{error}</div>}

      <div className="panel-body">
        {rows.length === 0 ? (
          <p className="empty">
            Nothing registered yet. Open the main window to find your projects.
          </p>
        ) : (
          <>
            {groups.map(({ project, branch, task }) => {
              const key = `${project.id}/${task.name}`;
              const total = task.steps.length;
              const allUp = total > 0 && task.running === total;
              const someUp = task.running > 0;
              const open = opened.includes(key);
              // Whichever member broke. What somebody wants to carry away is
              // the reason, not which of five names it came from.
              const broken = task.services
                .map((member) => failures.find((one) => one.service_id === member.id))
                .find(Boolean);
              return (
                <div className="panel-group" key={key}>
                  <div className="panel-row">
                    <span
                      className={allUp ? "dot status-healthy" : someUp ? "dot partial" : "dot"}
                      aria-hidden
                    />

                    <button
                      className="panel-row-body as-button"
                      onClick={() =>
                        setOpened(open ? opened.filter((one) => one !== key) : [...opened, key])
                      }
                      aria-expanded={open}
                      title={open ? "Hide its services" : "Show its services"}
                    >
                      <div className="panel-row-title">
                        <span className="panel-caret">{open ? "▾" : "▸"}</span>
                        <span className="panel-service">{task.name}</span>
                        <span className="panel-project">{project.name}</span>
                      </div>
                      <div className="panel-row-meta">
                        {task.running}/{total} up{branch && ` · ${branch}`}
                      </div>
                    </button>

                    <div className="panel-row-actions">
                      {broken && (
                        <button
                          className="icon-button"
                          title={copied === broken.service_id ? "Copied" : "Copy why this failed"}
                          onClick={() => void copyFailure(broken)}
                        >
                          {copied === broken.service_id ? "✓" : "⧉"}
                        </button>
                      )}
                      <button
                        className="icon-button"
                        disabled={busy === key}
                        title={someUp ? "Stop the group" : "Start the group"}
                        onClick={() =>
                          act(key, () =>
                            someUp
                              ? api.stopTask(project.id, task.name)
                              : api.runTask(project.id, task.name),
                          )
                        }
                      >
                        {someUp ? "■" : "▶"}
                      </button>
                    </div>
                  </div>

                  {open && (
                    <div className="panel-members">
                      {task.services.map((member) => serviceRow(project, branch, member))}
                    </div>
                  )}
                </div>
              );
            })}

            {groups.length > 0 && loose.length > 0 && (
              <div className="panel-divider">Ungrouped</div>
            )}

            {loose.map(({ project, branch, service }) => serviceRow(project, branch, service))}
          </>
        )}
      </div>

      <footer className="panel-foot">
        <button className="link" onClick={() => void api.openMainWindow()}>
          Open main window
        </button>
      </footer>
    </div>
  );
}

/**
 * The resting tab.
 *
 * Only as much as reads at a glance from the corner of a screen: one dot per
 * running service, up to a handful, then a count.
 */
function Island({
  running,
  total,
  edge,
}: {
  running: number;
  total: number;
  edge: "left" | "right";
}) {
  const DOTS = 4;
  const shown = Math.min(running, DOTS);

  // The tab is rounded on the inward side only, which depends on which edge it
  // is docked to.
  const className = ["island", `edge-${edge}`, running > 0 ? "active" : ""]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={className} title={`${running}/${total} running`}>
      {running === 0 ? (
        <span className="island-dot idle" />
      ) : (
        <>
          {Array.from({ length: shown }, (_, index) => (
            <span className="island-dot" key={index} />
          ))}
          {running > DOTS && <span className="island-more">+{running - DOTS}</span>}
        </>
      )}
    </div>
  );
}

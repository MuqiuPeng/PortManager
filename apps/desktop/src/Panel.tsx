import { useCallback, useEffect, useMemo, useState } from "react";

import { api, copyText, errorMessage, onPanelState, onRuntimeEvent, openExternal } from "./api";
import {
  isLive,
  type Failure,
  type PanelState,
  type ProjectView,
  type ServiceView,
} from "./types";

/**
 * The edge panel, in both of its sizes.
 *
 * At rest it is a tab: a few status dots, readable in the corner of your eye.
 * Expanded it is one glance and one click — what is running, on which port, and
 * start / stop / open. Logs, ports and project management stay in the main
 * window; a panel that grew a second screen would just be a small main window.
 */
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

  /** Flattened, because the panel shows services rather than a project tree. */
  const rows = useMemo(() => {
    const out: { project: ProjectView; branch: string; service: ServiceView }[] = [];
    for (const project of projects) {
      for (const workspace of project.workspaces) {
        for (const service of workspace.services) {
          out.push({ project, branch: workspace.git_branch ?? "", service });
        }
      }
    }
    // Running first: the panel is opened to check on something that is up, or
    // to bring something up.
    return out.sort((a, b) => {
      const live = Number(isLive(b.service.status)) - Number(isLive(a.service.status));
      return live !== 0 ? live : a.project.name.localeCompare(b.project.name);
    });
  }, [projects]);

  const running = rows.filter((row) => isLive(row.service.status));

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
          rows.map(({ project, branch, service }) => {
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
                      title={
                        copied === id
                          ? "Copied"
                          : "Copy why this failed"
                      }
                      onClick={() => void copyFailure(failed)}
                    >
                      {copied === id ? "✓" : "⧉"}
                    </button>
                  )}
                  {live && service.url && (
                    <button
                      className="icon-button"
                      title={`Open ${service.url}`}
                      onClick={() =>
                        act(id, () => openExternal(service.url as string))
                      }
                    >
                      ↗
                    </button>
                  )}
                  <button
                    className="icon-button"
                    disabled={busy === id}
                    title={live ? "Stop" : "Start"}
                    onClick={() =>
                      act(id, () => (live ? api.stopService(id) : api.startService(id)))
                    }
                  >
                    {live ? "■" : "▶"}
                  </button>
                </div>
              </div>
            );
          })
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

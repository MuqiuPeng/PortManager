import { useCallback, useEffect, useMemo, useState } from "react";

import { api, errorMessage, onRuntimeEvent, openExternal } from "./api";
import { isLive, type ProjectView, type ServiceView } from "./types";

/**
 * The compact panel.
 *
 * Everything here is one glance and one click: what is running, on which port,
 * and start / stop / open. Logs, ports and project management stay in the main
 * window — a panel that grew a second screen would just be a small main window.
 */
export default function Panel() {
  const [projects, setProjects] = useState<ProjectView[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pinned, setPinned] = useState(false);

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
    void api.getPanelConfig().then((config) => setPinned(config.pinned));
  }, [refresh]);

  useEffect(() => {
    const unlisten = onRuntimeEvent(() => void refresh());
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, [refresh]);

  // Escape dismisses the panel, the same as clicking away from it.
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
          out.push({
            project,
            branch: workspace.git_branch ?? "",
            service,
          });
        }
      }
    }
    // Running first: the panel is opened to check on something that is up, or
    // to bring something up.
    return out.sort((a, b) => {
      const live = Number(isLive(b.service.status)) - Number(isLive(a.service.status));
      if (live !== 0) return live;
      return a.project.name.localeCompare(b.project.name);
    });
  }, [projects]);

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
    const config = await api.getPanelConfig();
    await api.setPanelConfig({ ...config, pinned: next });
  }

  const runningCount = rows.filter((row) => isLive(row.service.status)).length;

  return (
    <div className="panel">
      <header className="panel-head">
        <span className="panel-title">Local Runtime</span>
        <span className="panel-count">{runningCount} running</span>
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
                  {live && service.url && (
                    <button
                      className="icon-button"
                      title={`Open ${service.url}`}
                      onClick={() => service.url && openExternal(service.url)}
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

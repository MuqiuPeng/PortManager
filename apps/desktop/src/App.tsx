import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, errorMessage, onRuntimeEvent, openExternal } from "./api";
import { LogPanel } from "./components/LogPanel";
import { PortTable } from "./components/PortTable";
import { ProjectList } from "./components/ProjectList";
import { ServiceRow } from "./components/ServiceRow";
import type { LogLine, PortOwner, ProjectView, ServiceView } from "./types";

type Tab = "services" | "ports";

/** Ports change without any event of their own, so that tab polls. */
const PORT_POLL_MS = 4000;
const LOG_POLL_MS = 800;

export default function App() {
  const [projects, setProjects] = useState<ProjectView[]>([]);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [selectedService, setSelectedService] = useState<string | null>(null);
  const [ports, setPorts] = useState<PortOwner[]>([]);
  const [logs, setLogs] = useState<LogLine[]>([]);
  const [tab, setTab] = useState<Tab>("services");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const logCursor = useRef<number | undefined>(undefined);

  const refreshProjects = useCallback(async () => {
    try {
      const next = await api.listProjects();
      setProjects(next);
      setError(null);
      setSelectedProject((current) => {
        if (current && next.some((project) => project.id === current)) return current;
        return next[0]?.id ?? null;
      });
    } catch (err) {
      setError(errorMessage(err));
    }
  }, []);

  useEffect(() => {
    void refreshProjects();
  }, [refreshProjects]);

  // Live updates: anything the daemon does — from this window, the CLI or an
  // agent — lands here without polling.
  useEffect(() => {
    const unlisten = onRuntimeEvent(() => {
      void refreshProjects();
    });
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, [refreshProjects]);

  useEffect(() => {
    if (tab !== "ports") return;
    let cancelled = false;

    const poll = async () => {
      try {
        const next = await api.listPorts();
        if (!cancelled) setPorts(next);
      } catch (err) {
        if (!cancelled) setError(errorMessage(err));
      }
    };

    void poll();
    const timer = setInterval(poll, PORT_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [tab]);

  // Logs are pulled with a cursor so each tick transfers only what is new.
  useEffect(() => {
    if (!selectedService) {
      setLogs([]);
      logCursor.current = undefined;
      return;
    }

    let cancelled = false;
    logCursor.current = undefined;
    setLogs([]);

    const poll = async () => {
      try {
        const incoming = await api.getLogs(selectedService, 300, logCursor.current);
        if (cancelled || incoming.length === 0) return;
        logCursor.current = incoming[incoming.length - 1].seq;
        setLogs((current) => [...current, ...incoming].slice(-500));
      } catch (err) {
        if (!cancelled) setError(errorMessage(err));
      }
    };

    void poll();
    const timer = setInterval(poll, LOG_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [selectedService]);

  const project = useMemo(
    () => projects.find((candidate) => candidate.id === selectedProject) ?? null,
    [projects, selectedProject],
  );

  const selectedServiceView = useMemo(() => {
    if (!project || !selectedService) return null;
    for (const workspace of project.workspaces) {
      const found = workspace.services.find((service) => service.id === selectedService);
      if (found) return found;
    }
    return null;
  }, [project, selectedService]);

  async function act(action: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    try {
      await action();
      await refreshProjects();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function handleAddProject() {
    // A directory picker is Phase 5 polish; a path is enough to prove the
    // registry end to end.
    const path = window.prompt("Project path");
    if (!path?.trim()) return;
    await act(() => api.addProject(path.trim()));
  }

  return (
    <div className="app">
      <header className="titlebar">
        <span className="brand">Local Runtime</span>
        <nav className="tabs">
          <button
            className={tab === "services" ? "tab active" : "tab"}
            onClick={() => setTab("services")}
          >
            Projects
          </button>
          <button
            className={tab === "ports" ? "tab active" : "tab"}
            onClick={() => setTab("ports")}
          >
            Ports
          </button>
        </nav>
      </header>

      {error && (
        <div className="banner" role="alert">
          {error}
          <button className="ghost" onClick={() => setError(null)}>
            Dismiss
          </button>
        </div>
      )}

      {tab === "ports" ? (
        <main className="ports-pane">
          <PortTable ports={ports} />
        </main>
      ) : (
        <div className="body">
          <ProjectList
            projects={projects}
            selectedId={selectedProject}
            onSelect={setSelectedProject}
            onAdd={handleAddProject}
            busy={busy}
          />

          <main className="detail">
            {!project ? (
              <p className="empty">Select a project.</p>
            ) : (
              <>
                <header className="detail-head">
                  <h1>{project.name}</h1>
                  <span className="path">{project.root_path}</span>
                </header>

                <div className="workspaces">
                  {project.workspaces.map((workspace) => (
                    <section className="workspace" key={workspace.id}>
                      <header className="workspace-head">
                        <span className="branch">
                          {workspace.git_branch ?? "(detached)"}
                        </span>
                        {workspace.worktree && (
                          <span className="badge">worktree +{workspace.port_offset}</span>
                        )}
                      </header>

                      {workspace.services.length === 0 ? (
                        <p className="empty">No services detected.</p>
                      ) : (
                        workspace.services.map((service: ServiceView) => (
                          <ServiceRow
                            key={service.id}
                            service={service}
                            selected={service.id === selectedService}
                            busy={busy}
                            onSelect={() => setSelectedService(service.id)}
                            onStart={() => act(() => api.startService(service.id))}
                            onStop={() => act(() => api.stopService(service.id))}
                            onRestart={() => act(() => api.restartService(service.id))}
                            onOpen={() => service.url && openExternal(service.url)}
                          />
                        ))
                      )}
                    </section>
                  ))}
                </div>

                <LogPanel
                  serviceName={selectedServiceView?.name ?? null}
                  lines={logs}
                />
              </>
            )}
          </main>
        </div>
      )}
    </div>
  );
}

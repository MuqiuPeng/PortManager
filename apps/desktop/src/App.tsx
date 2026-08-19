import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, errorMessage, onRuntimeEvent, openExternal } from "./api";
import { DiscoveryPanel } from "./components/DiscoveryPanel";
import { ContainerRow } from "./components/ContainerRow";
import { ExternalRow } from "./components/ExternalRow";
import { LogPanel } from "./components/LogPanel";
import { PortTable } from "./components/PortTable";
import { ProjectList } from "./components/ProjectList";
import { PromptSheet } from "./components/PromptSheet";
import { Settings } from "./components/Settings";
import { ServiceEditor } from "./components/ServiceEditor";
import { ServiceRow } from "./components/ServiceRow";
import type { Discovery, LogLine, PortOwner, ProjectView, ServiceView } from "./types";

type Tab = "services" | "ports" | "discover" | "settings";

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
  const [discoveries, setDiscoveries] = useState<Discovery[]>([]);
  const [scanning, setScanning] = useState(false);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState<ServiceView | null>(null);
  const [loaded, setLoaded] = useState(false);
  /** Which in-app prompt is open, if any. */
  const [prompt, setPrompt] = useState<"add-service" | "scan-folder" | null>(null);
  const [promptProject, setPromptProject] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const logCursor = useRef<number | undefined>(undefined);

  const refreshProjects = useCallback(async () => {
    try {
      const next = await api.listProjects();
      setProjects(next);
      setLoaded(true);
      setError(null);
      setSelectedProject((current) => {
        if (current && next.some((project) => project.id === current)) return current;
        return next[0]?.id ?? null;
      });
    } catch (err) {
      setError(errorMessage(err));
    }
  }, []);

  const scan = useCallback(async (paths: string[] = []) => {
    setScanning(true);
    try {
      setDiscoveries((await api.discoverProjects(paths)) ?? []);
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setScanning(false);
    }
  }, []);

  useEffect(() => {
    void refreshProjects();
  }, [refreshProjects]);

  // Nothing registered means the user has not told the runtime anything yet —
  // so find their projects rather than showing them an empty list and asking.
  //
  // Gated on the first load having *happened*: the list starts empty, so
  // without this every launch flashes the Discover tab before the projects
  // arrive.
  const autoScanned = useRef(false);
  useEffect(() => {
    if (!loaded || autoScanned.current || projects.length > 0) return;
    autoScanned.current = true;
    setTab("discover");
    void scan();
  }, [loaded, projects.length, scan]);

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

  async function handleAddDiscovered(discovery: Discovery) {
    await act(() => api.addProject(discovery.root_path));
    await scan();
  }

  async function handleAddAll() {
    await act(() => api.discoverProjects([], true));
    await scan();
  }

  /** Declare something detection did not find. */
  async function handleAddService(projectName: string, name: string, command: string) {
    await act(() => api.addService(projectName, name, command));
  }

  /** Scanning a folder finds projects that are not currently running. */
  async function handleScanFolder(path: string) {
    setTab("discover");
    await scan([path]);
  }

  return (
    <div className="app">
      {prompt === "add-service" && promptProject && (
        <PromptSheet
          title={`New service in ${promptProject}`}
          fields={[
            { label: "Name", placeholder: "worker" },
            { label: "Command", placeholder: "pnpm run worker", mono: true },
          ]}
          hint="Port, environment and the rest can be set afterwards with Edit."
          onCancel={() => setPrompt(null)}
          onConfirm={([name, command]) => {
            setPrompt(null);
            void handleAddService(promptProject, name, command);
          }}
        />
      )}

      {prompt === "scan-folder" && (
        <PromptSheet
          title="Scan a folder"
          confirmLabel="Scan"
          fields={[{ label: "Folder", placeholder: "/Users/you/code", mono: true }]}
          hint="Projects that are running are found without this; a folder scan also finds stopped ones."
          onCancel={() => setPrompt(null)}
          onConfirm={([path]) => {
            setPrompt(null);
            void handleScanFolder(path);
          }}
        />
      )}

      {editing && (
        <ServiceEditor
          service={editing}
          onClose={() => setEditing(null)}
          onSaved={() => void refreshProjects()}
        />
      )}
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
          <button
            className={tab === "discover" ? "tab active" : "tab"}
            onClick={() => {
              setTab("discover");
              if (discoveries.length === 0) void scan();
            }}
          >
            Discover
          </button>
          <button
            className={tab === "settings" ? "tab active" : "tab"}
            onClick={() => setTab("settings")}
          >
            Settings
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

      {tab === "settings" ? (
        <main className="ports-pane">
          <Settings />
        </main>
      ) : tab === "discover" ? (
        <main className="ports-pane">
          <DiscoveryPanel
            discoveries={discoveries}
            scanning={scanning}
            busy={busy}
            onAdd={handleAddDiscovered}
            onAddAll={handleAddAll}
            onRescan={() => void scan()}
            onAddByPath={() => setPrompt("scan-folder")}
          />
        </main>
      ) : tab === "ports" ? (
        <main className="ports-pane">
          <PortTable ports={ports} />
        </main>
      ) : (
        <div className="body">
          <ProjectList
            projects={projects}
            selectedId={selectedProject}
            onSelect={setSelectedProject}
            onAdd={() => {
              setTab("discover");
              void scan();
            }}
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
                        <span className="spacer" />
                        <button
                          className="ghost"
                          disabled={busy}
                          onClick={() => {
                            setPromptProject(project.name);
                            setPrompt("add-service");
                          }}
                        >
                          + Service
                        </button>
                      </header>

                      {workspace.services.length === 0 &&
                      (workspace.external ?? []).length === 0 &&
                      (workspace.containers ?? []).length === 0 ? (
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
                            onOpen={() =>
                              act(() => openExternal(service.url as string))
                            }
                            onEdit={() => setEditing(service)}
                          />
                        ))
                      )}

                      {(workspace.containers ?? []).map((container) => (
                        <ContainerRow
                          container={container}
                          busy={busy}
                          key={container.name}
                          onControl={(action) =>
                            act(() => api.controlContainer(container.name, action))
                          }
                        />
                      ))}

                      {(workspace.external ?? []).map((item) => (
                        <ExternalRow external={item} key={`${item.port}-${item.pid}`} />
                      ))}
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

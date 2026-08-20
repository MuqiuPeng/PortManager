import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, errorMessage, onRuntimeEvent, openExternal } from "./api";
import { DiscoveryPanel } from "./components/DiscoveryPanel";
import { ContainerRow } from "./components/ContainerRow";
import { ExternalRow } from "./components/ExternalRow";
import { FindingsBanner } from "./components/FindingsBanner";
import { LogPanel } from "./components/LogPanel";
import { PortTable } from "./components/PortTable";
import { ProjectList } from "./components/ProjectList";
import { PromptSheet } from "./components/PromptSheet";
import { Settings } from "./components/Settings";
import { ServiceEditor } from "./components/ServiceEditor";
import { ServiceRow } from "./components/ServiceRow";
import { SupervisedRow } from "./components/SupervisedRow";
import { TaskPanel } from "./components/TaskPanel";
import { TakeControlSheet } from "./components/TakeControlSheet";
import type {
  Discovery,
  Finding,
  LogLine,
  PortOwner,
  ProjectView,
  ServiceView,
  Task,
} from "./types";

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
  const [prompt, setPrompt] = useState<"add-service" | "scan-folder" | "add-task" | null>(null);
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

  /** Tasks for the selected project, reloaded whenever it changes. */
  const [tasks, setTasks] = useState<Task[]>([]);

  /** Problems with what is declared, across every project. */
  const [findings, setFindings] = useState<Finding[]>([]);
  const [findingsHidden, setFindingsHidden] = useState(false);

  // Re-run after anything that changes the registry, since that is when a
  // problem is introduced — and when the person is looking.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const found = await api.diagnose();
        if (!cancelled) setFindings(found);
      } catch {
        if (!cancelled) setFindings([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [busy, projects.length]);

  /** The port a Take control click is asking about, with its supervisor. */
  const [takingOver, setTakingOver] = useState<{
    port: number;
    supervisor?: string;
  } | null>(null);

  // Tasks belong to a project, so they are fetched with one rather than kept
  // in the project view: a list of names is cheap, and refetching it on every
  // status poll would not be.
  useEffect(() => {
    if (!project) {
      setTasks([]);
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const found = await api.listTasks(project.id);
        if (!cancelled) setTasks(found);
      } catch {
        // A project whose daemon cannot answer still renders; the services
        // above will be showing the same failure.
        if (!cancelled) setTasks([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [project?.id, busy]);

  async function reloadTasks() {
    if (!project) return;
    setTasks(await api.listTasks(project.id));
  }

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

      {prompt === "add-task" && project && (
        <PromptSheet
          title={`New task in ${project.name}`}
          fields={[
            { label: "Name", placeholder: "dev" },
            { label: "Steps", placeholder: "migrate api web", mono: true },
          ]}
          hint="Service names, in the order they should run. Each brings up its own dependencies first, so a step already covered by an earlier one does nothing."
          onCancel={() => setPrompt(null)}
          onConfirm={([name, steps]) => {
            setPrompt(null);
            void act(async () => {
              await api.setTask(project.id, name, steps.split(/\s+/).filter(Boolean));
              await reloadTasks();
            });
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

      {takingOver && (
        <TakeControlSheet
          port={takingOver.port}
          supervisor={takingOver.supervisor}
          busy={busy}
          onCancel={() => setTakingOver(null)}
          onConfirm={(force) => {
            const { port } = takingOver;
            setTakingOver(null);
            void act(() => api.adoptPort(port, force));
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

      {!findingsHidden && tab !== "settings" && (
        <FindingsBanner findings={findings} onDismiss={() => setFindingsHidden(true)} />
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
                            onStart={() =>
                              act(async () => {
                                const outcome = await api.startService(service.id);
                                // Surfaced where errors are, because it is the
                                // half of the outcome that will not announce
                                // itself later.
                                if (outcome.warning) setError(outcome.warning);
                              })
                            }
                            onStop={() => act(() => api.stopService(service.id))}
                            onRestart={() => act(() => api.restartService(service.id))}
                            onOpen={() =>
                              act(() => openExternal(service.url as string))
                            }
                            onEdit={() => setEditing(service)}
                            onSupervisedControl={(action) =>
                              service.supervisor_entry !== undefined &&
                              act(() =>
                                api.controlSupervised(
                                  service.supervisor_entry as string,
                                  action,
                                ),
                              )
                            }
                            onTakeControl={() =>
                              service.actual_port !== undefined &&
                              setTakingOver({
                                port: service.actual_port,
                                supervisor: service.supervisor,
                              })
                            }
                          />
                        ))
                      )}

                      {(workspace.supervised ?? []).map((entry) => (
                        <SupervisedRow
                          entry={entry}
                          busy={busy}
                          key={`${entry.supervisor}-${entry.name}`}
                          onControl={(action) =>
                            act(() => api.controlSupervised(entry.name, action))
                          }
                          onOpen={() => act(() => openExternal(entry.url as string))}
                        />
                      ))}

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
                        <ExternalRow
                          external={item}
                          busy={busy}
                          key={`${item.port}-${item.pid}`}
                          onTakeControl={() =>
                            setTakingOver({
                              port: item.port,
                              supervisor: item.supervisor,
                            })
                          }
                        />
                      ))}
                    </section>
                  ))}
                </div>

                <TaskPanel
                  tasks={tasks}
                  busy={busy}
                  onAdd={() => setPrompt("add-task")}
                  onRun={(name) =>
                    project && act(() => api.runTask(project.id, name))
                  }
                  onRemove={(name) =>
                    project &&
                    act(async () => {
                      await api.removeTask(project.id, name);
                      await reloadTasks();
                    })
                  }
                />

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

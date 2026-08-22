import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, errorMessage, onRuntimeEvent, openExternal } from "./api";
import { DiscoveryPanel } from "./components/DiscoveryPanel";
import { ContainerRow } from "./components/ContainerRow";
import { ExternalRow } from "./components/ExternalRow";
import { FailureToasts } from "./components/FailureToasts";
import { FolderSheet } from "./components/FolderSheet";
import { StackEditor } from "./components/StackEditor";
import { LogPanel } from "./components/LogPanel";
import { PortTable } from "./components/PortTable";
import { ProjectList } from "./components/ProjectList";
import { PromptSheet } from "./components/PromptSheet";
import { Settings } from "./components/Settings";
import { ServiceEditor } from "./components/ServiceEditor";
import { ServiceRow } from "./components/ServiceRow";
import { SupervisedRow } from "./components/SupervisedRow";
import { FlowChart } from "./components/FlowChart";
import { LOOSE, StackList } from "./components/StackList";
import { TakeControlSheet } from "./components/TakeControlSheet";
import type {
  Discovery,
  Failure,
  Finding,
  LogLine,
  PortOwner,
  ProjectView,
  ServiceView,
  StackView,
} from "./types";
import { affectsFailures, mergeLogs, servicesFor } from "./types";

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
  const [prompt, setPrompt] = useState<
    "add-service" | "scan-folder" | "add-stack" | "add-worktree" | null
  >(null);
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
    const unlisten = onRuntimeEvent((event) => {
      void refreshProjects();
      // What is broken is as live as what is running. Refreshing only the
      // service list left a toast on screen for a service that had since
      // been fixed, or removed — and its Logs and Copy buttons then asked
      // the daemon about an id it no longer knew.
      //
      if (affectsFailures(event)) setRevision((n) => n + 1);
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
    let reading = false;
    logCursor.current = undefined;
    setLogs([]);

    const poll = async () => {
      // One read at a time. The first read of a long log is the slow one, and
      // a tick landing inside it would ask from the beginning again, since the
      // cursor only moves when a reply arrives.
      if (reading) return;
      reading = true;
      try {
        const incoming = await api.getLogs(selectedService, 300, logCursor.current);
        if (cancelled || incoming.length === 0) return;
        // Never backwards. A line the daemon synthesises rather than stores has
        // no place in the sequence, and taking its seq as the cursor means
        // asking again for everything already shown — the whole log repeating,
        // which reads as the service repeating itself.
        const furthest = incoming.reduce((seq, line) => Math.max(seq, line.seq), 0);
        logCursor.current = Math.max(logCursor.current ?? 0, furthest);
        setLogs((current) => mergeLogs(current, incoming));
      } catch (err) {
        if (!cancelled) setError(errorMessage(err));
      } finally {
        reading = false;
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
  const [stacks, setTasks] = useState<StackView[]>([]);

  /** Problems with what is declared, across every project. */
  const [findings, setFindings] = useState<Finding[]>([]);
  const [findingsHidden, setFindingsHidden] = useState(false);

  /** Services that are not working, with what each one said. */
  const [failures, setFailures] = useState<Failure[]>([]);
  /// Bumped when the daemon reports a change worth re-checking for.
  const [revision, setRevision] = useState(0);
  /** The stack the editor is open on, or null when it is making a new one. */
  const [editingStack, setEditingStack] = useState<string | null>(null);
  /** Whether the drawer of services in no stack is open; null means "whatever suits". */
  /** The stack whose services are shown, LOOSE for the unfiled, null for all. */
  const [pickedStack, setPickedStack] = useState<string | null>(null);

  // A choice belongs to the project it was made in. Carrying it across would
  // filter the next project's list by a stack it does not have, which shows
  // nothing and looks like the project is empty.
  useEffect(() => {
    setPickedStack(null);
  }, [selectedProject]);
  /** Dismissed one at a time: reading one is not reading the rest. */
  const [dismissed, setDismissed] = useState<string[]>([]);

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
      try {
        const broken = await api.listFailures();
        if (cancelled) return;
        setFailures(broken);
        // Stop remembering a dismissal once the service it referred to is no
        // longer failing, so the same service breaking again is shown again.
        setDismissed((seen) =>
          seen.filter((id) => broken.some((failure) => failure.service_id === id)),
        );
      } catch {
        if (!cancelled) setFailures([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [busy, projects.length, revision]);

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
        const found = await api.listStacks(project.id);
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

  async function reloadStacks() {
    if (!project) return;
    setTasks(await api.listStacks(project.id));
  }

  /** One service row, wherever it appears — loose, or inside a group. */
  function serviceRow(service: ServiceView) {
    return (
                          <ServiceRow
                            inAStack={stacks.some((stack) =>
                              stack.members.includes(service.name),
                            )}
                            onAddToStack={() => {
                              setEditingStack(null);
                              setPrompt("add-stack");
                            }}
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
    );
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
            {
              label: "Name",
              placeholder: "worker",
              problem: (value) =>
                projects
                  .find((candidate) => candidate.name === promptProject)
                  ?.workspaces.some((workspace) =>
                    workspace.services.some((service) => service.name === value),
                  )
                  ? `${promptProject} already has a service called ${value}.`
                  : null,
            },
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

      {prompt === "add-worktree" && project && (
        <FolderSheet
          title={`Add a worktree of ${project.name}`}
          confirmLabel="Add"
          label="Folder"
          startingAt={project.root_path}
          hint="A git worktree of this repository. It arrives with this project's services on its own port range, so a second branch can be served without redeclaring anything."
          onCancel={() => setPrompt(null)}
          onConfirm={(path) => {
            setPrompt(null);
            void act(() => api.registerWorktree(project.id, path));
          }}
        />
      )}

      {prompt === "add-stack" && project && (
        <StackEditor
          services={project.workspaces.flatMap((workspace) => workspace.services)}
          existing={stacks}
          editing={stacks.find((stack) => stack.name === editingStack) ?? undefined}
          onCancel={() => {
            setPrompt(null);
            setEditingStack(null);
          }}
          onConfirm={(name, steps, after) => {
            const renamed = editingStack && editingStack !== name ? editingStack : null;
            const members = project.workspaces.flatMap((workspace) => workspace.services);
            setPrompt(null);
            setEditingStack(null);
            void act(async () => {
              // The edges are the members' own dependencies, so saving the
              // group writes them back where they live rather than keeping a
              // copy beside it. Only what changed: an untouched service is not
              // rewritten just because a group it belongs to was saved.
              for (const [step, waits] of Object.entries(after)) {
                const service = members.find((candidate) => candidate.name === step);
                if (!service) continue;
                const before = service.depends_on ?? [];
                const same =
                  before.length === waits.length &&
                  before.every((one) => waits.includes(one));
                if (!same) await api.updateService(service.id, { depends_on: waits });
              }
              await api.setStack(project.id, name, steps);
              // A group is keyed by its name, so saving under a new one
              // declares a second group rather than renaming the first.
              if (renamed) await api.removeStack(project.id, renamed);
              await reloadStacks();
            });
          }}
        />
      )}

      {prompt === "scan-folder" && (
        <FolderSheet
          title="Scan a folder"
          confirmLabel="Scan"
          label="Folder"
          hint="Projects that are running are found without this; a folder scan also finds stopped ones."
          onCancel={() => setPrompt(null)}
          onConfirm={(path) => {
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

      {/* Over the corner rather than in the layout: something failing should
          not move the row somebody was about to click. */}
      <FailureToasts
        error={error}
        onDismissError={() => setError(null)}
        failures={failures.filter((failure) => !dismissed.includes(failure.service_id))}
        findings={findingsHidden ? [] : findings}
        onDismissFindings={() => setFindingsHidden(true)}
        onDismiss={(serviceId) => setDismissed((seen) => [...seen, serviceId])}
        onOpenLogs={(serviceId) => {
          setTab("services");
          setSelectedService(serviceId);
          // And the project it belongs to, so the log panel below is showing
          // the service that was just asked about.
          const owner = projects.find((candidate) =>
            candidate.workspaces.some((workspace) =>
              workspace.services.some((service) => service.id === serviceId),
            ),
          );
          if (owner) setSelectedProject(owner.id);
        }}
      />

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
                        {!workspace.worktree && (
                          /* On the primary checkout only: a worktree is added
                             to the repository, not to a branch of it. */
                          <button
                            className="ghost"
                            disabled={busy}
                            onClick={() => setPrompt("add-worktree")}
                            title="Serve another branch of this repository at the same time"
                          >
                            + Worktree
                          </button>
                        )}
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

                      {/* Stacks on the left, services on the right. A stack
                          is what somebody declared this is brought up as, so
                          it is what you choose by; nothing chosen shows every
                          service, because the list is still one list. */}
                      <div className="workspace-body">
                        <StackList
                          stacks={stacks}
                          total={workspace.services.length}
                          loose={
                            workspace.services.filter(
                              (service: ServiceView) =>
                                !stacks.some((stack) => stack.members.includes(service.name)),
                            ).length
                          }
                          selected={pickedStack}
                          busy={busy}
                          onSelect={setPickedStack}
                          onRun={(name) => act(() => api.runStack(project.id, name))}
                          onStop={(name) => act(() => api.stopStack(project.id, name))}
                          onEdit={(name) => {
                            setEditingStack(name);
                            setPrompt("add-stack");
                          }}
                          onRemove={(name) =>
                            act(async () => {
                              await api.removeStack(project.id, name);
                              await reloadStacks();
                            })
                          }
                          onNew={() => {
                            // New means new: the sheet is the one Edit opens,
                            // so it would otherwise arrive filled in.
                            setEditingStack(null);
                            setPrompt("add-stack");
                          }}
                        />

                        <div className="workspace-services">
                          {(() => {
                            const chosen = stacks.find((stack) => stack.name === pickedStack);
                            const shown = servicesFor(
                              workspace.services,
                              stacks,
                              pickedStack,
                              LOOSE,
                            );

                            return (
                              <>
                                {/* The shape of the chosen stack, above its
                                    members: what waits for what is the reason
                                    it is one thing rather than several. */}
                                {chosen && (chosen.flow ?? []).length > 0 && (
                                  <div className="stack-flow">
                                    <FlowChart flow={chosen.flow ?? []} />
                                  </div>
                                )}

                                {shown.length === 0 ? (
                                  <p className="empty">
                                    {workspace.services.length === 0
                                      ? "No services detected."
                                      : "Nothing here."}
                                  </p>
                                ) : (
                                  shown.map((service: ServiceView) => serviceRow(service))
                                )}
                              </>
                            );
                          })()}
                        </div>
                      </div>

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

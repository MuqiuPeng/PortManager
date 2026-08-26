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
import { FlowChart, type Placement } from "./components/FlowChart";
import { LOOSE, StackCards } from "./components/StackCards";
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
import { mergeLogs, servicesFor } from "./types";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

type Tab = "services" | "ports" | "discover" | "settings";

/** Ports change without any event of their own, so that tab polls. */
const PORT_POLL_MS = 4000;
/**
 * How long after an action to look again.
 *
 * A container's exit is not an event either, and `stop` returns before Docker
 * has finished: the refresh that follows the call catches the containers still
 * up, and nothing after it says otherwise. So the rows kept saying "running"
 * for something `docker ps` no longer listed.
 *
 * This is a patch over a missing event rather than the fix. What would settle
 * it is the daemon noticing a container change and announcing it, the way it
 * announces a service's.
 */
const SETTLE_MS = 2500;
const LOG_POLL_MS = 800;

export default function App() {
  const [projects, setProjects] = useState<ProjectView[]>([]);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [selectedService, setSelectedService] = useState<string | null>(null);
  const [ports, setPorts] = useState<PortOwner[]>([]);
  /// False until the first answer arrives. An empty list before then is "not
  /// asked yet", which is not the same as "nothing is listening".
  const [portsRead, setPortsRead] = useState(false);
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
    const unlisten = onRuntimeEvent(() => {
      void refreshProjects();
      // What is broken is as live as what is running. Refreshing only the
      // service list left a toast on screen for a service that had since
      // been fixed, or removed — and its Logs and Copy buttons then asked
      // the daemon about an id it no longer knew.
      //
        // Every event, not only the ones that change a failure. A stack lights
        // up as its members come healthy, and each of those is a
        // `service_changed` at a moment when nothing is broken — without this
        // the flow chart stayed grey until the whole stack had finished and
        // then turned green at once, which is the one moment the order cannot
        // be seen.
        setRevision((n) => n + 1);
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
        if (!cancelled) {
          setPorts(next);
          setPortsRead(true);
        }
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

  /** Where somebody has dragged the nodes of a stack, kept per stack.
   *
   * Positions are only positions — the dependencies are still what the edges
   * are drawn from — so an arrangement that has gone stale is untidy rather
   * than wrong, and a node nobody has moved falls back to the layout. */
  const [placements, setPlacements] = useState<Record<string, Placement>>({});

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
  /** The checkout being looked at. A branch is a view, not more content. */
  const [branch, setBranch] = useState<string | null>(null);

  // A choice belongs to the project it was made in. Carrying it across would
  // filter the next project's list by a stack it does not have, which shows
  // nothing and looks like the project is empty.
  useEffect(() => {
    setPickedStack(null);
    setBranch(null);
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
      } catch (err) {
        // A project whose daemon cannot answer still renders — but say so
        // rather than showing an empty list. Swallowing this is how the window
        // came to invoke a command that no longer existed and look, for a day,
        // like a project with no stacks in it.
        if (!cancelled) {
          setTasks([]);
          setError(errorMessage(err));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [project?.id, busy, revision]);

  /** Read a stack's arrangement once, the first time it is looked at. */
  const loadPlacement = useCallback(async (stackId: string) => {
    try {
      const raw = await api.getSetting(`desktop.flow.${stackId}`);
      if (!raw) return;
      setPlacements((all) => ({ ...all, [stackId]: JSON.parse(raw) as Placement }));
    } catch {
      // An arrangement that will not parse is one to forget, not to stop for:
      // every node falls back to the layout it would have had anyway.
    }
  }, []);

  async function moveNode(stackId: string, name: string, x: number, y: number) {
    const next = { ...(placements[stackId] ?? {}), [name]: { x, y } };
    setPlacements((all) => ({ ...all, [stackId]: next }));
    try {
      await api.setSetting(`desktop.flow.${stackId}`, JSON.stringify(next));
    } catch (err) {
      setError(errorMessage(err));
    }
  }

  useEffect(() => {
    const chosenStack = stacks.find((stack) => stack.name === pickedStack);
    if (chosenStack && placements[chosenStack.id] === undefined) {
      void loadPlacement(chosenStack.id);
    }
  }, [stacks, pickedStack, placements, loadPlacement]);

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
      // And once more when whatever was asked has had time to finish. See
      // SETTLE_MS: the containers a stack brings up outlive the call that
      // stops them by a second or two, and nothing tells us when they go.
      window.setTimeout(() => void refreshProjects(), SETTLE_MS);
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
          onConfirm={(force, restart) => {
            const { port } = takingOver;
            setTakingOver(null);
            void act(async () => {
              // Declared first either way: taking over needs a service to take
              // over, and adopting is what reads the command off the running
              // process rather than guessing it from package.json.
              const adopted = await api.adoptPort(port, force);
              if (restart) await api.takeOverService(adopted.service.id);
            });
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

      <div className="body">
        <ProjectList
          projects={projects}
          selectedId={selectedProject}
          onSelect={(id) => {
            setSelectedProject(id);
            setTab("services");
          }}
          onAdd={() => {
            setTab("discover");
            void scan();
          }}
          busy={busy}
          current={tab}
          onView={(view) => {
            setTab(view);
            if (view === "discover" && discoveries.length === 0) void scan();
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
            <PortTable ports={ports} read={portsRead} />
          </main>
        ) : (
          <main className="detail">
            {!project ? (
              <p className="empty">Select a project.</p>
            ) : (
              <>
                <header className="detail-head">
                  <div className="min-w-0 flex-1">
                    <h1>{project.name}</h1>
                    <span className="path">{project.root_path}</span>
                  </div>
                  <span className="actions">
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy}
                      onClick={() => setPrompt("add-worktree")}
                      title="Serve another branch of this repository at the same time"
                    >
                      + Worktree
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={busy}
                      onClick={() => {
                        setPromptProject(project.name);
                        setPrompt("add-service");
                      }}
                    >
                      + Service
                    </Button>
                  </span>
                </header>

                {(() => {
                  // One checkout at a time. A branch is a view of this project,
                  // not more of it: rendering every checkout in full repeated
                  // the same stack list three times for a repository cloned
                  // three times, which is what made this look deep.
                  const checkout =
                    project.workspaces.find((w) => w.id === branch) ?? project.workspaces[0];
                  if (!checkout) return <p className="empty">No checkouts.</p>;

                  const loose = checkout.services.filter(
                    (service: ServiceView) =>
                      !stacks.some((stack) => stack.members.includes(service.name)),
                  );
                  const chosen = stacks.find((stack) => stack.name === pickedStack);
                  const shown = servicesFor(checkout.services, stacks, pickedStack, LOOSE);

                  // The pane itself does not scroll; each column does. One
                  // scrollbar for both would mean scrolling past a long service
                  // list to reach the stack that chose it.
                  return (
                    <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-hidden p-4">
                      {project.workspaces.length > 1 && (
                        <div className="flex flex-wrap items-center gap-1">
                          {project.workspaces.map((w) => (
                            <button
                              key={w.id}
                              onClick={() => setBranch(w.id)}
                              title={w.path}
                              className={cn(
                                "rounded-md border px-2 py-1 font-mono text-[11px] transition-colors",
                                w.id === checkout.id
                                  ? "border-ring bg-accent"
                                  : "text-muted-foreground hover:bg-accent/50",
                              )}
                            >
                              {w.git_branch ?? "(detached)"}
                              {w.port_offset > 0 && (
                                <span className="ml-1 opacity-60">+{w.port_offset}</span>
                              )}
                            </button>
                          ))}
                        </div>
                      )}

                      <div className="flex min-h-0 flex-1 gap-4">
                      {/* Stacks on the wide side, their members on the narrow
                          one, which is the opposite of how much each says: a
                          service row is a name and a state and needs no width,
                          while a row of stack cards wraps as soon as a project
                          has more than three ways to start. */}
                      <div className="flex min-w-0 flex-1 flex-col gap-3 overflow-y-auto">
                      <StackCards
                        stacks={stacks}
                        total={checkout.services.length}
                        loose={loose.length}
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
                          // New means new: the sheet is the one Edit opens.
                          setEditingStack(null);
                          setPrompt("add-stack");
                        }}
                      />

                      </div>

                      <div className="flex w-72 shrink-0 flex-col gap-4 overflow-y-auto">
                      <div className="flex flex-col gap-0.5">
                        {shown.length === 0 ? (
                          <p className="empty">
                            {checkout.services.length === 0
                              ? "No services detected."
                              : "Nothing here."}
                          </p>
                        ) : (
                          shown.map((service: ServiceView) => serviceRow(service))
                        )}
                      </div>

                      {/* Everything the runtime can see but did not declare.
                          Only when looking at all of it: they belong to no
                          stack, so a stack's view is not where they go. */}
                      {pickedStack === null && (
                        <>
                          {(checkout.supervised ?? []).map((entry) => (
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

                          {(checkout.containers ?? []).map((container) => (
                            <ContainerRow
                              container={container}
                              busy={busy}
                              key={container.name}
                              onControl={(action) =>
                                act(() => api.controlContainer(container.name, action))
                              }
                            />
                          ))}

                          {(checkout.external ?? []).map((item) => (
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
                        </>
                      )}
                        </div>
                      </div>

                      {/* The whole width, under both columns.

                          Stages run left to right, so what this picture needs
                          is width. It has been in two wrong places already: a
                          row between the cards and the services, where it was
                          squeezed vertically, and inside the stack column,
                          where it had half the width and ran off the edge with
                          a third of the height under it empty. A band of its
                          own gives it the long axis and takes only the short
                          one. */}
                    {chosen && (chosen.flow ?? []).length > 0 && (
                      <div className="flex min-h-0 flex-1 overflow-auto rounded-lg border">
                        <FlowChart
                          flow={chosen.flow ?? []}
                          placement={placements[chosen.id]}
                          onMove={(name, x, y) => void moveNode(chosen.id, name, x, y)}
                        />
                      </div>
                    )}
                    </div>
                  );
                })()}


              </>
            )}
          </main>
        )}

        {/* Beside what it belongs to, not under it. A log pane across the
            bottom takes the same height whether or not anything is selected,
            and the thing it describes is a row you are pointing at — which is
            up here. It appears only when there is a service to show, so it
            costs nothing the rest of the time. */}
        {tab === "services" && selectedServiceView && (
          <LogPanel
            serviceName={selectedServiceView.name}
            lines={logs}
            onClose={() => setSelectedService(null)}
          />
        )}
      </div>
    </div>
  );
}

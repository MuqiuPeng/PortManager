import type React from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";

import wire from "../__fixtures__/wire.json";
import { ContainerRow } from "../components/ContainerRow";
import { ExternalRow } from "../components/ExternalRow";
import { FailureToasts } from "../components/FailureToasts";
import { FindingsBanner } from "../components/FindingsBanner";
import { FlowChart } from "../components/FlowChart";
import { partition } from "../Panel";
import { mergeLogs, rowAction } from "../types";
import { ServiceRow } from "../components/ServiceRow";
import { StackEditor } from "../components/StackEditor";
import { SupervisedRow } from "../components/SupervisedRow";
import { StackRow } from "../components/StackRow";
import { affectsFailures } from "../types";
import type {
  ContainerView,
  Failure,
  ExternalService,
  Finding,
  ServiceView,
  SupervisedView,
  StackView,
  FlowNode,
  ProjectView,
  LogLine,
} from "../types";

/**
 * Rendering against what the daemon actually sends.
 *
 * The fixtures are written by the Rust types, not by hand, because the mistake
 * that took the window down cannot be made twice any other way: a TypeScript
 * interface said a field was always present, `skip_serializing_if` leaves it
 * out when empty, and reading `.length` off the gap threw during render and
 * unmounted the whole tree. A blank window, no error, nothing in the console.
 *
 * So every component is rendered twice — once with a full payload, once with
 * one where every optional field is absent. The second is the one that matters.
 */

const noop = () => {};

/** Server rendering separates adjacent text nodes with a comment. */
const render = (element: React.ReactElement) =>
  renderToString(element).replace(/<!-- -->/g, "");

describe("a payload with every optional field absent", () => {
  it("renders a supervised entry that holds no ports", () => {
    const entry = wire.supervised_minimal as unknown as SupervisedView;
    expect("ports" in entry).toBe(false);

    const html = render(
      <SupervisedRow entry={entry} busy={false} onControl={noop} onOpen={noop} />,
    );
    expect(html).toContain("loom-tunnel");
  });

  it("renders a service that is stopped and unclaimed", () => {
    const service = wire.service_minimal as unknown as ServiceView;
    const html = render(
      <ServiceRow
        service={service}
        selected={false}
        busy={false}
        onSelect={noop}
        onStart={noop}
        onStop={noop}
        onRestart={noop}
        onOpen={noop}
        onEdit={noop}
        onTakeControl={noop}
        onSupervisedControl={noop}
        inAStack
        onAddToStack={noop}
      />,
    );
    expect(html).toContain("web");
  });

  it("renders an external port with nothing known about it", () => {
    const external = wire.external_minimal as unknown as ExternalService;
    const html = render(
      <ExternalRow external={external} busy={false} onTakeControl={noop} />,
    );
    expect(html).toContain("5555");
  });

  it("renders a failure that said nothing", () => {
    const silent = wire.failure_silent as unknown as Failure;
    expect("detail" in silent).toBe(false);
    expect("exit_code" in silent).toBe(false);

    const html = render(
      <FailureToasts failures={[silent]} onOpenLogs={noop} onDismiss={noop} />,
    );
    expect(html).toContain("shop/web");
    expect(html).toContain("It said nothing before it stopped.");
  });

  it("renders a container that publishes nothing", () => {
    const container = wire.container_minimal as unknown as ContainerView;
    const html = render(
      <ContainerRow container={container} busy={false} onControl={noop} />,
    );
    expect(html).toContain("db");
  });
});

describe("a payload with everything set", () => {
  it("says who is keeping a service alive", () => {
    const service = wire.service_full as unknown as ServiceView;
    const html = render(
      <ServiceRow
        service={service}
        selected={false}
        busy={false}
        onSelect={noop}
        onStart={noop}
        onStop={noop}
        onRestart={noop}
        onOpen={noop}
        onEdit={noop}
        onTakeControl={noop}
        onSupervisedControl={noop}
        inAStack
        onAddToStack={noop}
      />,
    );
    // The answer to "why is there no Stop button" — and, since there is a
    // supervisor to ask, there is one.
    expect(html).toContain("kept alive by pm2");
    expect(html).toContain("Stop");
    expect(html).toContain("after db");
  });

  it("reports what a finished one-shot did rather than calling it stopped", () => {
    const service = wire.service_one_shot_ran as unknown as ServiceView;
    const html = render(
      <ServiceRow
        service={service}
        selected={false}
        busy={false}
        onSelect={noop}
        onStart={noop}
        onStop={noop}
        onRestart={noop}
        onOpen={noop}
        onEdit={noop}
        onTakeControl={noop}
        onSupervisedControl={noop}
        inAStack
        onAddToStack={noop}
      />,
    );
    expect(html).toContain("ran successfully");
    // Nothing to stop: it already finished.
    expect(html).not.toContain(">Stop<");
  });

  it("shows what a failing service said", () => {
    const failure = wire.failure_full as unknown as Failure;
    const html = render(
      <FailureToasts failures={[failure]} onOpenLogs={noop} onDismiss={noop} />,
    );
    expect(html).toContain("Loom/api");
    expect(html).toContain("exit 1");
    // The reason, which is the whole point of showing this at all.
    expect(html).toContain("DATABASE_URL is not set");
  });

  it("puts the certain findings first", () => {
    const certain = wire.finding as unknown as Finding;
    const maybe: Finding = { ...certain, subject: "other/web", certain: false };
    const html = render(
      <FindingsBanner findings={[maybe, certain]} onDismiss={noop} />,
    );
    expect(html.indexOf("Loom/api")).toBeLessThan(html.indexOf("other/web"));
  });
});

describe("what makes the window re-ask", () => {
  it("re-asks after a service is removed, so a toast for it goes away", () => {
    expect(
      affectsFailures({
        event: "service_changed",
        project_id: "p",
        service_id: "s",
        removed: true,
      }),
    ).toBe(true);
  });

  it("re-asks after a service exits, which is when it has just failed", () => {
    expect(affectsFailures({ event: "service_exited", service_id: "s" })).toBe(true);
  });

  it("does not re-ask for a log line", () => {
    expect(
      affectsFailures({ event: "log", seq: 1, service_id: "s", message: "hi" }),
    ).toBe(false);
  });
});

describe("declaring a group", () => {
  const services = [wire.service_minimal, wire.service_full] as ServiceView[];

  it("offers the project's services rather than a box to type names in", () => {
    const html = renderToString(
      <StackEditor services={services} existing={[]} onCancel={() => {}} onConfirm={() => {}} />,
    );
    for (const service of services) {
      expect(html).toContain(service.name);
    }
    // One box you can type in, for the name. Members are ticked, not typed.
    const typeable = html.match(/<input(?![^>]*type="checkbox")/g) ?? [];
    expect(typeable.length).toBe(1);
    expect(html).toContain('type="checkbox"');
  });

  it("cannot be created with no members", () => {
    const html = renderToString(
      <StackEditor services={services} existing={[]} onCancel={() => {}} onConfirm={() => {}} />,
    );
    expect(html).toContain("Create</button>");
    expect(html).toContain("disabled");
  });

  it("says so when the name is one the project already uses", () => {
    const existing = [
      { id: "t", workspace_id: "w", name: "dev", members: ["web"], services: [], running: 0 },
    ] as unknown as StackView[];
    const html = renderToString(
      <StackEditor
        services={services}
        existing={existing}
        editing={undefined}
        onCancel={() => {}}
        onConfirm={() => {}}
      />,
    );
    // Nothing typed yet, so nothing to complain about.
    expect(html).not.toContain("already has a group");
  });
});

describe("an existing group", () => {
  // The fixtures are both called `web`, which is fine for a shape but cannot
  // show an order. Two names, over the shape the daemon actually sends.
  const services = [
    { ...wire.service_minimal, id: "a", name: "db" },
    { ...wire.service_full, id: "b", name: "api" },
  ] as ServiceView[];
  const group = {
    id: "t",
    workspace_id: "w",
    name: "dev",
    members: ["db", "api"],
    services: [],
    running: 0,
  } as unknown as StackView;

  it("opens showing what it was set to, in the order it was set in", () => {
    const html = renderToString(
      <StackEditor
        services={services}
        existing={[group]}
        editing={group}
        onCancel={() => {}}
        onConfirm={() => {}}
      />,
    );
    expect(html).toContain("Edit dev");
    expect(html).toContain('value="dev"');
    // Both members ticked, and the first-declared one numbered 1.
    const first = html.indexOf(group.members[0]);
    const second = html.indexOf(group.members[1]);
    expect(first).toBeGreaterThan(-1);
    expect(second).toBeGreaterThan(first);
    expect(html).toContain("Save</button>");
  });

  it("does not call its own name a clash with itself", () => {
    const html = renderToString(
      <StackEditor
        services={services}
        existing={[group]}
        editing={group}
        onCancel={() => {}}
        onConfirm={() => {}}
      />,
    );
    expect(html).not.toContain("already has a group");
  });

  it("offers a way in from the row itself", () => {
    const html = renderToString(
      <StackRow
        stack={{ ...group, services: [] }}
        busy={false}
        onRun={() => {}}
        onStop={() => {}}
        onEdit={() => {}}
        onRemove={() => {}}
        renderService={() => null}
      />,
    );
    expect(html).toContain("Edit");
  });
});

describe("a group drawn as a graph", () => {
  // db first; api and jobs both wait only for db, so they are one level.
  const flow = [
    { name: "db", service_id: "a", after: [], level: 0, status: "healthy" },
    { name: "api", service_id: "b", after: ["db"], level: 1, status: "healthy" },
    { name: "jobs", service_id: "c", after: ["db"], level: 1, status: "stopped" },
    { name: "web", service_id: "d", after: ["api"], level: 2, status: "stopped" },
  ] as FlowNode[];

  it("puts what waits for the same thing side by side, not in a queue", () => {
    const html = renderToString(<FlowChart flow={flow} />);
    const y = (name: string) => {
      const at = html.indexOf(`>${name}<`);
      const g = html.lastIndexOf("translate(", at);
      return Number(html.slice(g + 10, html.indexOf(")", g)).split(" ")[1]);
    };
    expect(y("api")).toBe(y("jobs"));
    expect(y("db")).toBeLessThan(y("api"));
    expect(y("web")).toBeGreaterThan(y("api"));
  });

  it("draws a line for every wait", () => {
    const html = renderToString(<FlowChart flow={flow} />);
    expect(html.match(/class="flow-edge"/g)?.length).toBe(3);
  });

  it("marks a step whose service is gone", () => {
    const gone = [{ name: "ghost", after: [], level: 0, status: "stopped" }] as FlowNode[];
    expect(renderToString(<FlowChart flow={gone} />)).toContain("flow-node missing");
  });
});

describe("what the panel shows", () => {
  const service = (name: string, status = "stopped") =>
    ({ ...wire.service_minimal, id: name, name, status }) as ServiceView;

  const project = (workspaces: unknown[]) =>
    ({ id: "p", name: "shop", workspaces }) as unknown as ProjectView;

  it("shows a group as one thing, not as its members", () => {
    const { groups, loose } = partition([
      project([
        {
          id: "w",
          git_branch: "main",
          services: [service("db"), service("api"), service("docs")],
          stacks: [{ id: "t", name: "stack", members: ["db", "api"], services: [], running: 0 }],
        },
      ]),
    ]);
    expect(groups.map((one) => one.stack.name)).toEqual(["stack"]);
    // db and api are inside it, so only docs is left over.
    expect(loose.map((one) => one.service.name)).toEqual(["docs"]);
  });

  it("does not file one checkout's service under another's group", () => {
    // Both branches have a service called `api`; only main declares the group.
    const { loose } = partition([
      project([
        {
          id: "w1",
          git_branch: "main",
          services: [service("api")],
          stacks: [{ id: "t", name: "stack", members: ["api"], services: [], running: 0 }],
        },
        { id: "w2", git_branch: "feature", services: [service("api")], stacks: [] },
      ]),
    ]);
    expect(loose).toHaveLength(1);
    expect(loose[0].branch).toBe("feature");
  });

  it("puts what is running first", () => {
    const { groups } = partition([
      project([
        {
          id: "w",
          git_branch: "main",
          services: [],
          stacks: [
            { id: "a", name: "aaa", members: ["x"], services: [], running: 0 },
            { id: "b", name: "zzz", members: ["y"], services: [], running: 1 },
          ],
        },
      ]),
    ]);
    expect(groups.map((one) => one.stack.name)).toEqual(["zzz", "aaa"]);
  });
});

describe("adding newly-read log lines", () => {
  const line = (seq: number) => ({ seq, stream: "stdout", message: `line ${seq}` }) as LogLine;

  it("shows a line once when the same read arrives twice", () => {
    // Two reads in flight, both asked from the beginning because the cursor
    // had not moved yet. This is the whole log arriving twice.
    const first = [line(0), line(1), line(2)];
    const shown = mergeLogs(mergeLogs([], first), first);
    expect(shown.map((one) => one.seq)).toEqual([0, 1, 2]);
  });

  it("still adds what is genuinely new", () => {
    const shown = mergeLogs(mergeLogs([], [line(0), line(1)]), [line(1), line(2)]);
    expect(shown.map((one) => one.seq)).toEqual([0, 1, 2]);
  });

  it("keeps the newest when there are more than it holds", () => {
    const many = Array.from({ length: 12 }, (_, index) => line(index));
    expect(mergeLogs([], many, 5).map((one) => one.seq)).toEqual([7, 8, 9, 10, 11]);
  });
});

describe("what the panel will do with a service", () => {
  it("starts one that is in a group", () => {
    expect(rowAction(false, true)).toBe("start");
  });

  it("will not start one that is in none", () => {
    expect(rowAction(false, false)).toBe("stack");
  });

  it("stops one that is running, grouped or not", () => {
    // Refusing here would withhold the thing the panel is open for, and
    // nothing is being brought up in the wrong shape by stopping it.
    expect(rowAction(true, true)).toBe("stop");
    expect(rowAction(true, false)).toBe("stop");
  });
});

describe("a service in no stack, in the window", () => {
  it("is not offered a Start button", () => {
    const html = renderToString(
      <ServiceRow
        service={{ ...wire.service_minimal, status: "stopped" } as ServiceView}
        selected={false}
        busy={false}
        inAStack={false}
        onAddToStack={() => {}}
        onSelect={() => {}}
        onStart={() => {}}
        onStop={() => {}}
        onRestart={() => {}}
        onOpen={() => {}}
        onEdit={() => {}}
        onTakeControl={() => {}}
        onSupervisedControl={() => {}}
      />,
    );
    // The daemon refuses this one, so the window must not offer it.
    expect(html).not.toContain(">Start<");
    expect(html).toContain("Add to a stack");
  });
});

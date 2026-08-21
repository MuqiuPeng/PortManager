import type React from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";

import wire from "../__fixtures__/wire.json";
import { ContainerRow } from "../components/ContainerRow";
import { ExternalRow } from "../components/ExternalRow";
import { FailureToasts } from "../components/FailureToasts";
import { FindingsBanner } from "../components/FindingsBanner";
import { ServiceRow } from "../components/ServiceRow";
import { SupervisedRow } from "../components/SupervisedRow";
import { affectsFailures } from "../types";
import type {
  ContainerView,
  Failure,
  ExternalService,
  Finding,
  ServiceView,
  SupervisedView,
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

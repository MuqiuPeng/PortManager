import { useState } from "react";

import type { ServiceView, StackView } from "../types";
import { FlowChart } from "./FlowChart";

interface Props {
  stack: StackView;
  busy: boolean;
  onRun: () => void;
  onStop: () => void;
  onEdit: () => void;
  onRemove: () => void;
  /** Rendered by the caller, so a member looks like any other service row. */
  renderService: (service: ServiceView) => React.ReactNode;
}

/**
 * A declared group, as one thing.
 *
 * A database, an API and a front end that have to start in that order are one
 * thing to the person using them. Shown as three peers with three buttons, the
 * reader has to reassemble that every time they look, and the order lives only
 * in their memory — which is the part that gets it wrong at the wrong moment.
 *
 * So the group gets the state and the buttons, and its members sit under it as
 * what it is made of rather than as alternatives to it. Nothing here is
 * inferred: a group exists because somebody said so.
 */
export function StackRow({ stack, busy, onRun, onStop, onEdit, onRemove, renderService }: Props) {
  /** A node clicked on the diagram, whose row is then the one shown. */
  const [picked, setPicked] = useState<string | undefined>(undefined);
  const total = stack.steps.length;
  const missing = stack.missing ?? [];
  const allUp = total > 0 && stack.running === total;
  const someUp = stack.running > 0;

  return (
    <section className="stack">
      <header className="stack-head">
        <span
          className={allUp ? "dot status-healthy" : someUp ? "dot partial" : "dot"}
          aria-hidden
        />
        <span className="stack-name">{stack.name}</span>
        <span className="stack-count">
          {stack.running}/{total} up
        </span>

        {missing.length > 0 && (
          // A step naming nothing fails the group halfway through, having
          // already started what came before it.
          <span className="stack-missing">missing {missing.join(", ")}</span>
        )}

        <span className="spacer" />
        {someUp ? (
          <button className="ghost danger" onClick={onStop} disabled={busy}>
            Stop
          </button>
        ) : (
          <button className="ghost primary" onClick={onRun} disabled={busy}>
            Start
          </button>
        )}
        {/* Somewhere to see what this group was set to. Without it the only
            way back to the order somebody chose is to delete the group and
            declare it again from memory. */}
        <button className="ghost" onClick={onEdit} disabled={busy} title="Change what is in this stack">
          Edit
        </button>
        <button className="ghost" onClick={onRemove} disabled={busy} title="Delete this stack">
          −
        </button>
      </header>

      {(stack.flow ?? []).length > 0 && (
        <div className="stack-flow">
          <FlowChart
            flow={stack.flow ?? []}
            selected={picked}
            onSelect={(node) => setPicked(picked === node.name ? undefined : node.name)}
          />
        </div>
      )}

      {/* The diagram says the shape; the rows say the state, and only for what
          was clicked unless nothing was — a group of eight is a diagram worth
          reading, not eight rows to scroll past. */}
      <div className="stack-members">
        {stack.services
          .filter((service) => !picked || service.name === picked)
          .map(renderService)}
      </div>
    </section>
  );
}

import type { ServiceView, TaskView } from "../types";

interface Props {
  task: TaskView;
  busy: boolean;
  onRun: () => void;
  onStop: () => void;
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
export function TaskGroup({ task, busy, onRun, onStop, onRemove, renderService }: Props) {
  const total = task.steps.length;
  const missing = task.missing ?? [];
  const allUp = total > 0 && task.running === total;
  const someUp = task.running > 0;

  return (
    <section className="group">
      <header className="group-head">
        <span
          className={allUp ? "dot status-healthy" : someUp ? "dot partial" : "dot"}
          aria-hidden
        />
        <span className="group-name">{task.name}</span>
        <span className="group-count">
          {task.running}/{total} up
        </span>

        {missing.length > 0 && (
          // A step naming nothing fails the group halfway through, having
          // already started what came before it.
          <span className="group-missing">missing {missing.join(", ")}</span>
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
        <button className="ghost" onClick={onRemove} disabled={busy} title="Ungroup">
          −
        </button>
      </header>

      <div className="group-members">{task.services.map(renderService)}</div>
    </section>
  );
}

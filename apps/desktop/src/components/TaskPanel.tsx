import type { Task } from "../types";

interface Props {
  tasks: Task[];
  busy: boolean;
  onRun: (name: string) => void;
  onRemove: (name: string) => void;
  onAdd: () => void;
}

/**
 * Named step sequences for a checkout.
 *
 * Dependencies say what one service needs. A task says what *you* want up,
 * which is often not one service's chain: a dev session is an API and a web
 * front end side by side, with a migration in front of both. Each step brings
 * up its own dependencies, so a step already covered by an earlier one does
 * nothing, and a task run twice is not a task run twice.
 */
export function TaskPanel({ tasks, busy, onRun, onRemove, onAdd }: Props) {
  return (
    <section className="tasks">
      <div className="tasks-head">
        <h3>Tasks</h3>
        <button className="ghost" onClick={onAdd} disabled={busy}>
          + Task
        </button>
      </div>

      {tasks.length === 0 ? (
        <p className="hint">
          A task brings up several services in order — a migration, then an API,
          then a front end.
        </p>
      ) : (
        tasks.map((task) => (
          <div className="task" key={task.id}>
            <span className="task-name">{task.name}</span>
            <span className="task-steps" title={task.steps.join(" → ")}>
              {task.steps.join(" → ")}
            </span>
            <button
              className="ghost primary"
              onClick={() => onRun(task.name)}
              disabled={busy}
            >
              Run
            </button>
            <button
              className="ghost"
              onClick={() => onRemove(task.name)}
              disabled={busy}
              title="Remove the task. Nothing running is touched."
            >
              −
            </button>
          </div>
        ))
      )}
    </section>
  );
}

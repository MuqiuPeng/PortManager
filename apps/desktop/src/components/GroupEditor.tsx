import { useState } from "react";

import type { ServiceView, TaskView } from "../types";

interface Props {
  /** Everything in the project that could be a member. */
  services: ServiceView[];
  /** Groups already declared, so a name is not reused by accident. */
  existing: TaskView[];
  /** The group being edited, if this is an edit rather than a new one. */
  editing?: TaskView;
  onCancel: () => void;
  onConfirm: (name: string, steps: string[]) => void;
}

/**
 * Declare a group: which services, and in what order.
 *
 * Members are picked from what exists rather than typed. A typed list of names
 * can name a service that is not there, spell one wrong, or list one twice, and
 * none of that is visible until the group is run. Picking cannot express any of
 * those, so the check is not needed and the mistake cannot be made.
 *
 * Order is the list's order and is moved with the arrows, the way a scheme's
 * build order reads: first at the top, and stopped bottom-up. Something already
 * up when its turn comes is left alone, so the order only ever says what must
 * precede what.
 */
export function GroupEditor({ services, existing, editing, onCancel, onConfirm }: Props) {
  const [name, setName] = useState(editing?.name ?? "");
  const [steps, setSteps] = useState<string[]>(editing?.steps ?? []);

  const available = services.filter((view) => !steps.includes(view.name));
  const taken = existing.some(
    (task) => task.name === name.trim() && task.name !== editing?.name,
  );
  const problem = taken
    ? `This project already has a group called ${name.trim()}.`
    : steps.length === 0
      ? "Choose at least one service."
      : null;
  const ready = name.trim() !== "" && problem === null;

  function move(index: number, by: number) {
    const to = index + by;
    if (to < 0 || to >= steps.length) return;
    const next = [...steps];
    [next[index], next[to]] = [next[to], next[index]];
    setSteps(next);
  }

  return (
    <div className="sheet-backdrop" onClick={onCancel}>
      <div className="sheet" onClick={(event) => event.stopPropagation()}>
        <header className="sheet-head">
          <h2>{editing ? `Edit ${editing.name}` : "Group services"}</h2>
        </header>

        <div className="sheet-body">
          <label className="field wide">
            <span>Name</span>
            <input
              autoFocus
              value={name}
              placeholder="dev"
              spellCheck={false}
              onChange={(event) => setName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") onCancel();
              }}
            />
          </label>

          <div className="group-columns">
            <section className="group-column">
              <h3>Starts in this order</h3>
              {steps.length === 0 ? (
                <p className="hint">Nothing chosen yet.</p>
              ) : (
                <ol className="group-steps">
                  {steps.map((step, index) => (
                    <li key={step}>
                      <span className="step-order">{index + 1}</span>
                      <span className="step-name">{step}</span>
                      <button
                        type="button"
                        className="ghost tiny"
                        title="Earlier"
                        disabled={index === 0}
                        onClick={() => move(index, -1)}
                      >
                        ↑
                      </button>
                      <button
                        type="button"
                        className="ghost tiny"
                        title="Later"
                        disabled={index === steps.length - 1}
                        onClick={() => move(index, 1)}
                      >
                        ↓
                      </button>
                      <button
                        type="button"
                        className="ghost tiny"
                        title="Remove from the group"
                        onClick={() => setSteps(steps.filter((name) => name !== step))}
                      >
                        ✕
                      </button>
                    </li>
                  ))}
                </ol>
              )}
            </section>

            <section className="group-column">
              <h3>In this project</h3>
              {available.length === 0 ? (
                <p className="hint">
                  {services.length === 0
                    ? "This project has no services yet."
                    : "Every service is in the group."}
                </p>
              ) : (
                <ul className="group-available">
                  {available.map((view) => (
                    <li key={view.id}>
                      <button
                        type="button"
                        className="ghost"
                        onClick={() => setSteps([...steps, view.name])}
                      >
                        + {view.name}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          </div>

          <p className="hint">
            Started top to bottom and stopped in reverse, as one thing. A service already up
            when its turn comes is left alone.
          </p>
          {problem && name.trim() !== "" && <p className="hint problem">{problem}</p>}
        </div>

        <footer className="sheet-foot">
          <span className="spacer" />
          <button className="ghost" onClick={onCancel}>
            Cancel
          </button>
          <button
            className="ghost primary"
            disabled={!ready}
            onClick={() => ready && onConfirm(name.trim(), steps)}
          >
            {editing ? "Save" : "Create"}
          </button>
        </footer>
      </div>
    </div>
  );
}

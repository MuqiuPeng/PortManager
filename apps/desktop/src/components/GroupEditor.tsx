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
  onConfirm: (name: string, steps: string[], after: Record<string, string[]>) => void;
}

/**
 * Declare a group: which services, and in what order.
 *
 * One list rather than two. A picked-from and a picked-into column is the
 * usual shape, but it is half empty in both directions until a group is half
 * built, and a project with two services shows two mostly blank panels. The
 * single list is what a scheme's build order looks like: everything the
 * project has, the chosen ones numbered and on top, the rest below waiting to
 * be ticked.
 *
 * Members are ticked rather than typed. A typed list of names can name a
 * service that is not there, spell one wrong, or list one twice, and none of
 * that shows until the group is run. Ticking cannot express any of it.
 */
export function GroupEditor({ services, existing, editing, onCancel, onConfirm }: Props) {
  const [name, setName] = useState(editing?.name ?? "");
  const [steps, setSteps] = useState<string[]>(editing?.steps ?? []);
  // What each member waits for, seeded from the services' own dependencies —
  // which is where it is stored, and where saving puts it back.
  const [after, setAfter] = useState<Record<string, string[]>>(() =>
    Object.fromEntries(
      services.map((view) => [view.name, [...(view.depends_on ?? [])]]),
    ),
  );

  const chosen = steps.filter((step) => services.some((view) => view.name === step));
  const rest = services.filter((view) => !steps.includes(view.name)).map((view) => view.name);

  const clash = existing.find(
    (task) => task.name === name.trim() && task.name !== editing?.name,
  );
  const ready = name.trim() !== "" && !clash && chosen.length > 0;

  function move(index: number, by: number) {
    const to = index + by;
    if (to < 0 || to >= chosen.length) return;
    const next = [...chosen];
    [next[index], next[to]] = [next[to], next[index]];
    setSteps(next);
  }

  /** Would making `step` wait for `dep` close a loop? */
  function loops(step: string, dep: string): boolean {
    const seen = new Set<string>();
    const walk = (from: string): boolean => {
      if (from === step) return true;
      if (seen.has(from)) return false;
      seen.add(from);
      return (after[from] ?? []).some(walk);
    };
    return walk(dep);
  }

  function toggleAfter(step: string, dep: string) {
    const waits = after[step] ?? [];
    setAfter({
      ...after,
      [step]: waits.includes(dep) ? waits.filter((one) => one !== dep) : [...waits, dep],
    });
  }

  return (
    <div className="sheet-backdrop" onClick={onCancel}>
      <div className="sheet narrow" onClick={(event) => event.stopPropagation()}>
        <header className="sheet-head">
          <h2>{editing ? `Edit ${editing.name}` : "New group"}</h2>
        </header>

        <div className="sheet-body">
          <label className="field wide">
            <span>Name</span>
            <input
              autoFocus
              type="text"
              value={name}
              placeholder="dev"
              spellCheck={false}
              onChange={(event) => setName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") onCancel();
              }}
            />
          </label>
          {clash && (
            <p className="hint problem">This project already has a group called {clash.name}.</p>
          )}

          <div className="field wide">
            <span>Services</span>
            {services.length === 0 ? (
              <p className="hint">This project has no services yet.</p>
            ) : (
              <ul className="group-list">
                {chosen.map((step, index) => (
                  <li key={step} className="chosen">
                    <input
                      type="checkbox"
                      checked
                      aria-label={`Remove ${step} from the group`}
                      onChange={() => setSteps(chosen.filter((other) => other !== step))}
                    />
                    <span className="step-order">{index + 1}</span>
                    <span className="step-name">{step}</span>
                    <button
                      type="button"
                      className="ghost tiny"
                      title="Start earlier"
                      disabled={index === 0}
                      onClick={() => move(index, -1)}
                    >
                      ↑
                    </button>
                    <button
                      type="button"
                      className="ghost tiny"
                      title="Start later"
                      disabled={index === chosen.length - 1}
                      onClick={() => move(index, 1)}
                    >
                      ↓
                    </button>
                    {/* What it waits for. Stored on the service, so it holds
                        wherever else the service is used — and the diagram is
                        read from the same place rather than kept alongside. */}
                    <div className="step-after">
                      <span className="step-after-label">after</span>
                      {chosen.filter((other) => other !== step).length === 0 ? (
                        <span className="step-after-none">nothing</span>
                      ) : (
                        chosen
                          .filter((other) => other !== step)
                          .map((other) => {
                            const on = (after[step] ?? []).includes(other);
                            const cyclic = !on && loops(step, other);
                            return (
                              <button
                                type="button"
                                key={other}
                                className={on ? "chip on" : "chip"}
                                disabled={cyclic}
                                title={
                                  cyclic
                                    ? `${other} already waits for ${step}`
                                    : `Wait for ${other}`
                                }
                                onClick={() => toggleAfter(step, other)}
                              >
                                {other}
                              </button>
                            );
                          })
                      )}
                    </div>
                  </li>
                ))}
                {rest.map((service) => (
                  <li key={service}>
                    <input
                      type="checkbox"
                      checked={false}
                      aria-label={`Add ${service} to the group`}
                      onChange={() => setSteps([...chosen, service])}
                    />
                    <span className="step-order" />
                    <span className="step-name">{service}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>

          <p className="hint">
            {chosen.length === 0
              ? "Tick the services this group starts."
              : "Anything waiting for nothing starts at once; the rest follow what they wait for, and the group stops in reverse. A service already up when its turn comes is left alone. What a member waits for is its own dependency, so it holds outside this group too."}
          </p>
        </div>

        <footer className="sheet-foot">
          <span className="spacer" />
          <button className="ghost" onClick={onCancel}>
            Cancel
          </button>
          <button
            className="ghost primary"
            disabled={!ready}
            onClick={() => ready && onConfirm(name.trim(), chosen, after)}
          >
            {editing ? "Save" : "Create"}
          </button>
        </footer>
      </div>
    </div>
  );
}

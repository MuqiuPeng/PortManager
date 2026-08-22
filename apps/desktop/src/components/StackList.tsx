import type { StackView } from "../types";
import { Button } from "@/components/ui/button";

/** The selection that means "the ones no stack names". */
export const LOOSE = " loose";

interface Props {
  stacks: StackView[];
  /** How many services this checkout has, for the "everything" row. */
  total: number;
  /** How many belong to no stack. Zero hides the row rather than showing nil. */
  loose: number;
  selected: string | null;
  busy: boolean;
  onSelect: (name: string | null) => void;
  onRun: (name: string) => void;
  onStop: (name: string) => void;
  onEdit: (name: string) => void;
  onRemove: (name: string) => void;
  onNew: () => void;
}

/**
 * The stacks of a checkout, as the thing you pick from.
 *
 * A stack is what somebody declared this project is brought up as, so it is
 * the left-hand column and choosing one narrows what is shown beside it.
 * Nothing chosen means everything: a project with three stacks still has one
 * list of services, and sometimes that is what you came to look at.
 *
 * The state and the buttons live on the stack rather than on each of its
 * members. Five services declared as one thing were five rows and five clicks,
 * which is the arithmetic declaring a stack exists to remove.
 */
export function StackList({
  stacks,
  total,
  loose,
  selected,
  busy,
  onSelect,
  onRun,
  onStop,
  onEdit,
  onRemove,
  onNew,
}: Props) {
  return (
    <nav className="stacks" aria-label="Stacks">
      <button
        className={selected === null ? "stack-pick active" : "stack-pick"}
        onClick={() => onSelect(null)}
      >
        <span className="stack-pick-name">All services</span>
        <span className="stack-pick-count">{total}</span>
      </button>

      {stacks.map((stack) => {
        const members = stack.members.length;
        const allUp = members > 0 && stack.running === members;
        const someUp = stack.running > 0;
        const flow = stack.flow ?? [];
        // A stack of nothing but one-shots is never "up": one that has run is
        // not running and never will be.
        const ready = flow.length > 0 && flow.every((node) => node.one_shot);
        const chosen = selected === stack.name;
        return (
          <div className={chosen ? "stack-pick active" : "stack-pick"} key={stack.id}>
            <button
              className="stack-pick-main"
              onClick={() => onSelect(chosen ? null : stack.name)}
            >
              <span
                className={allUp ? "dot status-healthy" : someUp ? "dot partial" : "dot"}
                aria-hidden
              />
              <span className="stack-pick-name">{stack.name}</span>
              <span className="stack-pick-count">
                {stack.running}/{members} {ready ? "ready" : "up"}
              </span>
              {(stack.missing ?? []).length > 0 && (
                <span
                  className="stack-pick-missing"
                  title={`missing ${(stack.missing ?? []).join(", ")}`}
                >
                  !
                </span>
              )}
            </button>

            {/* Rendered only for the one being looked at, not hidden with a
                rule: a button that is display:none is still in the page for
                anything reading or tabbing through it, so the wall is still
                there for the people most likely to hit it. */}
            {chosen && (
            <div className="stack-pick-actions">
              {someUp ? (
                <Button variant="destructive" size="sm" disabled={busy} onClick={() => onStop(stack.name)}>
                  Stop
                </Button>
              ) : (
                <Button variant="default" size="sm" disabled={busy} onClick={() => onRun(stack.name)}>
                  Start
                </Button>
              )}
              <Button
                variant="outline" size="sm"
                disabled={busy}
                onClick={() => onEdit(stack.name)}
                title="Change what is in this stack"
              >
                Edit
              </Button>
              <Button
                variant="outline" size="sm"
                disabled={busy}
                onClick={() => onRemove(stack.name)}
                title="Delete this stack"
              >
                &minus;
              </Button>
            </div>
            )}
          </div>
        );
      })}

      {loose > 0 && (
        <button
          className={selected === LOOSE ? "stack-pick active loose-pick" : "stack-pick loose-pick"}
          onClick={() => onSelect(selected === LOOSE ? null : LOOSE)}
          title="No stack names these, so they cannot be started until one does"
        >
          <span className="stack-pick-name">Not in a stack</span>
          <span className="stack-pick-count">{loose}</span>
        </button>
      )}

      <Button variant="ghost" size="sm" className="justify-start" disabled={busy} onClick={onNew}>
        + Stack
      </Button>
    </nav>
  );
}

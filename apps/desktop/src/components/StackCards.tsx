import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { StackView } from "../types";

/** The selection that means "the ones no stack names". */
export const LOOSE = " loose";

interface Props {
  stacks: StackView[];
  /** How many services this checkout has, for the "everything" card. */
  total: number;
  /** How many belong to no stack. Zero hides the card rather than showing nil. */
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
 * The ways this project is brought up, across the top of the stack column.
 *
 * A stack is the unit: what somebody declared this project is started as, and
 * what the panel will start. The cards are small on purpose — a name, a state,
 * one button — because the room belongs to the thing underneath them: a stack
 * is a graph, and the interesting ones have several steps, a one-shot at the
 * front, a fan-out in the middle.
 *
 * Each card carries its own state and its own start button, which is the
 * arithmetic declaring a stack removes: five services grouped into one thing
 * were five rows and five clicks.
 */
export function StackCards({
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
    <div className="grid grid-cols-[repeat(auto-fill,minmax(10rem,1fr))] gap-2">
      <button
        onClick={() => onSelect(null)}
        className={cn(
          "flex flex-col justify-between gap-1 rounded-lg border px-3 py-2 text-left transition-colors",
          selected === null ? "border-ring bg-accent" : "hover:bg-accent/50",
        )}
      >
        <span className="truncate text-sm">All services</span>
        <span className="font-mono text-[11px] text-muted-foreground">
          {total} {total === 1 ? "service" : "services"}
        </span>
      </button>

      {stacks.map((stack) => {
        const flow = stack.flow ?? [];
        // A one-shot has no steady state, so it is no part of how much of this
        // is up: it is run, not started. A stack of nothing but one-shots has
        // no up-ness at all — which is why this one used to show a live dot and
        // a Stop button for a migration that had never been executed.
        const stays = flow.filter((node) => !node.one_shot).length;
        const oneShots = flow.length - stays;
        const allUp = stays > 0 && stack.running === stays;
        const someUp = stack.running > 0;
        const chosen = selected === stack.name;
        const missing = stack.missing ?? [];

        return (
          <div
            key={stack.id}
            className={cn(
              "flex flex-col justify-between gap-1 rounded-lg border px-3 py-2 transition-colors",
              chosen ? "border-ring bg-accent" : "hover:bg-accent/50",
            )}
          >
            <button
              onClick={() => onSelect(chosen ? null : stack.name)}
              className="flex items-center gap-2 text-left"
            >
              <span
                className={cn(
                  "size-2 shrink-0 rounded-full",
                  allUp ? "bg-live" : someUp ? "bg-warn" : "bg-muted-foreground/40",
                )}
                aria-hidden
              />
              <span className="flex-1 truncate text-sm">{stack.name}</span>
              {missing.length > 0 && (
                <span className="text-warn" title={`missing ${missing.join(", ")}`}>
                  !
                </span>
              )}
            </button>

            <div className="flex items-center gap-1">
              <span className="flex-1 truncate font-mono text-[11px] text-muted-foreground">
                {stays === 0
                  ? `${oneShots} to run`
                  : `${stack.running}/${stays} up${oneShots > 0 ? ` +${oneShots}` : ""}`}
              </span>
              {someUp ? (
                <Button
                  variant="destructive"
                  size="sm"
                  className="h-6 px-2 text-[11px]"
                  disabled={busy}
                  onClick={() => onStop(stack.name)}
                >
                  Stop
                </Button>
              ) : (
                <Button
                  variant="default"
                  size="sm"
                  className="h-6 px-2 text-[11px]"
                  disabled={busy}
                  onClick={() => onRun(stack.name)}
                  title={stays === 0 ? "Run it once, now" : undefined}
                >
                  {stays === 0 ? "Run" : "Start"}
                </Button>
              )}
              {/* Only on the one being looked at, and not rendered otherwise:
                  a hidden button is still there for anything reading the page
                  or tabbing through it. */}
              {chosen && (
                <>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="size-6"
                    disabled={busy}
                    onClick={() => onEdit(stack.name)}
                    title="Change what is in this stack"
                  >
                    ✎
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="size-6"
                    disabled={busy}
                    onClick={() => onRemove(stack.name)}
                    title="Delete this stack"
                  >
                    &minus;
                  </Button>
                </>
              )}
            </div>
          </div>
        );
      })}

      {loose > 0 && (
        <button
          onClick={() => onSelect(selected === LOOSE ? null : LOOSE)}
          title="No stack names these, so they cannot be started until one does"
          className={cn(
            "flex flex-col justify-between gap-1 rounded-lg border border-dashed px-3 py-2 text-left transition-colors",
            selected === LOOSE ? "border-ring bg-accent" : "hover:bg-accent/50",
          )}
        >
          <span className="truncate text-sm text-muted-foreground">Not in a stack</span>
          <span className="font-mono text-[11px] text-muted-foreground">
            {loose} {loose === 1 ? "service" : "services"}
          </span>
        </button>
      )}

      {/* The same shape as the cards beside it: a button of another size in a
          row of cards reads as another kind of thing, which it is not. */}
      <button
        disabled={busy}
        onClick={onNew}
        className="flex items-center justify-center rounded-lg border border-dashed py-2 text-sm text-muted-foreground transition-colors hover:bg-accent/50 disabled:opacity-50"
      >
        + Stack
      </button>
    </div>
  );
}

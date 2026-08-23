import { Button } from "@/components/ui/button";
interface Props {
  port: number;
  /** Another supervisor already keeping this alive. */
  supervisor?: string;
  busy: boolean;
  onCancel: () => void;
  /** `restart` stops what is running and starts it here instead. */
  onConfirm: (force: boolean, restart: boolean) => void;
}

/**
 * Ask before taking a running service over.
 *
 * The runtime does not stop what it did not start, so declaring the service is
 * how it becomes startable from here — worth a sentence rather than a silent
 * button, because what it records is a command, and a wrong one is expensive.
 *
 * When another supervisor already holds the service the answer is different
 * again: the runtime can drive that supervisor directly, so declaring it here
 * adds a second definition of one service rather than any new ability. The
 * banner says so instead of recommending it.
 *
 * Nothing is stopped or restarted either way. Adopting is about being able to.
 */
export function TakeControlSheet({
  port,
  supervisor,
  busy,
  onCancel,
  onConfirm,
}: Props) {
  return (
    <div className="sheet-backdrop" onClick={onCancel}>
      <div className="sheet" onClick={(event) => event.stopPropagation()}>
        <div className="sheet-head">
          <h2>Take control of :{port}</h2>
        </div>

        <div className="sheet-body">
          <p>
            <strong>Take over</strong> stops what is running and starts it here
            instead, so it can be stopped and restarted from this window. There
            is a moment where the port is free.
          </p>

          <p>
            <strong>Just record it</strong> writes down how it is running and
            leaves it alone. It stays outside the runtime — this window can show
            it, and cannot stop it.
          </p>

          <p className="hint">
            The command is read off the running process, never guessed from
            package.json — a project whose <code>dev</code> and{" "}
            <code>start</code> scripts share a build directory is left unable to
            boot if it is adopted under the wrong one.
          </p>

          {supervisor && (
            <div className="banner inline">
              {supervisor} is keeping this alive, and the runtime can already
              start, stop and restart it through {supervisor} — the buttons on
              its row do that. Declaring it here as well would give this
              checkout a second definition of the same service, and starting
              that one would fight with {supervisor} for the port.
            </div>
          )}
        </div>

        <div className="sheet-foot">
          <span className="spacer" />
          <Button variant="outline" size="sm" onClick={onCancel} disabled={busy}>
            Cancel
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => onConfirm(Boolean(supervisor), false)}
            disabled={busy}
          >
            Just record it
          </Button>
          {/* Not offered when another supervisor holds it: stopping it here is
              undone the moment that supervisor notices, and the runtime can
              drive it directly instead. */}
          {!supervisor && (
            <Button
              variant="default"
              size="sm"
              onClick={() => onConfirm(false, true)}
              disabled={busy}
            >
              Take over
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}

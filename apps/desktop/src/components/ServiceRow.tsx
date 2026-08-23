import { isLive, type ServiceView, type StartedBy } from "../types";
import { Button } from "@/components/ui/button";

interface Props {
  service: ServiceView;
  selected: boolean;
  busy: boolean;
  /** False when no stack names it, which is what makes it unstartable. */
  inAStack: boolean;
  /** Offered instead of Start, to make it startable. */
  onAddToStack: () => void;
  onSelect: () => void;
  onOpen: () => void;
  onEdit: () => void;
  onTakeControl: () => void;
  onSupervisedControl: (action: "start" | "stop" | "restart") => void;
}

const OWNER_LABELS: Record<StartedBy, string> = {
  manual: "manually",
  desktop: "this app",
  cli: "the CLI",
  "claude-code": "Claude Code",
  codex: "Codex",
  cursor: "Cursor",
  unknown: "an unknown caller",
};

export function ServiceRow({
  service,
  selected,
  busy,
  inAStack,
  onAddToStack,
  onSelect,
  onOpen,
  onEdit,
  onTakeControl,
  onSupervisedControl,
}: Props) {
  const live = isLive(service.status);
  const owner = service.instance?.started_by;
  // Found already listening: real, but not ours to stop or restart.
  const external = live && service.managed === false;
  // Unless somebody else can be asked to. A stop routed through the supervisor
  // that owns this is a stop that sticks, where one issued here would be undone
  // the moment that supervisor noticed.
  const viaSupervisor = external ? service.supervisor_entry : undefined;
  const dependencies = service.depends_on ?? [];
  // A step that runs to completion is not a service that is down when it is
  // not running, so it does not get a status dot arguing that it is.
  const oneShot = service.one_shot === true;

  return (
    <div
      className={selected ? "service selected" : "service"}
      onClick={onSelect}
      role="button"
      tabIndex={0}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") onSelect();
      }}
    >
      <span
        className={oneShot ? "dot one-shot" : `dot status-${service.status}`}
        aria-hidden
      />

      <span className="service-body">
        <span className="service-name">{service.name}</span>
        <span className="service-meta">
          {oneShot
            ? // "Did it work?" is the only question a step like this raises,
              // so the row answers that rather than reporting it as stopped.
              service.instance === undefined
                ? "runs to completion · not run yet"
                : service.instance.exit_code === 0
                  ? "ran successfully"
                  : `last run failed (exit ${service.instance.exit_code ?? "?"})`
            : service.status}
          {/* Who started it is the answer to "why is this running?", so it sits
              next to the status rather than hidden in a detail pane. */}
          {external
            ? // Naming the supervisor is the answer to "why is there no Stop
              // button?". Without it the row says only that the runtime is not
              // in charge, which reads as a limitation rather than a fact about
              // the machine: a stop issued here would be undone in a second.
              service.supervisor
              ? ` · kept alive by ${service.supervisor}`
              : " · not started by the runtime"
            : live && owner && owner !== "unknown"
              ? ` · started by ${OWNER_LABELS[owner]}`
              : ""}
        </span>
        {dependencies.length > 0 && (
          /* What it waits for, where somebody looking at a slow start will
             look first. */
          <span className="service-deps">after {dependencies.join(", ")}</span>
        )}
      </span>

      <span className="service-port">
        {service.actual_port ? `:${service.actual_port}` : "—"}
      </span>

      <span className="service-actions" onClick={(event) => event.stopPropagation()}>
        {/* Detection guesses; this is where the guess gets corrected. */}
        <Button variant="outline" size="sm" onClick={onEdit} title="Edit how this starts">
          Edit
        </Button>
        {service.url && live && (
          <Button variant="outline" size="sm" onClick={onOpen} title={service.url}>
            Open
          </Button>
        )}
        {/* Another supervisor's entry is not the runtime's to run as part of a
            stack — it answers to whatever holds it, and these drive that. */}
        {viaSupervisor && (
          <>
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => onSupervisedControl(live ? "restart" : "start")}
              title={`Through ${service.supervisor}`}
            >
              {live ? "Restart" : "Start"}
            </Button>
            {live && (
              <Button
                variant="destructive"
                size="sm"
                disabled={busy}
                onClick={() => onSupervisedControl("stop")}
                title={`Through ${service.supervisor}`}
              >
                Stop
              </Button>
            )}
          </>
        )}

        {/* Running, and not the runtime's. Taking it over is about who owns a
            process that already exists rather than about bringing one up, so
            the stack rule has nothing to say to it. */}
        {live && !service.managed && !viaSupervisor && (
          <Button variant="outline" size="sm" disabled={busy} onClick={onTakeControl}>
            Take control
          </Button>
        )}

        {!inAStack && (
          <Button
            variant="ghost"
            size="sm"
            disabled={busy}
            onClick={onAddToStack}
            title="A service runs as part of a stack; this one is in none"
          >
            Add to a stack
          </Button>
        )}

        {/* No Start, Stop or Restart of the service itself: it runs as part of
            a stack, so those live on the stack. A button here would offer what
            the daemon refuses, and the row would be arguing with it. */}
      </span>
    </div>
  );
}

import { isLive, type ServiceView, type StartedBy } from "../types";

interface Props {
  service: ServiceView;
  selected: boolean;
  busy: boolean;
  onSelect: () => void;
  onStart: () => void;
  onStop: () => void;
  onRestart: () => void;
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
  onSelect,
  onStart,
  onStop,
  onRestart,
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
      <span className={`dot status-${service.status}`} aria-hidden />

      <span className="service-body">
        <span className="service-name">{service.name}</span>
        <span className="service-meta">
          {service.status}
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
      </span>

      <span className="service-port">
        {service.actual_port ? `:${service.actual_port}` : "—"}
      </span>

      <span className="service-actions" onClick={(event) => event.stopPropagation()}>
        {/* Detection guesses; this is where the guess gets corrected. */}
        <button className="ghost" onClick={onEdit} title="Edit how this starts">
          Edit
        </button>
        {service.url && live && (
          <button className="ghost" onClick={onOpen} title={service.url}>
            Open
          </button>
        )}
        {viaSupervisor ? (
          /* Not the runtime's process, but the supervisor holding it takes
             orders — so these do what the buttons say. */
          <>
            <button
              className="ghost"
              onClick={() => onSupervisedControl("restart")}
              disabled={busy}
              title={`Restart via ${service.supervisor}`}
            >
              Restart
            </button>
            <button
              className="ghost danger"
              onClick={() => onSupervisedControl("stop")}
              disabled={busy}
              title={`Stop via ${service.supervisor}`}
            >
              Stop
            </button>
          </>
        ) : external ? (
          /* Nobody to ask, so the way out is to declare it — with the command
             read off the running process, never guessed from the scripts. */
          <button
            className="ghost"
            onClick={onTakeControl}
            disabled={busy}
            title="Declare this so the runtime can start it again"
          >
            Take control
          </button>
        ) : live ? (
          <>
            <button className="ghost" onClick={onRestart} disabled={busy}>
              Restart
            </button>
            <button className="ghost danger" onClick={onStop} disabled={busy}>
              Stop
            </button>
          </>
        ) : (
          <button className="ghost primary" onClick={onStart} disabled={busy}>
            Start
          </button>
        )}
      </span>
    </div>
  );
}

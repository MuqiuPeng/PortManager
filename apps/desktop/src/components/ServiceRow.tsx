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
}: Props) {
  const live = isLive(service.status);
  const owner = service.instance?.started_by;

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
          {live && owner && owner !== "unknown" ? ` · started by ${OWNER_LABELS[owner]}` : ""}
        </span>
      </span>

      <span className="service-port">
        {service.actual_port ? `:${service.actual_port}` : "—"}
      </span>

      <span className="service-actions" onClick={(event) => event.stopPropagation()}>
        {service.url && live && (
          <button className="ghost" onClick={onOpen} title={service.url}>
            Open
          </button>
        )}
        {live ? (
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

import type { SupervisedView } from "../types";

interface Props {
  entry: SupervisedView;
  busy: boolean;
  onControl: (action: "start" | "stop" | "restart") => void;
  onOpen: () => void;
}

/**
 * A service another supervisor keeps, that this app can switch.
 *
 * Shown as its own kind of row rather than as a declared service, for the same
 * reason a container is: PM2 decided what this is, and PM2 decides whether it
 * comes back after a reboot. What the runtime offers is the reversible half —
 * start, stop, restart — which leaves PM2's registry untouched. There is no
 * Delete here on purpose; removing an entry is usually also what stops it
 * starting at boot, and that belongs to whoever set the machine up.
 */
export function SupervisedRow({ entry, busy, onControl, onOpen }: Props) {
  const live = entry.status === "online" || entry.status === "launching";
  // Absent rather than empty when it holds none: the daemon omits empty lists,
  // and reading `.length` off the gap takes the whole window down with it.
  const ports = entry.ports ?? [];

  return (
    <div className="service supervised">
      <span className={live ? "dot status-healthy" : "dot"} aria-hidden />

      <span className="service-body">
        <span className="service-name">
          {entry.name}
          <span className="badge">{entry.supervisor}</span>
        </span>
        <span className="service-meta">
          {entry.status}
          {entry.restarts > 0 && ` · ${entry.restarts} restarts`}
        </span>
        {/* On the row, not behind a click: it is only useful before somebody
            presses Restart, and by then they have decided. */}
        {entry.restart_warning && (
          <span className="service-warning">{entry.restart_warning}</span>
        )}
      </span>

      <span className="service-port">
        {ports.length > 0 ? ports.map((p) => `:${p}`).join(" ") : "—"}
      </span>

      <span className="service-actions" onClick={(event) => event.stopPropagation()}>
        {entry.url && live && (
          <button className="ghost" onClick={onOpen} title={entry.url}>
            Open
          </button>
        )}
        {live ? (
          <>
            <button
              className="ghost"
              onClick={() => onControl("restart")}
              disabled={busy}
              title={entry.restart_warning ?? undefined}
            >
              Restart
            </button>
            <button
              className="ghost danger"
              onClick={() => onControl("stop")}
              disabled={busy}
            >
              Stop
            </button>
          </>
        ) : (
          <button
            className="ghost primary"
            onClick={() => onControl("start")}
            disabled={busy}
          >
            Start
          </button>
        )}
      </span>
    </div>
  );
}

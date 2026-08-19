import { openExternal } from "../api";
import type { ContainerView } from "../types";

interface Props {
  container: ContainerView;
  busy: boolean;
  onControl: (action: "start" | "stop" | "restart") => void;
}

/**
 * A container compose defines for this checkout.
 *
 * It has a switch even though the runtime did not create it: `docker stop` is a
 * graceful operation on a named, restartable object, unlike signalling a pid —
 * which is why a process started elsewhere has no such button.
 */
export function ContainerRow({ container, busy, onControl }: Props) {
  const running = container.status === "running";
  const ports = container.ports ?? [];

  return (
    <div className="service container-row">
      <span className={running ? "dot live" : "dot"} aria-hidden />

      <span className="service-body">
        <span className="service-name">{container.service ?? container.name}</span>
        <span className="service-meta">
          {container.status}
          {container.health && ` · ${container.health}`}
          <span className="container-tag">container</span>
        </span>
      </span>

      <span className="service-port">
        {ports.length > 0 ? ports.map((port) => `:${port}`).join(" ") : "—"}
      </span>

      <span className="service-actions" onClick={(event) => event.stopPropagation()}>
        {running && container.url && (
          <button
            className="ghost"
            title={container.url}
            onClick={() => void openExternal(container.url as string)}
          >
            Open
          </button>
        )}
        {running ? (
          <>
            <button className="ghost" disabled={busy} onClick={() => onControl("restart")}>
              Restart
            </button>
            <button className="ghost danger" disabled={busy} onClick={() => onControl("stop")}>
              Stop
            </button>
          </>
        ) : (
          <button className="ghost primary" disabled={busy} onClick={() => onControl("start")}>
            Start
          </button>
        )}
      </span>
    </div>
  );
}

import { openExternal } from "../api";
import type { ContainerView } from "../types";
import { Button } from "@/components/ui/button";

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
          <Button
            variant="outline" size="sm"
            title={container.url}
            onClick={() => void openExternal(container.url as string)}
          >
            Open
          </Button>
        )}
        {running ? (
          <>
            <Button variant="outline" size="sm" disabled={busy} onClick={() => onControl("restart")}>
              Restart
            </Button>
            <Button variant="destructive" size="sm" disabled={busy} onClick={() => onControl("stop")}>
              Stop
            </Button>
          </>
        ) : (
          <Button variant="default" size="sm" disabled={busy} onClick={() => onControl("start")}>
            Start
          </Button>
        )}
      </span>
    </div>
  );
}

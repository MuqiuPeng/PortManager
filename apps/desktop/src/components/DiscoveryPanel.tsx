import type { Discovery } from "../types";
import { Button } from "@/components/ui/button";

interface Props {
  discoveries: Discovery[];
  scanning: boolean;
  busy: boolean;
  onAdd: (discovery: Discovery) => void;
  onAddAll: () => void;
  onRescan: () => void;
  onAddByPath: () => void;
}

/**
 * What the runtime found on its own.
 *
 * Shown instead of an empty project list, and reachable later from the sidebar.
 * Projects that are listening right now come first: those are the ones the user
 * is trying to make sense of.
 */
export function DiscoveryPanel({
  discoveries,
  scanning,
  busy,
  onAdd,
  onAddAll,
  onRescan,
  onAddByPath,
}: Props) {
  const found = discoveries ?? [];
  const unregistered = found.filter((item) => !item.registered);

  return (
    <section className="discovery">
      <header className="discovery-head">
        <div>
          <h1>Found on this machine</h1>
          <p className="discovery-sub">
            {scanning
              ? "Scanning…"
              : found.length === 0
                ? "Nothing found yet. Anything listening on a port is detected automatically."
                : `${found.length} project${found.length === 1 ? "" : "s"}, ${unregistered.length} not added yet.`}
          </p>
        </div>
        <div className="discovery-actions">
          <Button variant="outline" size="sm" onClick={onRescan} disabled={scanning || busy}>
            Rescan
          </Button>
          <Button variant="outline" size="sm" onClick={onAddByPath} disabled={busy}>
            Scan a folder…
          </Button>
          {unregistered.length > 0 && (
            <Button variant="default" size="sm" onClick={onAddAll} disabled={busy}>
              Add all
            </Button>
          )}
        </div>
      </header>

      <div className="discovery-list">
        {found.map((item) => (
          <div className="discovery-item" key={item.root_path}>
            <span className={item.running ? "dot live" : "dot"} aria-hidden />

            <div className="discovery-body">
              <div className="discovery-title">
                <span className="discovery-name">{item.name}</span>
                {item.git_branch && <span className="branch">{item.git_branch}</span>}
                {(item.ports ?? []).map((port) => (
                  <span className="badge port" key={port}>
                    :{port}
                  </span>
                ))}
              </div>
              <div className="path">{item.root_path}</div>
              {(item.suggested_services ?? []).length > 0 && (
                <div className="discovery-services">
                  services: {(item.suggested_services ?? []).join(", ")}
                </div>
              )}
            </div>

            {item.registered ? (
              <span className="discovery-added">added</span>
            ) : (
              <Button variant="default" size="sm" onClick={() => onAdd(item)} disabled={busy}>
                Add
              </Button>
            )}
          </div>
        ))}
      </div>
    </section>
  );
}

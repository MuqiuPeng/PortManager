import type { Discovery } from "../types";

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
  const unregistered = discoveries.filter((item) => !item.registered);

  return (
    <section className="discovery">
      <header className="discovery-head">
        <div>
          <h1>Found on this machine</h1>
          <p className="discovery-sub">
            {scanning
              ? "Scanning…"
              : discoveries.length === 0
                ? "Nothing found yet. Anything listening on a port is detected automatically."
                : `${discoveries.length} project${discoveries.length === 1 ? "" : "s"}, ${unregistered.length} not added yet.`}
          </p>
        </div>
        <div className="discovery-actions">
          <button className="ghost" onClick={onRescan} disabled={scanning || busy}>
            Rescan
          </button>
          <button className="ghost" onClick={onAddByPath} disabled={busy}>
            Scan a folder…
          </button>
          {unregistered.length > 0 && (
            <button className="ghost primary" onClick={onAddAll} disabled={busy}>
              Add all
            </button>
          )}
        </div>
      </header>

      <div className="discovery-list">
        {discoveries.map((item) => (
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
              <button className="ghost primary" onClick={() => onAdd(item)} disabled={busy}>
                Add
              </button>
            )}
          </div>
        ))}
      </div>
    </section>
  );
}

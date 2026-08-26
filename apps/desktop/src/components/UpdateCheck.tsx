import { useCallback, useEffect, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

import { Button } from "@/components/ui/button";
import { errorMessage } from "../api";

type State =
  | { at: "asking" }
  | { at: "current" }
  | { at: "found"; update: Update }
  | { at: "downloading"; done: number; total: number | null }
  | { at: "installed" }
  | { at: "failed"; why: string };

/**
 * Whether there is a newer version, and the button that takes it.
 *
 * Everything under this was already in place — the release publishes a signed
 * `latest.json`, the app carries the public key that verifies it, and the
 * updater plugin is registered — and nothing ever asked. A release nobody is
 * told about is a release nobody installs.
 *
 * Asked once when this screen opens rather than on a timer at launch: a
 * check is a request to GitHub, and doing it behind somebody's back every
 * time they open the app is not the kind of thing a local tool should do
 * without being looked at.
 */
export function UpdateCheck({ running }: { running: string }) {
  const [state, setState] = useState<State>({ at: "asking" });

  const ask = useCallback(async () => {
    setState({ at: "asking" });
    try {
      const update = await check();
      setState(update ? { at: "found", update } : { at: "current" });
    } catch (err) {
      // Offline, rate-limited, or no release yet. None of those are worth an
      // alarm — the app works exactly as well without an update as with one.
      setState({ at: "failed", why: errorMessage(err) });
    }
  }, []);

  useEffect(() => {
    void ask();
  }, [ask]);

  async function install(update: Update) {
    setState({ at: "downloading", done: 0, total: null });
    try {
      let done = 0;
      let total: number | null = null;
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? null;
          setState({ at: "downloading", done: 0, total });
        } else if (event.event === "Progress") {
          done += event.data.chunkLength;
          setState({ at: "downloading", done, total });
        } else if (event.event === "Finished") {
          setState({ at: "installed" });
        }
      });
      setState({ at: "installed" });
    } catch (err) {
      setState({ at: "failed", why: errorMessage(err) });
    }
  }

  return (
    <section className="settings-group">
      <h2>Version</h2>
      <dl className="facts">
        <dt>Installed</dt>
        <dd>{running}</dd>
        <dt>Update</dt>
        <dd>
          {state.at === "asking" && <span className="muted">checking…</span>}

          {state.at === "current" && (
            <span className="muted">
              up to date{" "}
              <button className="linklike" onClick={() => void ask()}>
                check again
              </button>
            </span>
          )}

          {state.at === "found" && (
            <span className="update-offer">
              <span>{state.update.version} is available</span>
              <Button size="sm" onClick={() => void install(state.update)}>
                Update
              </Button>
            </span>
          )}

          {state.at === "downloading" && (
            <span className="muted">
              downloading
              {state.total
                ? ` — ${Math.round((state.done / state.total) * 100)}%`
                : ` — ${Math.round(state.done / 1024 / 1024)} MB`}
            </span>
          )}

          {state.at === "installed" && (
            <span className="update-offer">
              <span>installed — restart to run it</span>
              <Button size="sm" onClick={() => void relaunch()}>
                Restart
              </Button>
            </span>
          )}

          {state.at === "failed" && (
            <span className="muted">
              could not check: {state.why}{" "}
              <button className="linklike" onClick={() => void ask()}>
                try again
              </button>
            </span>
          )}
        </dd>
      </dl>

      {/* Said next to the button, because it is the question somebody about to
          press it has: this replaces the window, and the daemon it started is
          a separate process that outlives it. Services keep running across the
          restart — which is the whole reason the daemon is not inside the
          app. */}
      <p className="hint">
        Updating replaces the app and restarts the window. Services keep
        running: the daemon is its own process and does not go down with it.
      </p>
    </section>
  );
}

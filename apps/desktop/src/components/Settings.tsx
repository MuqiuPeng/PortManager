import { useEffect, useState } from "react";

import { api, errorMessage } from "../api";
import type { DaemonInfo, PanelSettings, ScreenInfo } from "../types";

/**
 * Panel geometry and the global shortcut.
 *
 * Saved through the daemon rather than beside the app, so settings survive
 * reinstalling the bundle. Each change is applied immediately — a panel you
 * have to press Save to preview is a panel you tune by guesswork.
 */
export function Settings() {
  const [settings, setSettings] = useState<PanelSettings | null>(null);
  // Null until asked. The platform's own answer, not a guess from the user
  // agent: only the app knows whether it has a way to place a panel.
  const [hasPanel, setHasPanel] = useState<boolean | null>(null);
  const [screens, setScreens] = useState<ScreenInfo[]>([]);
  const [info, setInfo] = useState<DaemonInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    void (async () => {
      try {
        setHasPanel(await api.panelSupported());
        setSettings(await api.getPanelSettings());
        setScreens(await api.listScreens());
        setInfo(await api.daemonInfo());
      } catch (err) {
        setError(errorMessage(err));
      }
    })();
  }, []);

  async function update(patch: Partial<PanelSettings>) {
    if (!settings) return;
    const next = { ...settings, ...patch };
    const previous = settings;
    setSettings(next);
    setError(null);
    try {
      await api.setPanelSettings(next);
      setSaved(true);
      window.setTimeout(() => setSaved(false), 1200);
    } catch (err) {
      // A refused shortcut is the common case; put the old value back rather
      // than showing a setting that is not in force.
      setSettings(previous);
      setError(errorMessage(err));
    }
  }

  if (!settings) {
    return <p className="empty">{error ?? "Loading…"}</p>;
  }

  return (
    <div className="settings">
      {error && <div className="banner inline">{error}</div>}

      <section className="settings-group">
        <h2>
          Panel
          {saved && <span className="settings-saved">saved</span>}
        </h2>

        {/* Said, not left blank. A section that silently disappears reads as
            something broken; one line explains why there is nothing to set. */}
        {hasPanel === false && (
          <p className="empty">
            The edge panel is a macOS feature. Everything else works the same
            here.
          </p>
        )}

        {hasPanel && (
          <label className="field">
            <span>Edge panel</span>
            <input
              type="checkbox"
              checked={settings.enabled}
              onChange={(event) => void update({ enabled: event.target.checked })}
            />
          </label>
        )}
      </section>

      {/* Only the settings that describe a panel that is running. A row of
          controls greyed out beneath a switch that is off is more to read and
          no more to do. */}
      {hasPanel && settings.enabled && (
        <>
      <section className="settings-group">
        <h2>Placement</h2>

        <label className="field">
          <span>Screen edge</span>
          <select
            value={settings.edge}
            onChange={(event) =>
              void update({ edge: event.target.value as "left" | "right" })
            }
          >
            <option value="right">Right</option>
            <option value="left">Left</option>
          </select>
        </label>

        <label className="field">
          <span>Display</span>
          <select
            value={settings.screen ?? ""}
            onChange={(event) => void update({ screen: event.target.value || undefined })}
          >
            {/* Following the pointer is right for a laptop plus an external
                display, where "the screen I am looking at" changes. */}
            <option value="">Follow the pointer</option>
            {screens.map((screen, index) => (
              <option value={screen.id} key={screen.id}>
                {screen.primary ? "Primary" : `Display ${index + 1}`} — {Math.round(screen.width)}×
                {Math.round(screen.height)}
              </option>
            ))}
          </select>
        </label>

        <label className="field">
          <span>Width</span>
          <input
            type="number"
            min={200}
            max={640}
            value={settings.width}
            onChange={(event) => void update({ width: Number(event.target.value) })}
          />
          <span className="unit">px</span>
        </label>

        <label className="field">
          <span>Height</span>
          <input
            type="number"
            min={20}
            max={100}
            value={Math.round(settings.height_ratio * 100)}
            onChange={(event) =>
              void update({ height_ratio: Number(event.target.value) / 100 })
            }
          />
          <span className="unit">% of screen</span>
        </label>

        <label className="field">
          <span>Animation</span>
          <input
            type="number"
            min={0}
            max={600}
            step={10}
            value={settings.animation_ms}
            onChange={(event) => void update({ animation_ms: Number(event.target.value) })}
          />
          <span className="unit">ms — 0 disables</span>
        </label>

        <label className="field">
          <span>Keep open</span>
          <input
            type="checkbox"
            checked={settings.pinned}
            onChange={(event) => void update({ pinned: event.target.checked })}
          />
          <span className="unit">never collapse to the tab</span>
        </label>
      </section>

      <section className="settings-group">
        <h2>Resting tab</h2>

        <label className="field">
          <span>Tab width</span>
          <input
            type="number"
            min={4}
            max={40}
            value={settings.island_width}
            onChange={(event) => void update({ island_width: Number(event.target.value) })}
          />
          <span className="unit">px</span>
        </label>

        <label className="field">
          <span>Tab height</span>
          <input
            type="number"
            min={32}
            max={400}
            value={settings.island_height}
            onChange={(event) => void update({ island_height: Number(event.target.value) })}
          />
          <span className="unit">px</span>
        </label>

        <label className="field">
          <span>Hover slack</span>
          <input
            type="number"
            min={0}
            max={40}
            value={settings.hover_margin}
            onChange={(event) => void update({ hover_margin: Number(event.target.value) })}
          />
          <span className="unit">px around the tab</span>
        </label>
      </section>

      <section className="settings-group">
        <h2>Shortcut</h2>
        <label className="field wide">
          <span>Summon the panel</span>
          <input
            type="text"
            value={settings.shortcut}
            onChange={(event) => setSettings({ ...settings, shortcut: event.target.value })}
            onBlur={(event) => void update({ shortcut: event.target.value })}
            spellCheck={false}
          />
        </label>
        <p className="hint">
          Tauri accelerator syntax, e.g. <code>CmdOrCtrl+Alt+L</code> or{" "}
          <code>Ctrl+Shift+Space</code>. If another app already owns it the change
          is refused and the previous one stays in force.
        </p>
      </section>
        </>
      )}

      {info && (
        <section className="settings-group">
          <h2>Daemon</h2>
          <dl className="facts">
            <dt>Version</dt>
            <dd>
              {info.version} · {info.platform}
            </dd>
            <dt>Uptime</dt>
            <dd>{formatUptime(info.uptime_seconds)}</dd>
            <dt>Socket</dt>
            <dd className="mono">{info.socket_path}</dd>
            <dt>Database</dt>
            <dd className="mono">{info.database_path}</dd>
          </dl>
        </section>
      )}
    </div>
  );
}

function formatUptime(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

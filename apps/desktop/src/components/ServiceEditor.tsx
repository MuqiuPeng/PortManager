import { useMemo, useState } from "react";

import { api, errorMessage } from "../api";
import { FolderField } from "./FolderField";
import type { ServicePatch, ServiceType, ServiceView } from "../types";
import { Button } from "@/components/ui/button";

interface Props {
  service: ServiceView;
  onClose: () => void;
  onSaved: () => void;
}

const TYPES: ServiceType[] = [
  "web",
  "api",
  "worker",
  "database",
  "cache",
  "container",
  "custom",
];

const POLICIES = ["reuse", "allocate-next", "fail", "ask", "kill-existing"];

interface EnvRow {
  key: string;
  value: string;
}

/**
 * Correct how a service starts.
 *
 * Everything here comes from inference, which guesses: a framework's default
 * port the project does not use, the `dev` script where `dev:local` is the one
 * that works, an environment variable the process needs and nothing supplies.
 * Until now those could only be fixed from the CLI.
 */
export function ServiceEditor({ service, onClose, onSaved }: Props) {
  const [name, setName] = useState(service.name);
  const [command, setCommand] = useState(service.command);
  const [cwd, setCwd] = useState(service.cwd);
  const [port, setPort] = useState(service.preferred_port?.toString() ?? "");
  const [type, setType] = useState<ServiceType>(service.service_type);
  const [policy, setPolicy] = useState("allocate-next");
  const [dependsOn, setDependsOn] = useState((service.depends_on ?? []).join(" "));
  const [oneShot, setOneShot] = useState(service.one_shot === true);
  const [env, setEnv] = useState<EnvRow[]>(() =>
    Object.entries(service.env ?? {}).map(([key, value]) => ({ key, value })),
  );
  const [busy, setBusy] = useState(false);
  const [confirmingRemove, setConfirmingRemove] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const originalKeys = useMemo(() => Object.keys(service.env ?? {}), [service.env]);

  async function save() {
    setBusy(true);
    setError(null);
    try {
      const kept = env.filter((row) => row.key.trim() !== "");
      const patch: ServicePatch = {
        command,
        cwd,
        service_type: type,
        conflict_policy: policy,
        // `null` clears it — the wire format distinguishes that from "leave it
        // alone", and an empty field here means the service has no port.
        preferred_port: port.trim() === "" ? null : Number(port),
        env: Object.fromEntries(kept.map((row) => [row.key.trim(), row.value])),
        // `env` merges, so a variable the user deleted has to be named.
        remove_env: originalKeys.filter(
          (key) => !kept.some((row) => row.key.trim() === key),
        ),
        // Replaced whole rather than merged: a dependency list is an ordering,
        // and an empty field means "none", not "leave them".
        depends_on: dependsOn.split(/\s+/).filter(Boolean),
        one_shot: oneShot,
      };
      if (name !== service.name) patch.name = name;

      await api.updateService(service.id, patch);
      onSaved();
      onClose();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  // Two steps rather than `window.confirm`, which the webview does not
  // implement — it returns false, so the button silently did nothing.
  async function remove() {
    if (!confirmingRemove) {
      setConfirmingRemove(true);
      return;
    }
    setBusy(true);
    try {
      await api.removeService(service.id);
      onSaved();
      onClose();
    } catch (err) {
      setError(errorMessage(err));
      setBusy(false);
    }
  }

  return (
    <div className="sheet-backdrop" onClick={onClose}>
      <div className="sheet" onClick={(event) => event.stopPropagation()}>
        <header className="sheet-head">
          <h2>{service.name}</h2>
          <Button variant="outline" size="sm" onClick={onClose}>
            Close
          </Button>
        </header>

        {error && <div className="banner inline">{error}</div>}

        <div className="sheet-body">
          <label className="field wide">
            <span>Name</span>
            <input value={name} onChange={(event) => setName(event.target.value)} />
          </label>

          <label className="field wide">
            <span>Command</span>
            <input
              value={command}
              onChange={(event) => setCommand(event.target.value)}
              spellCheck={false}
            />
          </label>

          <FolderField
            label="Working directory"
            value={cwd}
            onChange={setCwd}
            startingAt={service.cwd}
          />

          <label className="field wide">
            <span>Port</span>
            <input
              type="number"
              value={port}
              placeholder="none"
              onChange={(event) => setPort(event.target.value)}
            />
            <span className="unit">blank if it has none</span>
          </label>

          {/* What setting this actually does, because on its own the field
              reads as "make it serve here" and it is not. A service that hard
              codes its port, or reads one under another name, ignores this —
              and then the window shows the port that was asked for while the
              service is on another, which is exactly as confusing as it
              sounds. The health check says so once it is running. */}
          <p className="hint">
            Passed to the service as <code>$PORT</code>, and reserved so nothing
            else takes it. A service that hardcodes its port, or reads a
            different variable, will not move — check its health after saving.
          </p>

          <label className="field wide">
            <span>Type</span>
            <select value={type} onChange={(event) => setType(event.target.value as ServiceType)}>
              {TYPES.map((option) => (
                <option value={option} key={option}>
                  {option}
                </option>
              ))}
            </select>
          </label>

          <label className="field wide">
            <span>If the port is taken</span>
            <select value={policy} onChange={(event) => setPolicy(event.target.value)}>
              {POLICIES.map((option) => (
                <option value={option} key={option}>
                  {option}
                </option>
              ))}
            </select>
          </label>

          <label className="field wide">
            <span>Starts after</span>
            <input
              type="text"
              value={dependsOn}
              onChange={(event) => setDependsOn(event.target.value)}
              placeholder="db migrate"
              spellCheck={false}
            />
          </label>
          <p className="hint">
            Service names in this checkout, separated by spaces. Each is brought
            up and given time to report healthy first. One already running is
            left alone rather than restarted.
          </p>

          <label className="field wide">
            <span>Runs to completion</span>
            <input
              type="checkbox"
              checked={oneShot}
              onChange={(event) => setOneShot(event.target.checked)}
            />
            <span className="unit">a migration or a seed, not a server</span>
          </label>

          <div className="field wide env-field">
            <span>Environment</span>
            <div className="env-rows">
              {env.map((row, index) => (
                <div className="env-row" key={index}>
                  <input
                    placeholder="KEY"
                    value={row.key}
                    spellCheck={false}
                    onChange={(event) =>
                      setEnv(env.map((r, i) => (i === index ? { ...r, key: event.target.value } : r)))
                    }
                  />
                  <input
                    placeholder="value"
                    value={row.value}
                    spellCheck={false}
                    onChange={(event) =>
                      setEnv(
                        env.map((r, i) => (i === index ? { ...r, value: event.target.value } : r)),
                      )
                    }
                  />
                  <Button
                    variant="ghost" size="icon"
                    title="Remove"
                    onClick={() => setEnv(env.filter((_, i) => i !== index))}
                  >
                    ×
                  </Button>
                </div>
              ))}
              <Button
                variant="outline" size="sm"
                onClick={() => setEnv([...env, { key: "", value: "" }])}
              >
                Add variable
              </Button>
              <p className="hint inline-hint">
                These override any <code>.env</code> file the project has.
              </p>
            </div>
          </div>
        </div>

        <footer className="sheet-foot">
          <Button variant="destructive" size="sm" onClick={() => void remove()} disabled={busy}>
            {confirmingRemove ? "Really remove?" : "Remove service"}
          </Button>
          {confirmingRemove && (
            <span className="unit">nothing running is stopped</span>
          )}
          <span className="spacer" />
          <Button variant="outline" size="sm" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="default" size="sm" onClick={() => void save()} disabled={busy}>
            Save
          </Button>
        </footer>
      </div>
    </div>
  );
}

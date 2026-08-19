import { useMemo, useState } from "react";

import { api, errorMessage } from "../api";
import type { ServicePatch, ServiceType, ServiceView } from "../types";

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
  const [env, setEnv] = useState<EnvRow[]>(() =>
    Object.entries(service.env ?? {}).map(([key, value]) => ({ key, value })),
  );
  const [busy, setBusy] = useState(false);
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

  async function remove() {
    if (!window.confirm(`Remove the definition of "${service.name}"? Nothing running is stopped.`)) {
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
          <button className="ghost" onClick={onClose}>
            Close
          </button>
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

          <label className="field wide">
            <span>Working directory</span>
            <input value={cwd} onChange={(event) => setCwd(event.target.value)} spellCheck={false} />
          </label>

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
                  <button
                    className="icon-button"
                    title="Remove"
                    onClick={() => setEnv(env.filter((_, i) => i !== index))}
                  >
                    ×
                  </button>
                </div>
              ))}
              <button
                className="ghost"
                onClick={() => setEnv([...env, { key: "", value: "" }])}
              >
                Add variable
              </button>
              <p className="hint inline-hint">
                These override any <code>.env</code> file the project has.
              </p>
            </div>
          </div>
        </div>

        <footer className="sheet-foot">
          <button className="ghost danger" onClick={() => void remove()} disabled={busy}>
            Remove service
          </button>
          <span className="spacer" />
          <button className="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button className="ghost primary" onClick={() => void save()} disabled={busy}>
            Save
          </button>
        </footer>
      </div>
    </div>
  );
}

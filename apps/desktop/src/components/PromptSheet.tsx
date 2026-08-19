import { useState } from "react";

export interface PromptField {
  label: string;
  placeholder?: string;
  value?: string;
  mono?: boolean;
}

interface Props {
  title: string;
  fields: PromptField[];
  confirmLabel?: string;
  hint?: string;
  onCancel: () => void;
  onConfirm: (values: string[]) => void;
}

/**
 * Ask for a value or two.
 *
 * `window.prompt` exists in the browser but not here: the webview implements
 * none of the JavaScript panel callbacks, so a prompt returns null and the
 * button it belongs to appears to do nothing at all.
 */
export function PromptSheet({
  title,
  fields,
  confirmLabel = "Add",
  hint,
  onCancel,
  onConfirm,
}: Props) {
  const [values, setValues] = useState(() => fields.map((field) => field.value ?? ""));

  const ready = values.every((value, index) => !required(fields[index]) || value.trim() !== "");

  function submit() {
    if (ready) onConfirm(values.map((value) => value.trim()));
  }

  return (
    <div className="sheet-backdrop" onClick={onCancel}>
      <div className="sheet narrow" onClick={(event) => event.stopPropagation()}>
        <header className="sheet-head">
          <h2>{title}</h2>
        </header>

        <div className="sheet-body">
          {fields.map((field, index) => (
            <label className="field wide" key={field.label}>
              <span>{field.label}</span>
              <input
                autoFocus={index === 0}
                value={values[index]}
                placeholder={field.placeholder}
                spellCheck={false}
                style={field.mono ? { fontFamily: "var(--mono)" } : undefined}
                onChange={(event) =>
                  setValues(values.map((value, i) => (i === index ? event.target.value : value)))
                }
                // Enter submits, Escape cancels — the reflexes a prompt had.
                onKeyDown={(event) => {
                  if (event.key === "Enter") submit();
                  if (event.key === "Escape") onCancel();
                }}
              />
            </label>
          ))}
          {hint && <p className="hint">{hint}</p>}
        </div>

        <footer className="sheet-foot">
          <span className="spacer" />
          <button className="ghost" onClick={onCancel}>
            Cancel
          </button>
          <button className="ghost primary" onClick={submit} disabled={!ready}>
            {confirmLabel}
          </button>
        </footer>
      </div>
    </div>
  );
}

/** Every field is required unless it arrives with a value already filled in. */
function required(field: PromptField): boolean {
  return field.value === undefined;
}

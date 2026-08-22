import { useState } from "react";
import { Button } from "@/components/ui/button";

export interface PromptField {
  label: string;
  placeholder?: string;
  value?: string;
  mono?: boolean;
  /**
   * Why this value cannot be used, or null when it can.
   *
   * Checked as it is typed and shown under the field, rather than letting the
   * sheet close on a value the daemon will refuse — by then the sheet is gone
   * and the words arrive detached from the box that caused them.
   */
  problem?: (value: string) => string | null;
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

  const problems = fields.map((field, index) =>
    values[index].trim() === "" ? null : (field.problem?.(values[index].trim()) ?? null),
  );
  const ready =
    values.every((value, index) => !required(fields[index]) || value.trim() !== "") &&
    problems.every((problem) => problem === null);

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
              {problems[index] && <span className="hint problem">{problems[index]}</span>}
            </label>
          ))}
          {hint && <p className="hint">{hint}</p>}
        </div>

        <footer className="sheet-foot">
          <span className="spacer" />
          <Button variant="outline" size="sm" onClick={onCancel}>
            Cancel
          </Button>
          <Button variant="default" size="sm" onClick={submit} disabled={!ready}>
            {confirmLabel}
          </Button>
        </footer>
      </div>
    </div>
  );
}

/** Every field is required unless it arrives with a value already filled in. */
function required(field: PromptField): boolean {
  return field.value === undefined;
}

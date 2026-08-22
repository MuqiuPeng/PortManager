import { chooseFolder } from "../api";
import { Button } from "@/components/ui/button";

interface Props {
  label: string;
  value: string;
  onChange: (path: string) => void;
  /** Where the picker opens when nothing is chosen yet. */
  startingAt?: string;
  hint?: string;
}

/**
 * A folder, chosen rather than typed.
 *
 * The path is shown but not editable: every OS already has a picker that shows
 * what exists, and a typed path is one the app has to guess about — it may not
 * exist, may be a file, may be a typo of a real folder. Empty reads as a
 * prompt rather than an empty box, so it is obvious nothing has been chosen.
 */
export function FolderField({ label, value, onChange, startingAt, hint }: Props) {
  return (
    <label className="field wide folder-field">
      <span>{label}</span>
      <div className="folder-row">
        <output className={value ? "folder-path" : "folder-path empty"} title={value}>
          {value || "No folder chosen"}
        </output>
        <Button
          type="button"
          variant="outline" size="sm"
          onClick={() => {
            void chooseFolder(value || startingAt).then((picked) => {
              if (picked) onChange(picked);
            });
          }}
        >
          Choose…
        </Button>
      </div>
      {hint && <p className="hint">{hint}</p>}
    </label>
  );
}

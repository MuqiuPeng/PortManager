import { useState } from "react";

import { FolderField } from "./FolderField";

interface Props {
  title: string;
  label: string;
  confirmLabel: string;
  startingAt?: string;
  hint?: string;
  onCancel: () => void;
  onConfirm: (path: string) => void;
}

/** Ask for one folder, and nothing else. */
export function FolderSheet({
  title,
  label,
  confirmLabel,
  startingAt,
  hint,
  onCancel,
  onConfirm,
}: Props) {
  const [path, setPath] = useState("");

  return (
    <div className="sheet-backdrop" onClick={onCancel}>
      <div className="sheet narrow" onClick={(event) => event.stopPropagation()}>
        <header className="sheet-head">
          <h2>{title}</h2>
        </header>
        <div className="sheet-body">
          <FolderField label={label} value={path} onChange={setPath} startingAt={startingAt} />
          {hint && <p className="hint">{hint}</p>}
        </div>
        <footer className="sheet-foot">
          <span className="spacer" />
          <button className="ghost" onClick={onCancel}>
            Cancel
          </button>
          <button className="ghost primary" disabled={!path} onClick={() => onConfirm(path)}>
            {confirmLabel}
          </button>
        </footer>
      </div>
    </div>
  );
}

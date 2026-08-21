import { useState } from "react";

import { copyText } from "../api";
import type { Failure } from "../types";

interface Props {
  failures: Failure[];
  /** Open the service's own log, where the whole of it already is. */
  onOpenLogs: (serviceId: string) => void;
  onDismiss: (serviceId: string) => void;
}

/**
 * What broke, in front of whoever is looking, without moving the page.
 *
 * A banner in the layout pushes everything down the moment something fails,
 * which is the moment the layout should be holding still — the row you were
 * about to click has moved. These sit over the corner instead, the way editors
 * and container tools put them, and leave the page where it was.
 *
 * They do not fade. A toast that disappears is right for "saved" and wrong for
 * "your API is down": the thing that makes an error worth showing is that
 * somebody has to act on it, and it should still be there when they look back.
 *
 * Every one can be copied, because the next thing that happens to an error
 * message is that it gets pasted somewhere — a search, an issue, a message to
 * whoever owns the service. Selecting monospace text out of a scrolling box by
 * hand is the small daily tax this removes.
 */
export function FailureToasts({ failures, onOpenLogs, onDismiss }: Props) {
  if (failures.length === 0) return null;

  return (
    <div className="toasts" role="region" aria-label="Failures">
      {failures.map((failure) => (
        <FailureToast
          failure={failure}
          key={failure.service_id}
          onOpenLogs={() => onOpenLogs(failure.service_id)}
          onDismiss={() => onDismiss(failure.service_id)}
        />
      ))}
    </div>
  );
}

function FailureToast({
  failure,
  onOpenLogs,
  onDismiss,
}: {
  failure: Failure;
  onOpenLogs: () => void;
  onDismiss: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const detail = failure.detail ?? [];

  /** What somebody pasting this would want: which service, and what it said. */
  const asText = () => {
    const code = failure.exit_code === undefined ? "" : ` (exit ${failure.exit_code})`;
    return [`${failure.subject} — ${failure.status}${code}`, ...detail].join("\n");
  };

  async function copy() {
    try {
      await copyText(asText());
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Nothing useful to say and nowhere better to say it: the text is still
      // selectable, which is what the button was saving the user from.
    }
  }

  return (
    <div className="toast" role="alert">
      <div className="toast-head">
        <span className="toast-title">{failure.subject}</span>
        <span className="toast-status">
          {failure.status}
          {failure.exit_code !== undefined && ` · exit ${failure.exit_code}`}
        </span>
        <button className="toast-close" onClick={onDismiss} title="Dismiss" aria-label="Dismiss">
          ×
        </button>
      </div>

      {detail.length > 0 ? (
        // Selectable as well as copyable: the button is the fast path, not the
        // only one, and somebody may want a single line rather than all of it.
        <pre className="toast-detail">{detail.join("\n")}</pre>
      ) : (
        <p className="toast-detail quiet">It said nothing before it stopped.</p>
      )}

      <div className="toast-actions">
        <button className="ghost" onClick={onOpenLogs}>
          Logs
        </button>
        <button className="ghost" onClick={() => void copy()} disabled={detail.length === 0}>
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
    </div>
  );
}

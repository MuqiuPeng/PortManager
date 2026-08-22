import { useState } from "react";

import { copyText } from "../api";
import type { Failure, Finding } from "../types";

interface Props {
  /** What the last action said when it would not do what was asked. */
  error: string | null;
  onDismissError: () => void;
  failures: Failure[];
  /** Problems with what is declared, rather than with a run of it. */
  findings: Finding[];
  /** Open the service's own log, where the whole of it already is. */
  onOpenLogs: (serviceId: string) => void;
  onDismiss: (serviceId: string) => void;
  onDismissFindings: () => void;
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
export function FailureToasts({
  error,
  onDismissError,
  failures,
  findings,
  onOpenLogs,
  onDismiss,
  onDismissFindings,
}: Props) {
  if (!error && failures.length === 0 && findings.length === 0) return null;

  return (
    <div className="toasts" role="region" aria-label="Problems">
      {error && (
        // Was a strip across the top, which moved the page at the moment
        // something had just gone wrong on it. Same place as everything else
        // that goes wrong now.
        <div className="toast" role="alert">
          <div className="toast-head">
            <span className="toast-title">That did not work</span>
            <button
              className="toast-close"
              onClick={onDismissError}
              title="Dismiss"
              aria-label="Dismiss"
            >
              ×
            </button>
          </div>
          <p className="toast-detail">{error}</p>
        </div>
      )}
      {failures.map((failure) => (
        <FailureToast
          failure={failure}
          key={failure.service_id}
          onOpenLogs={() => onOpenLogs(failure.service_id)}
          onDismiss={() => onDismiss(failure.service_id)}
        />
      ))}
      {findings.length > 0 && (
        <FindingsToast findings={findings} onDismiss={onDismissFindings} />
      )}
    </div>
  );
}

/**
 * Problems with what is declared, in the same place as problems with what ran.
 *
 * These used to sit in the layout, above the list, which pushed every row down
 * the moment one appeared — the thing toasts exist to avoid, done to the
 * quieter kind of problem because it felt less urgent. Two kinds of problem
 * shown two different ways is one more thing to learn for no benefit.
 *
 * One toast for all of them rather than one each: a checkout naming a missing
 * service is not an event, it is a state, and five states are a list.
 */
function FindingsToast({
  findings,
  onDismiss,
}: {
  findings: Finding[];
  onDismiss: () => void;
}) {
  const [copied, setCopied] = useState(false);
  // The ones that will fail first; the rest only might.
  const ordered = [...findings].sort((a, b) => Number(b.certain) - Number(a.certain));
  const asText = () =>
    ordered
      .map((finding) => `${finding.certain ? "!" : "?"} ${finding.subject}: ${finding.message}`)
      .join("\n");

  return (
    <div className="toast warn" role="alert">
      <div className="toast-head">
        <span className="toast-title">
          {findings.length === 1 ? "1 problem" : `${findings.length} problems`} in what is
          declared
        </span>
        <button className="toast-close" onClick={onDismiss} title="Dismiss" aria-label="Dismiss">
          ×
        </button>
      </div>

      <ul className="toast-findings">
        {ordered.map((finding) => (
          <li key={`${finding.subject}-${finding.message}`}>
            <span className={finding.certain ? "finding-mark certain" : "finding-mark"}>
              {finding.certain ? "!" : "?"}
            </span>
            <span className="finding-subject">{finding.subject}</span>
            <span className="finding-message">{finding.message}</span>
          </li>
        ))}
      </ul>

      <div className="toast-actions">
        <button
          className="ghost"
          onClick={() => {
            void copyText(asText())
              .then(() => {
                setCopied(true);
                window.setTimeout(() => setCopied(false), 1500);
              })
              .catch(() => {});
          }}
        >
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
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

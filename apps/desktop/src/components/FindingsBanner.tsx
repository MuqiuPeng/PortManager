import type { Finding } from "../types";

interface Props {
  findings: Finding[];
  onDismiss: () => void;
}

/**
 * What is wrong with the registry, in the way of the person who can fix it.
 *
 * Deliberately not behind a tab. Every one of these is a problem that stays
 * quiet until the moment it is expensive — a build two services share, a
 * dependency naming nothing, a command that resolves in a shell and not here —
 * and a warning nobody goes looking for is a warning that does not exist. It
 * is dismissible, because the person who has read it should not have to keep
 * reading it.
 */
export function FindingsBanner({ findings, onDismiss }: Props) {
  if (findings.length === 0) return null;

  // The ones that will fail first: the rest only might.
  const ordered = [...findings].sort(
    (a, b) => Number(b.certain) - Number(a.certain),
  );

  return (
    <div className="findings">
      <div className="findings-head">
        <span className="findings-count">
          {findings.length === 1 ? "1 problem" : `${findings.length} problems`} in
          what is declared
        </span>
        <button className="ghost" onClick={onDismiss} title="Hide until next launch">
          Dismiss
        </button>
      </div>
      <ul className="findings-list">
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
    </div>
  );
}

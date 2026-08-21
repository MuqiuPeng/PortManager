import type { Failure } from "../types";

interface Props {
  failures: Failure[];
  onSelect: (serviceId: string) => void;
  onDismiss: () => void;
}

/**
 * What is broken, with what it said, without being asked.
 *
 * The alternative is the two steps somebody debugging cannot take yet: know
 * which service failed, then read its log for the few lines that matter. A
 * service that fails on startup normally explains itself and then goes quiet,
 * so the tail is the message — and it is worth putting in front of the person
 * rather than behind a click, since they are here because something is wrong.
 *
 * Clicking one selects it, which is where the full log already lives.
 */
export function FailureBanner({ failures, onSelect, onDismiss }: Props) {
  if (failures.length === 0) return null;

  return (
    <div className="failures">
      <div className="failures-head">
        <span className="failures-count">
          {failures.length === 1
            ? "1 service is not working"
            : `${failures.length} services are not working`}
        </span>
        <button className="ghost" onClick={onDismiss} title="Hide until something changes">
          Dismiss
        </button>
      </div>

      {failures.map((failure) => {
        const detail = failure.detail ?? [];
        return (
          <div
            className="failure"
            key={failure.service_id}
            onClick={() => onSelect(failure.service_id)}
            role="button"
            tabIndex={0}
            onKeyDown={(event) => {
              if (event.key === "Enter" || event.key === " ") onSelect(failure.service_id);
            }}
            title="Show this service's full output"
          >
            <span className="failure-subject">
              {failure.subject}
              <span className="failure-status">
                {failure.status}
                {failure.exit_code !== undefined && ` · exit ${failure.exit_code}`}
              </span>
            </span>
            {detail.length > 0 ? (
              <pre className="failure-detail">{detail.join("\n")}</pre>
            ) : (
              <span className="failure-detail quiet">it said nothing</span>
            )}
          </div>
        );
      })}
    </div>
  );
}

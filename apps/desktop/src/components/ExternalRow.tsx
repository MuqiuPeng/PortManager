import { openExternal } from "../api";
import type { ExternalService } from "../types";

/**
 * A live port in this checkout that no declared service explains.
 *
 * Shown rather than folded into a declared service: a process in Loom's
 * directory on `:3001` is certainly part of Loom, but deciding *which* service
 * it is would be a guess, and a service reported as running when something else
 * holds its port is worse than an honest gap.
 */
export function ExternalRow({ external }: { external: ExternalService }) {
  const what =
    external.container ?? external.command_line?.split(/\s+/)[0]?.split(/[/\\]/).pop();

  return (
    <div className="service external">
      <span className="dot external-dot" aria-hidden />

      <span className="service-body">
        <span className="service-name">{what ?? `pid ${external.pid}`}</span>
        <span className="service-meta">
          running · not started by the runtime
        </span>
      </span>

      <span className="service-port">:{external.port}</span>

      <span className="service-actions">
        {external.url && (
          <button
            className="ghost"
            title={external.url}
            onClick={() => void openExternal(external.url as string)}
          >
            Open
          </button>
        )}
      </span>
    </div>
  );
}

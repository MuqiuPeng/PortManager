import { useEffect, useRef } from "react";

import type { LogLine } from "../types";

interface Props {
  serviceName: string | null;
  lines: LogLine[];
}

export function LogPanel({ serviceName, lines }: Props) {
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);

  // Follow the tail, but stop fighting the user the moment they scroll up to
  // read something.
  useEffect(() => {
    const element = scroller.current;
    if (element && pinned.current) {
      element.scrollTop = element.scrollHeight;
    }
  }, [lines]);

  function handleScroll() {
    const element = scroller.current;
    if (!element) return;
    const distanceFromBottom =
      element.scrollHeight - element.scrollTop - element.clientHeight;
    pinned.current = distanceFromBottom < 40;
  }

  return (
    <section className="logs">
      <header className="logs-head">
        <span>Logs</span>
        {serviceName && <span className="logs-service">{serviceName}</span>}
      </header>

      <div className="logs-body" ref={scroller} onScroll={handleScroll}>
        {lines.length === 0 ? (
          <p className="empty">
            {serviceName
              ? "No output captured yet."
              : "Select a service to see its output."}
          </p>
        ) : (
          lines.map((line) => (
            <div key={line.seq} className={`log-line ${line.stream}`}>
              <span className="log-time">{formatTime(line.timestamp)}</span>
              <span className="log-message">{line.message}</span>
            </div>
          ))
        )}
      </div>
    </section>
  );
}

function formatTime(timestamp: string): string {
  const date = new Date(timestamp);
  return Number.isNaN(date.getTime())
    ? ""
    : date.toLocaleTimeString(undefined, { hour12: false });
}

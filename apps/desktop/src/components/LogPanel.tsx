import { useEffect, useRef, useState } from "react";

import { copyText } from "../api";
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

  const [copied, setCopied] = useState(false);

  async function copyAll() {
    try {
      await copyText(lines.map((line) => line.message).join("\n"));
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // The text is still selectable, which is what the button was for.
    }
  }

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
        <span className="spacer" />
        {/* The next thing that happens to an error is that it gets pasted
            somewhere. Selecting it out of a scrolling box by hand is the tax
            this removes; the text stays selectable for anyone who wants one
            line rather than all of them. */}
        <button
          className="ghost"
          disabled={lines.length === 0}
          onClick={() => void copyAll()}
          title="Copy this output"
        >
          {copied ? "Copied" : "Copy"}
        </button>
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

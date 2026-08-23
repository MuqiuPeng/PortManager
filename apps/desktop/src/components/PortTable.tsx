import type { PortOwner } from "../types";

interface Props {
  ports: PortOwner[];
}

/**
 * Everything listening on this machine, whether the runtime started it or not.
 *
 * The unmanaged rows are the point: this is where a developer finds out that
 * `:3000` belongs to a service they started in a terminal two days ago.
 */
export function PortTable({ ports }: Props) {
  if (ports.length === 0) {
    return <p className="empty">Nothing is listening.</p>;
  }

  return (
    <table className="ports">
      <thead>
        <tr>
          <th>Port</th>
          <th>Proto</th>
          <th>Owner</th>
          <th>PID</th>
          <th>Working directory</th>
        </tr>
      </thead>
      <tbody>
        {/* The protocol is part of a row's identity: one number can be held by
            both a TCP and a UDP socket, and keying without it collapses the two
            into a single row. */}
        {ports.map((port) => (
          <tr key={`${port.port}-${port.protocol}-${port.pid}`}>
            <td className="mono">{port.port}</td>
            <td className="mono">{port.protocol}</td>
            <td>
              {port.project_name ? (
                <span className="owner">
                  <span className="owner-project">{port.project_name}</span>
                  {port.git_branch && (
                    <span className="owner-branch">{port.git_branch}</span>
                  )}
                  {port.service_name && (
                    <span className="owner-service">{port.service_name}</span>
                  )}
                </span>
              ) : (
                <span className="owner unregistered">
                  {shortCommand(port) ?? "unregistered process"}
                </span>
              )}
            </td>
            {/* A container's pid is Docker's; the container name is the fact. */}
            <td className="mono">{port.container ?? port.pid}</td>
            <td className="mono path" title={port.cwd}>
              {port.cwd ?? "—"}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** The executable name is more identifying than a full argv at table width. */
function shortCommand(port: PortOwner): string | null {
  if (port.container) return port.container;
  if (port.executable) {
    const parts = port.executable.split(/[/\\]/);
    return parts[parts.length - 1] || port.executable;
  }
  return port.command_line?.split(/\s+/)[0] ?? null;
}

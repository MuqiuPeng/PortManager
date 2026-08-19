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
          <th>Owner</th>
          <th>PID</th>
          <th>Working directory</th>
        </tr>
      </thead>
      <tbody>
        {ports.map((port) => (
          <tr key={`${port.port}-${port.pid}`}>
            <td className="mono">{port.port}</td>
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
            <td className="mono">{port.pid}</td>
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
  if (port.executable) {
    const parts = port.executable.split(/[/\\]/);
    return parts[parts.length - 1] || port.executable;
  }
  return port.command_line?.split(/\s+/)[0] ?? null;
}

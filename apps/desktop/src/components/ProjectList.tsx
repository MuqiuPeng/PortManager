import type { ProjectView } from "../types";
import { Button } from "@/components/ui/button";

interface Props {
  projects: ProjectView[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  onAdd: () => void;
  busy: boolean;
}

export function ProjectList({ projects, selectedId, onSelect, onAdd, busy }: Props) {
  return (
    <nav className="sidebar">
      <div className="sidebar-head">
        <span className="sidebar-title">Projects</span>
        <Button variant="outline" size="sm" onClick={onAdd} disabled={busy} title="Find projects">
          +
        </Button>
      </div>

      {projects.length === 0 ? (
        <p className="empty">
          Nothing added yet — check the Discover tab.
        </p>
      ) : (
        <ul className="project-list">
          {projects.map((project) => (
            <li key={project.id}>
              <button
                className={project.id === selectedId ? "project selected" : "project"}
                onClick={() => onSelect(project.id)}
              >
                {/* Something being up is what the dot means, whoever started it. */}
                <span
                  className={
                    project.running_services > 0 || (project.external_services ?? 0) > 0
                      ? "dot live"
                      : "dot"
                  }
                  aria-hidden
                />
                <span className="project-body">
                  <span className="project-name">{project.name}</span>
                  <span className="project-meta">
                    {project.running_services}/{project.total_services} running
                    {(project.external_services ?? 0) > 0 &&
                      ` · ${project.external_services} external`}
                  </span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </nav>
  );
}

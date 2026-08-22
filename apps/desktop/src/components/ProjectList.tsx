import { cn } from "@/lib/utils";
import type { ProjectView } from "../types";
import { Button } from "@/components/ui/button";

type View = "services" | "ports" | "discover" | "settings";

interface Props {
  /** Which view the window is on, so this can show where you are. */
  current: View;
  onView: (view: View) => void;
  projects: ProjectView[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  onAdd: () => void;
  busy: boolean;
}

export function ProjectList({
  projects,
  selectedId,
  onSelect,
  onAdd,
  busy,
  current,
  onView,
}: Props) {
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

      {/* Ports, Discover and Settings are views of the machine, not of a
          project — they sat in the same tab strip as the project list, which
          asked the reader to hold two kinds of thing in one row. Down here
          they are what they are: somewhere else to go. */}
      <div className="mt-auto flex flex-col gap-0.5 border-t pt-2">
        {views.map((view) => (
          <button
            key={view.id}
            onClick={() => onView(view.id)}
            className={cn(
              "rounded-md px-2 py-1.5 text-left text-sm transition-colors",
              current === view.id
                ? "bg-accent text-accent-foreground"
                : "text-muted-foreground hover:bg-accent/50",
            )}
          >
            {view.label}
          </button>
        ))}
      </div>
    </nav>
  );
}

const views = [
  { id: "ports" as const, label: "Ports" },
  { id: "discover" as const, label: "Discover" },
  { id: "settings" as const, label: "Settings" },
];

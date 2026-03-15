import { FolderGit2 } from "lucide-react";
import { memo, useCallback } from "react";
import { useLocation, useNavigate } from "react-router";
import type { Project } from "../../lib/api";

interface ProjectItemProps {
  project: Project;
}

export const ProjectItem = memo(function ProjectItem({
  project,
}: ProjectItemProps) {
  const navigate = useNavigate();
  const location = useLocation();
  const isActive = location.pathname === `/projects/${project.id}`;

  const handleClick = useCallback(() => {
    void navigate(`/projects/${project.id}`);
  }, [navigate, project.id]);

  return (
    <button
      onClick={handleClick}
      className={`flex h-7 w-full items-center gap-1.5 rounded-sm px-2 text-left text-[12px] transition-colors duration-150 hover:bg-bg-hover ${
        isActive ? "bg-bg-hover text-text-primary" : "text-text-secondary"
      }`}
    >
      <FolderGit2 size={13} className="shrink-0 text-text-tertiary" />
      <span className="truncate">{project.name}</span>
      {project.has_claude_config && (
        <span
          className="ml-auto h-1.5 w-1.5 shrink-0 rounded-full bg-accent"
          title=".claude/ config present"
        />
      )}
    </button>
  );
});

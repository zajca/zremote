import { Brain, ChevronRight, FolderGit2, GitBranch, Plus } from "lucide-react";
import { memo, useCallback, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import type { Project, Session } from "../../lib/api";
import { api } from "../../lib/api";
import { useKnowledgeStore } from "../../stores/knowledge-store";
import { SessionItem } from "./SessionItem";

interface ProjectItemProps {
  project: Project;
  sessions: Session[];
  hostId: string;
}

export const ProjectItem = memo(function ProjectItem({
  project,
  sessions,
  hostId,
}: ProjectItemProps) {
  const navigate = useNavigate();
  const location = useLocation();
  const isActive = location.pathname === `/projects/${project.id}`;
  const knowledgeStatus = useKnowledgeStore(
    (s) => s.statusByProject[project.id]?.status,
  );
  const [expanded, setExpanded] = useState(sessions.length > 0);

  const handleClick = useCallback(() => {
    void navigate(`/projects/${project.id}`);
  }, [navigate, project.id]);

  const handleToggle = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      setExpanded((prev) => !prev);
    },
    [],
  );

  const handleNewSession = useCallback(
    async (e: React.MouseEvent) => {
      e.stopPropagation();
      try {
        const session = await api.sessions.create(hostId, {
          workingDir: project.path,
        });
        void navigate(`/hosts/${hostId}/sessions/${session.id}`);
      } catch (err) {
        console.error("failed to create session", err);
      }
    },
    [hostId, project.path, navigate],
  );

  return (
    <div>
      <div
        className={`group flex h-7 w-full items-center gap-1.5 rounded-sm px-2 text-left text-[12px] transition-colors duration-150 hover:bg-bg-hover ${
          isActive ? "bg-bg-hover text-text-primary" : "text-text-secondary"
        }`}
      >
        {sessions.length > 0 ? (
          <button
            onClick={handleToggle}
            className="flex h-4 w-4 shrink-0 items-center justify-center rounded transition-colors duration-150 hover:bg-bg-active"
            aria-label={expanded ? "Collapse" : "Expand"}
          >
            <ChevronRight
              size={11}
              className={`transition-transform duration-150 ${expanded ? "rotate-90" : ""}`}
            />
          </button>
        ) : project.project_type === "worktree" ? (
          <GitBranch size={13} className="shrink-0 text-text-tertiary" />
        ) : (
          <FolderGit2 size={13} className="shrink-0 text-text-tertiary" />
        )}
        <button
          onClick={handleClick}
          className="flex min-w-0 flex-1 items-center gap-1.5 truncate text-left"
        >
          {sessions.length > 0 && (
            project.project_type === "worktree"
              ? <GitBranch size={13} className="shrink-0 text-text-tertiary" />
              : <FolderGit2 size={13} className="shrink-0 text-text-tertiary" />
          )}
          <span className="truncate">{project.name}</span>
        </button>
        {project.git_branch && (
          <span
            className="flex shrink-0 items-center gap-0.5 rounded bg-bg-active px-1 py-0.5 text-[9px] text-text-tertiary"
            title={`Branch: ${project.git_branch}${project.git_ahead > 0 ? ` (+${project.git_ahead})` : ""}${project.git_behind > 0 ? ` (-${project.git_behind})` : ""}`}
          >
            <GitBranch size={9} />
            <span className="max-w-[60px] truncate">{project.git_branch}</span>
          </span>
        )}
        {project.git_is_dirty && (
          <span
            className="h-1.5 w-1.5 shrink-0 rounded-full bg-status-warning"
            title="Uncommitted changes"
          />
        )}
        {knowledgeStatus === "ready" && (
          <span title="Knowledge base active">
            <Brain size={11} className="shrink-0 text-accent" />
          </span>
        )}
        {project.has_claude_config && (
          <span
            className="h-1.5 w-1.5 shrink-0 rounded-full bg-accent"
            title=".claude/ config present"
          />
        )}
        {sessions.length > 0 && (
          <span className="shrink-0 text-[10px] text-text-tertiary">
            {sessions.length}
          </span>
        )}
        <button
          onClick={(e) => void handleNewSession(e)}
          className="hidden h-4 w-4 shrink-0 items-center justify-center rounded text-text-tertiary transition-colors duration-150 hover:bg-bg-active hover:text-text-primary group-hover:flex"
          aria-label="New session in project"
        >
          <Plus size={11} />
        </button>
      </div>
      {expanded && sessions.length > 0 && (
        <div className="ml-4">
          {sessions.map((session) => (
            <SessionItem
              key={session.id}
              session={session}
              hostId={hostId}
            />
          ))}
        </div>
      )}
    </div>
  );
});

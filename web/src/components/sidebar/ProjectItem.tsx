import { Bot, Brain, ChevronRight, FolderGit2, GitBranch, Plus, RotateCcw } from "lucide-react";
import { memo, useCallback, useEffect, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import type { Project, Session } from "../../lib/api";
import { api } from "../../lib/api";
import { useClaudeTaskStore } from "../../stores/claude-task-store";
import { useKnowledgeStore } from "../../stores/knowledge-store";
import { StartClaudeDialog } from "../StartClaudeDialog";
import { SessionItem } from "./SessionItem";

interface ProjectItemProps {
  project: Project;
  sessions: Session[];
  hostId: string;
  worktreeChildren?: Project[];
  projectSessionsMap?: Map<string, Session[]>;
}

export const ProjectItem = memo(function ProjectItem({
  project,
  sessions,
  hostId,
  worktreeChildren = [],
  projectSessionsMap,
}: ProjectItemProps) {
  const navigate = useNavigate();
  const location = useLocation();
  const isActive = location.pathname === `/projects/${project.id}`;
  const knowledgeStatus = useKnowledgeStore(
    (s) => s.statusByProject[project.id]?.status,
  );
  const [expanded, setExpanded] = useState(
    sessions.length > 0 || worktreeChildren.some((wt) => (projectSessionsMap?.get(wt.id) ?? []).length > 0),
  );
  const [showClaudeDialog, setShowClaudeDialog] = useState(false);

  // Find the last resumable Claude task for this project (return stable string, not object)
  const lastResumableTaskId = useClaudeTaskStore((s) => {
    let bestId: string | null = null;
    let bestEnded = "";
    for (const task of s.tasks.values()) {
      if (task.project_id !== project.id) continue;
      if (task.status !== "completed" && task.status !== "error") continue;
      const ended = task.ended_at ?? "";
      if (!bestId || ended > bestEnded) {
        bestId = task.id;
        bestEnded = ended;
      }
    }
    return bestId;
  });
  const lastResumableTask = useClaudeTaskStore((s) =>
    lastResumableTaskId ? s.tasks.get(lastResumableTaskId) : undefined,
  );

  // Fetch tasks once on mount
  useEffect(() => {
    void useClaudeTaskStore.getState().fetchTasks({ project_id: project.id });
  }, [project.id]);

  // Re-fetch on task updates
  useEffect(() => {
    const handler = () =>
      void useClaudeTaskStore.getState().fetchTasks({ project_id: project.id });
    window.addEventListener("myremote:claude-task-update", handler);
    return () => window.removeEventListener("myremote:claude-task-update", handler);
  }, [project.id]);

  const handleResume = useCallback(
    async (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!lastResumableTaskId) return;
      try {
        const newTask = await api.claudeTasks.resume(lastResumableTaskId);
        void navigate(`/hosts/${hostId}/sessions/${newTask.session_id}`);
      } catch (err) {
        console.error("failed to resume Claude task", err);
      }
    },
    [lastResumableTaskId, navigate, hostId],
  );

  const totalSessions = sessions.length + worktreeChildren.reduce(
    (acc, wt) => acc + (projectSessionsMap?.get(wt.id) ?? []).length,
    0,
  );

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
        {sessions.length > 0 || worktreeChildren.length > 0 ? (
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
          {(sessions.length > 0 || worktreeChildren.length > 0) && (
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
            className="shrink-0 rounded bg-status-warning/15 px-1 py-0.5 text-[9px] text-status-warning"
            title="Uncommitted changes"
          >
            M
          </span>
        )}
        {knowledgeStatus === "ready" && (
          <span title="Knowledge base active">
            <Brain size={11} className="shrink-0 text-accent" />
          </span>
        )}
        {project.has_claude_config && (
          <span
            className="shrink-0 rounded bg-accent/15 px-1 py-0.5 text-[9px] text-accent"
            title=".claude/ config present"
          >
            .claude
          </span>
        )}
        {totalSessions > 0 && (
          <span className="shrink-0 text-[10px] text-text-tertiary">
            {totalSessions}
          </span>
        )}
        {lastResumableTaskId && (
          <button
            onClick={(e) => void handleResume(e)}
            className="hidden h-4 w-4 shrink-0 items-center justify-center rounded text-accent transition-colors duration-150 hover:bg-bg-active hover:text-accent group-hover:flex"
            aria-label="Resume last Claude task"
            title={lastResumableTask?.summary ?? "Resume Claude"}
          >
            <RotateCcw size={11} />
          </button>
        )}
        <button
          onClick={(e) => {
            e.stopPropagation();
            setShowClaudeDialog(true);
          }}
          className="hidden h-4 w-4 shrink-0 items-center justify-center rounded text-text-tertiary transition-colors duration-150 hover:bg-bg-active hover:text-accent group-hover:flex"
          aria-label="Start Claude in project"
        >
          <Bot size={11} />
        </button>
        <button
          onClick={(e) => void handleNewSession(e)}
          className="hidden h-4 w-4 shrink-0 items-center justify-center rounded text-text-tertiary transition-colors duration-150 hover:bg-bg-active hover:text-text-primary group-hover:flex"
          aria-label="New session in project"
        >
          <Plus size={11} />
        </button>
      </div>
      {expanded && (sessions.length > 0 || worktreeChildren.length > 0) && (
        <div className="ml-4">
          {sessions.map((session) => (
            <SessionItem
              key={session.id}
              session={session}
              hostId={hostId}
            />
          ))}
          {worktreeChildren.map((wt) => (
            <ProjectItem
              key={wt.id}
              project={wt}
              sessions={projectSessionsMap?.get(wt.id) ?? []}
              hostId={hostId}
            />
          ))}
        </div>
      )}
      {showClaudeDialog && (
        <StartClaudeDialog
          projectName={project.name}
          projectPath={project.path}
          hostId={hostId}
          projectId={project.id}
          onClose={() => setShowClaudeDialog(false)}
        />
      )}
    </div>
  );
});

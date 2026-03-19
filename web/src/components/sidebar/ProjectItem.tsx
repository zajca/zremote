import { Bot, Brain, ChevronRight, FileText, FolderGit2, GitBranch, Loader2, Plus, RotateCcw, Settings, Sparkles, Zap } from "lucide-react";
import { memo, useCallback, useEffect, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import type { Project, ProjectAction, Session } from "../../lib/api";
import { api } from "../../lib/api";
import type { PromptTemplate } from "../../types/prompt";
import { useClaudeTaskStore } from "../../stores/claude-task-store";
import { useKnowledgeStore } from "../../stores/knowledge-store";
import { showToast } from "../layout/Toast";
import { getActionIcon, hasScope } from "../project/action-utils";
import { StartClaudeDialog } from "../StartClaudeDialog";
import { RunPromptDialog } from "../RunPromptDialog";
import { SessionItem } from "./SessionItem";

// CSS-only tooltip — no JS state, instant hover
function Tooltip({ children, label }: { children: React.ReactNode; label: string }) {
  return (
    <span className="group/tip relative flex items-center">
      {children}
      <span className="pointer-events-none absolute bottom-full left-1/2 z-50 mb-1.5 -translate-x-1/2 whitespace-nowrap rounded bg-bg-primary px-2 py-1 text-[11px] text-text-primary opacity-0 shadow-lg ring-1 ring-white/10 transition-opacity duration-150 group-hover/tip:opacity-100">
        {label}
      </span>
    </span>
  );
}

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
  const [prompts, setPrompts] = useState<PromptTemplate[]>([]);
  const [showPromptMenu, setShowPromptMenu] = useState(false);
  const [selectedPrompt, setSelectedPrompt] = useState<PromptTemplate | null>(null);
  const [sidebarActions, setSidebarActions] = useState<ProjectAction[]>([]);
  const [showSidebarMenu, setShowSidebarMenu] = useState(false);
  const [runningSidebarAction, setRunningSidebarAction] = useState<string | null>(null);

  // Fetch prompt templates and sidebar actions if project has zremote config
  useEffect(() => {
    if (!project.has_zremote_config) return;
    void api.projects.actions(project.id).then(
      (data) => {
        setPrompts(data.prompts ?? []);
        setSidebarActions(data.actions.filter((a) => hasScope(a, "sidebar")));
      },
      (err) => console.error("failed to fetch sidebar actions", err),
    );
  }, [project.id, project.has_zremote_config]);

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
    window.addEventListener("zremote:claude-task-update", handler);
    return () => window.removeEventListener("zremote:claude-task-update", handler);
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
        showToast("Failed to resume task", "error");
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

  // Dismiss dropdowns on outside click or Escape
  useEffect(() => {
    if (!showSidebarMenu && !showPromptMenu) return;
    const handleClick = () => {
      setShowSidebarMenu(false);
      setShowPromptMenu(false);
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setShowSidebarMenu(false);
        setShowPromptMenu(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [showSidebarMenu, showPromptMenu]);

  const handleSidebarAction = useCallback(
    async (e: React.MouseEvent, actionName: string) => {
      e.stopPropagation();
      setRunningSidebarAction(actionName);
      setShowSidebarMenu(false);
      try {
        const result = await api.projects.runAction(project.id, actionName);
        void navigate(`/hosts/${hostId}/sessions/${result.session_id}`);
      } catch (err) {
        console.error("failed to run sidebar action", err);
        showToast(`Failed to run "${actionName}"`, "error");
      } finally {
        setRunningSidebarAction(null);
      }
    },
    [project.id, hostId, navigate],
  );

  return (
    <div>
      <div
        role="button"
        tabIndex={0}
        onClick={handleClick}
        onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); handleClick(); } }}
        className={`group flex h-7 w-full cursor-pointer items-center gap-1.5 rounded-sm px-2 text-left text-[12px] transition-colors duration-150 hover:bg-bg-hover ${
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
        <span className="flex min-w-0 flex-1 items-center gap-1.5 truncate">
          {(sessions.length > 0 || worktreeChildren.length > 0) && (
            project.project_type === "worktree"
              ? <GitBranch size={13} className="shrink-0 text-text-tertiary" />
              : <FolderGit2 size={13} className="shrink-0 text-text-tertiary" />
          )}
          <span className="truncate">{project.name}</span>
        </span>
        <span className="flex shrink-0 items-center gap-1">
          {project.git_branch && (
            <Tooltip label={`Branch: ${project.git_branch}${project.git_is_dirty ? " (uncommitted changes)" : ""}${project.git_ahead > 0 ? ` +${project.git_ahead} ahead` : ""}${project.git_behind > 0 ? ` ${project.git_behind} behind` : ""}`}>
              <GitBranch
                size={10}
                className={project.git_is_dirty ? "text-status-warning" : "text-text-tertiary"}
              />
            </Tooltip>
          )}
          {!project.git_branch && project.git_is_dirty && (
            <Tooltip label="Uncommitted changes">
              <span className="inline-flex h-2 w-2 rounded-full bg-status-warning" />
            </Tooltip>
          )}
          {knowledgeStatus === "ready" && (
            <Tooltip label="Knowledge base active">
              <Brain size={10} className="text-accent" />
            </Tooltip>
          )}
          {project.has_claude_config && (
            <Tooltip label="Claude Code config (.claude/)">
              <Sparkles size={10} className="text-accent" />
            </Tooltip>
          )}
          {project.has_zremote_config && (
            <Tooltip label="ZRemote config (.zremote/)">
              <Settings size={10} className="text-status-online" />
            </Tooltip>
          )}
          {totalSessions > 0 && (
            <span className="text-[10px] text-text-tertiary">{totalSessions}</span>
          )}
        </span>
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
        {sidebarActions.length === 1 && sidebarActions[0] && (() => {
          const SidebarIcon = getActionIcon(sidebarActions[0]!.icon);
          return (
            <button
              onClick={(e) => void handleSidebarAction(e, sidebarActions[0]!.name)}
              disabled={runningSidebarAction === sidebarActions[0]!.name}
              className="hidden h-4 w-4 shrink-0 items-center justify-center rounded text-text-tertiary transition-colors duration-150 hover:bg-bg-active hover:text-accent focus-visible:ring-2 focus-visible:ring-accent/50 focus-visible:outline-none group-hover:flex"
              aria-label={`Run ${sidebarActions[0]!.name}`}
            >
              {runningSidebarAction === sidebarActions[0]!.name ? (
                <Loader2 size={11} className="animate-spin" />
              ) : (
                <SidebarIcon size={11} />
              )}
            </button>
          );
        })()}
        {sidebarActions.length > 1 && (
          <span className="relative">
            <button
              onClick={(e) => {
                e.stopPropagation();
                setShowSidebarMenu((prev) => !prev);
              }}
              className="hidden h-4 w-4 shrink-0 items-center justify-center rounded text-text-tertiary transition-colors duration-150 hover:bg-bg-active hover:text-accent focus-visible:ring-2 focus-visible:ring-accent/50 focus-visible:outline-none group-hover:flex"
              aria-label="Run sidebar action"
            >
              {runningSidebarAction ? (
                <Loader2 size={11} className="animate-spin" />
              ) : (
                <Zap size={11} />
              )}
            </button>
            {showSidebarMenu && (
              <div className="absolute right-0 top-full z-50 mt-1 min-w-[160px] rounded-md border border-border bg-bg-primary py-1 shadow-lg">
                {sidebarActions.map((a) => {
                  const Icon = getActionIcon(a.icon);
                  return (
                    <button
                      key={a.name}
                      onClick={(e) => void handleSidebarAction(e, a.name)}
                      disabled={runningSidebarAction === a.name}
                      className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-text-secondary transition-colors hover:bg-bg-hover hover:text-text-primary"
                    >
                      <Icon size={12} />
                      {a.name}
                    </button>
                  );
                })}
              </div>
            )}
          </span>
        )}
        {prompts.length > 0 && (
          <span className="relative">
            <button
              onClick={(e) => {
                e.stopPropagation();
                setShowPromptMenu((prev) => !prev);
              }}
              className="hidden h-4 w-4 shrink-0 items-center justify-center rounded text-text-tertiary transition-colors duration-150 hover:bg-bg-active hover:text-accent group-hover:flex"
              aria-label="Run prompt template"
            >
              <FileText size={11} />
            </button>
            {showPromptMenu && (
              <div className="absolute right-0 top-full z-50 mt-1 min-w-[160px] rounded-md border border-border bg-bg-primary py-1 shadow-lg">
                {prompts.map((p) => (
                  <button
                    key={p.name}
                    onClick={(e) => {
                      e.stopPropagation();
                      setShowPromptMenu(false);
                      setSelectedPrompt(p);
                    }}
                    className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-text-secondary transition-colors hover:bg-bg-hover hover:text-text-primary"
                  >
                    {p.name}
                  </button>
                ))}
              </div>
            )}
          </span>
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
      {selectedPrompt && (
        <RunPromptDialog
          template={selectedPrompt}
          projectId={project.id}
          projectPath={project.path}
          hostId={hostId}
          projectName={project.name}
          onClose={() => setSelectedPrompt(null)}
        />
      )}
    </div>
  );
});

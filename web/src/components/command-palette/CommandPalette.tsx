import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { Command } from "cmdk";
import { useCommandPaletteStore } from "../../stores/command-palette-store";
import { useCommandPaletteContext } from "../../hooks/useCommandPaletteContext";
import { useDoubleShift } from "../../hooks/useDoubleShift";
import { useMode } from "../../hooks/useMode";
import { useHosts } from "../../hooks/useHosts";
import { api } from "../../lib/api";
import type { ClaudeDefaults, Project, ProjectAction, Session } from "../../lib/api";
import type { AgenticLoop } from "../../types/agentic";
import type { PromptTemplate } from "../../types/prompt";
import { resolveActions, type ResolveData } from "./actions/registry";
import type { ActionDeps, ContextLevel, PaletteAction, PaletteContext } from "./types";
import { CommandPaletteInput } from "./CommandPaletteInput";
import { CommandPaletteItem } from "./CommandPaletteItem";
import { CommandPaletteFooter } from "./CommandPaletteFooter";
import { AddProjectDialog } from "../AddProjectDialog";
import { StartClaudeDialog } from "../StartClaudeDialog";
import { RunPromptDialog } from "../RunPromptDialog";
import { ActionInputDialog } from "../project/ActionInputDialog";
import { useShortcutSessions } from "../../hooks/useShortcutSessions";
import { useGlobalShortcuts, type ShortcutAction } from "../../hooks/useGlobalShortcuts";
import { showToast } from "../layout/Toast";

const GROUP_HEADING_CLASS =
  "[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-text-tertiary";

interface CommandPaletteProps {
  onOpenHelp?: () => void;
}

export function CommandPalette({ onOpenHelp }: CommandPaletteProps) {
  const navigate = useNavigate();
  const { isLocal } = useMode();
  const { hosts } = useHosts();
  const routeContext = useCommandPaletteContext();
  const shortcutSessions = useShortcutSessions(hosts, isLocal);

  const {
    open,
    setOpen,
    toggle,
    contextStack,
    pushContext,
    popContext,
    jumpToIndex,
    resetToRouteContext,
    query,
    setQuery,
    currentContext,
  } = useCommandPaletteStore();

  const ctx = currentContext();

  // Dialog state
  const [addProjectHostId, setAddProjectHostId] = useState<string | null>(null);
  const [claudeDialogProject, setClaudeDialogProject] = useState<{
    id: string;
    name: string;
    path: string;
    host_id: string;
  } | null>(null);
  const [runPromptState, setRunPromptState] = useState<{
    template: PromptTemplate;
    project: { id: string; name: string; path: string; host_id: string };
  } | null>(null);
  const [actionInputState, setActionInputState] = useState<{
    action: ProjectAction;
    project: { id: string; host_id: string };
  } | null>(null);

  // Fetched entity state
  const [projects, setProjects] = useState<Project[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loops, setLoops] = useState<AgenticLoop[]>([]);
  const [customActions, setCustomActions] = useState<ProjectAction[]>([]);
  const [promptTemplates, setPromptTemplates] = useState<PromptTemplate[]>([]);
  const [project, setProject] = useState<Project | null>(null);
  const [parentProject, setParentProject] = useState<Project | null>(null);
  const [session, setSession] = useState<Session | null>(null);
  const [loop, setLoop] = useState<AgenticLoop | null>(null);
  const [hasRecentClaudeTask, setHasRecentClaudeTask] = useState(false);

  // Ancestor entity state
  const [ancestorProject, setAncestorProject] = useState<Project | null>(null);
  const [ancestorProjectSessions, setAncestorProjectSessions] = useState<Session[]>([]);
  const [ancestorProjectWorktrees, setAncestorProjectWorktrees] = useState<Project[]>([]);
  const [ancestorProjectActions, setAncestorProjectActions] = useState<ProjectAction[]>([]);
  const [ancestorProjectTemplates, setAncestorProjectTemplates] = useState<PromptTemplate[]>([]);
  const [ancestorProjectHasRecentClaude, setAncestorProjectHasRecentClaude] = useState(false);
  const [ancestorHostProjects, setAncestorHostProjects] = useState<Project[]>([]);
  const [ancestorHostSessions, setAncestorHostSessions] = useState<Session[]>([]);

  // Ctrl+K handler
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        toggle();
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [toggle]);

  // Double-Shift handler
  const handleDoubleShift = useCallback(() => {
    if (!open) {
      setOpen(true);
    }
  }, [open, setOpen]);
  useDoubleShift(handleDoubleShift);

  // Global keyboard shortcuts (always active, not gated by palette open state)
  const globalShortcutActions = useMemo((): ShortcutAction[] => {
    const sa: ShortcutAction[] = [];

    // Session shortcuts (Ctrl+1-9)
    for (let i = 0; i < Math.min(shortcutSessions.length, 9); i++) {
      const s = shortcutSessions[i];
      if (!s) continue;
      sa.push({
        shortcut: { mod: true, key: String(i + 1) },
        onSelect: () => {
          void navigate(`/hosts/${s.hostId}/sessions/${s.sessionId}`);
          setOpen(false);
        },
      });
    }

    // Ctrl+, → Settings
    sa.push({
      shortcut: { mod: true, key: "," },
      onSelect: () => {
        void navigate("/settings");
        setOpen(false);
      },
    });

    // Alt+N → New session (context-aware)
    const targetHost = hosts.find((h) => h.status === "online");
    if (targetHost) {
      sa.push({
        shortcut: { alt: true, key: "n" },
        onSelect: () => {
          void (async () => {
            try {
              let hostId = targetHost.id;
              let opts: { workingDir?: string } = {};

              // Resolve project context from current route
              if (routeContext.projectId) {
                try {
                  const proj = await api.projects.get(routeContext.projectId);
                  hostId = proj.host_id;
                  opts = { workingDir: proj.path };
                } catch { /* fall back to default host */ }
              } else if (routeContext.sessionId) {
                try {
                  const sess = await api.sessions.get(routeContext.sessionId);
                  if (sess.project_id) {
                    const proj = await api.projects.get(sess.project_id);
                    hostId = proj.host_id;
                    opts = { workingDir: proj.path };
                  } else if (sess.host_id) {
                    hostId = sess.host_id;
                  }
                } catch { /* fall back to default host */ }
              } else if (routeContext.hostId) {
                hostId = routeContext.hostId;
              }

              const s = await api.sessions.create(hostId, opts);
              void navigate(`/hosts/${hostId}/sessions/${s.id}`);
              setOpen(false);
            } catch (err) {
              showToast(
                `Failed to create session: ${err instanceof Error ? err.message : String(err)}`,
                "error",
              );
            }
          })();
        },
      });
    }

    // Shift+Alt+N → Quick-start Claude session with project defaults
    sa.push({
      shortcut: { shift: true, alt: true, key: "n" },
      onSelect: () => {
        void (async () => {
          try {
            let projectId: string | undefined;
            let hostId: string | undefined;

            // Resolve project context from current route
            if (routeContext.projectId) {
              projectId = routeContext.projectId;
            } else if (routeContext.sessionId) {
              try {
                const sess = await api.sessions.get(routeContext.sessionId);
                if (sess.project_id) projectId = sess.project_id;
                hostId = sess.host_id;
              } catch { /* ignore */ }
            }

            if (!projectId) {
              showToast("No project context for quick Claude start", "error");
              return;
            }

            const proj = await api.projects.get(projectId);
            hostId = proj.host_id;

            // Fetch project settings for claude defaults
            const { settings } = await api.projects.getSettings(projectId);
            const defaults: ClaudeDefaults | undefined = settings?.claude ?? undefined;

            const task = await api.claudeTasks.create({
              host_id: hostId,
              project_path: proj.path,
              project_id: projectId,
              model: defaults?.model ?? "sonnet",
              allowed_tools: defaults?.allowed_tools,
              skip_permissions: defaults?.skip_permissions,
              custom_flags: defaults?.custom_flags,
            });

            void navigate(`/hosts/${hostId}/sessions/${task.session_id}`);
            setOpen(false);
          } catch (err) {
            showToast(
              `Failed to start Claude: ${err instanceof Error ? err.message : String(err)}`,
              "error",
            );
          }
        })();
      },
    });

    return sa;
  }, [shortcutSessions, hosts, navigate, setOpen, routeContext]);

  useGlobalShortcuts(globalShortcutActions);

  // Reset context from route when palette opens
  const prevOpenRef = useRef(false);
  useEffect(() => {
    if (open && !prevOpenRef.current) {
      // Palette just opened - reset to route context
      resetToRouteContext(routeContext);
    }
    prevOpenRef.current = open;
  }, [open, routeContext, resetToRouteContext]);

  // Fetch data based on current context
  useEffect(() => {
    if (!open) return;

    let cancelled = false;

    /** Reset all state to defaults */
    function resetAllState() {
      setProjects([]);
      setSessions([]);
      setLoops([]);
      setCustomActions([]);
      setPromptTemplates([]);
      setProject(null);
      setParentProject(null);
      setSession(null);
      setLoop(null);
      setHasRecentClaudeTask(false);
      // Reset ancestor state
      setAncestorProject(null);
      setAncestorProjectSessions([]);
      setAncestorProjectWorktrees([]);
      setAncestorProjectActions([]);
      setAncestorProjectTemplates([]);
      setAncestorProjectHasRecentClaude(false);
      setAncestorHostProjects([]);
      setAncestorHostSessions([]);
    }

    /** Fetch ancestor project data for a given project_id */
    async function fetchAncestorProjectData(projectId: string): Promise<{
      project: Project | null;
      sessions: Session[];
      worktrees: Project[];
      actions: ProjectAction[];
      templates: PromptTemplate[];
      hasRecentClaude: boolean;
    }> {
      try {
        const [proj, projSessions, projWorktrees, projActions, claudeTasks] = await Promise.all([
          api.projects.get(projectId),
          api.projects.sessions(projectId).catch(() => [] as Session[]),
          api.projects.worktrees(projectId).catch(() => [] as Project[]),
          api.projects.actions(projectId).catch(() => ({ actions: [] as ProjectAction[], prompts: [] as PromptTemplate[] })),
          api.claudeTasks.list({ project_id: projectId, status: "completed" }).catch(() => []),
        ]);
        return {
          project: proj,
          sessions: projSessions,
          worktrees: projWorktrees,
          actions: projActions.actions,
          templates: projActions.prompts ?? [],
          hasRecentClaude: claudeTasks.length > 0,
        };
      } catch {
        return { project: null, sessions: [], worktrees: [], actions: [], templates: [], hasRecentClaude: false };
      }
    }

    async function fetchContextData() {
      // Determine effective context
      let effectiveCtx = ctx;

      // In local mode at global level, treat as host level if there's one online host
      if (isLocal && effectiveCtx.level === "global") {
        const onlineHost = hosts.find((h) => h.status === "online");
        if (onlineHost) {
          effectiveCtx = { level: "host", hostId: onlineHost.id, hostName: onlineHost.hostname };
        }
      }

      // Reset all state at start of each fetch
      resetAllState();

      try {
        switch (effectiveCtx.level) {
          case "global":
            // Just needs hosts, which come from useHosts()
            break;

          case "host": {
            if (!effectiveCtx.hostId) break;
            const [hostProjects, hostSessions] = await Promise.all([
              api.projects.list(effectiveCtx.hostId),
              api.sessions.list(effectiveCtx.hostId),
            ]);
            if (cancelled) return;
            setProjects(hostProjects);
            setSessions(hostSessions);
            break;
          }

          case "project":
          case "worktree": {
            if (!effectiveCtx.projectId) break;
            const [p, pSessions, pWorktrees, pActions] = await Promise.all([
              api.projects.get(effectiveCtx.projectId),
              api.projects.sessions(effectiveCtx.projectId).catch(() => [] as Session[]),
              api.projects.worktrees(effectiveCtx.projectId).catch(() => [] as Project[]),
              api.projects.actions(effectiveCtx.projectId).catch(() => ({ actions: [] as ProjectAction[], prompts: [] as PromptTemplate[] })),
            ]);
            if (cancelled) return;

            // Check if this is actually a worktree
            const isWorktree = p.parent_project_id !== null;
            let parent: Project | null = null;
            if (isWorktree && p.parent_project_id) {
              try {
                parent = await api.projects.get(p.parent_project_id);
              } catch {
                // Parent not found, continue without it
              }
            }
            if (cancelled) return;

            // Check for recent claude tasks
            let hasRecent = false;
            try {
              const tasks = await api.claudeTasks.list({ project_id: effectiveCtx.projectId, status: "completed" });
              hasRecent = tasks.length > 0;
            } catch {
              // Ignore errors
            }
            if (cancelled) return;

            setProject(p);
            setParentProject(parent);
            setProjects(pWorktrees);
            setSessions(pSessions);
            setCustomActions(pActions.actions);
            setPromptTemplates(pActions.prompts ?? []);
            setHasRecentClaudeTask(hasRecent);

            // Update context level if we resolved worktree
            if (isWorktree && effectiveCtx.level === "project") {
              // Replace top of stack with worktree level
              const store = useCommandPaletteStore.getState();
              const stack = [...store.contextStack];
              stack[stack.length - 1] = {
                ...stack[stack.length - 1],
                level: "worktree",
                projectName: p.name,
              };
              // Directly set state to avoid clearing query
              useCommandPaletteStore.setState({ contextStack: stack });
            }

            // Fetch host ancestor data
            const hostId = p.host_id ?? effectiveCtx.hostId;
            if (hostId) {
              const [ancHostProjects, ancHostSessions] = await Promise.all([
                api.projects.list(hostId).catch(() => [] as Project[]),
                api.sessions.list(hostId).catch(() => [] as Session[]),
              ]);
              if (cancelled) return;
              setAncestorHostProjects(ancHostProjects);
              setAncestorHostSessions(ancHostSessions);
            }
            break;
          }

          case "session": {
            if (!effectiveCtx.sessionId || !effectiveCtx.hostId) break;
            const [s, sLoops, sibSessions, ancHostProjects] = await Promise.all([
              api.sessions.get(effectiveCtx.sessionId),
              api.loops.list({ session_id: effectiveCtx.sessionId }),
              api.sessions.list(effectiveCtx.hostId),
              api.projects.list(effectiveCtx.hostId).catch(() => [] as Project[]),
            ]);
            if (cancelled) return;
            setSession(s);
            setLoops(sLoops);
            setSessions(sibSessions);
            setAncestorHostProjects(ancHostProjects);
            setAncestorHostSessions(sibSessions);

            // Fetch project ancestor data if session has a project
            if (s.project_id) {
              const projData = await fetchAncestorProjectData(s.project_id);
              if (cancelled) return;
              setAncestorProject(projData.project);
              setAncestorProjectSessions(projData.sessions);
              setAncestorProjectWorktrees(projData.worktrees);
              setAncestorProjectActions(projData.actions);
              setAncestorProjectTemplates(projData.templates);
              setAncestorProjectHasRecentClaude(projData.hasRecentClaude);
            }
            break;
          }

          case "loop": {
            if (!effectiveCtx.loopId) break;
            const l = await api.loops.get(effectiveCtx.loopId);
            if (cancelled) return;
            setLoop(l);

            // Fetch session for ancestor data
            if (effectiveCtx.sessionId) {
              const [s, sLoops] = await Promise.all([
                api.sessions.get(effectiveCtx.sessionId).catch(() => null),
                api.loops.list({ session_id: effectiveCtx.sessionId }).catch(() => []),
              ]);
              if (cancelled) return;
              if (s) {
                setSession(s);
                setLoops(sLoops);

                // Fetch sibling sessions as ancestor host sessions
                if (effectiveCtx.hostId) {
                  const [ancHostProjects, ancHostSessions] = await Promise.all([
                    api.projects.list(effectiveCtx.hostId).catch(() => [] as Project[]),
                    api.sessions.list(effectiveCtx.hostId).catch(() => [] as Session[]),
                  ]);
                  if (cancelled) return;
                  setAncestorHostProjects(ancHostProjects);
                  setAncestorHostSessions(ancHostSessions);
                  setSessions(ancHostSessions);
                }

                // Fetch project ancestor data if session has a project
                if (s.project_id) {
                  const projData = await fetchAncestorProjectData(s.project_id);
                  if (cancelled) return;
                  setAncestorProject(projData.project);
                  setAncestorProjectSessions(projData.sessions);
                  setAncestorProjectWorktrees(projData.worktrees);
                  setAncestorProjectActions(projData.actions);
                  setAncestorProjectTemplates(projData.templates);
                  setAncestorProjectHasRecentClaude(projData.hasRecentClaude);
                }
              }
            }
            break;
          }
        }
      } catch (err) {
        console.warn("Failed to fetch palette context data:", err);
      }
    }

    void fetchContextData();

    return () => {
      cancelled = true;
    };
  }, [open, ctx.level, ctx.hostId, ctx.projectId, ctx.sessionId, ctx.loopId, isLocal, hosts]);

  // Build action dependencies
  const deps: ActionDeps = useMemo(
    () => ({
      navigate: (path: string) => void navigate(path),
      close: () => setOpen(false),
      pushContext,
      isLocal,
      openAddProject: (hostId: string) => {
        setOpen(false);
        setAddProjectHostId(hostId);
      },
      openStartClaude: (proj: { id: string; name: string; path: string; host_id: string }) => {
        setOpen(false);
        setClaudeDialogProject(proj);
      },
      openRunPrompt: (tmpl: PromptTemplate, proj: { id: string; name: string; path: string; host_id: string }) => {
        setOpen(false);
        setRunPromptState({ template: tmpl, project: proj });
      },
      openActionInput: (action: ProjectAction, proj: { id: string; host_id: string }) => {
        setOpen(false);
        setActionInputState({ action, project: proj });
      },
      openHelp: () => {
        setOpen(false);
        onOpenHelp?.();
      },
    }),
    [navigate, setOpen, pushContext, isLocal, onOpenHelp],
  );

  // Resolve actions based on context
  const resolveData: ResolveData = useMemo(
    () => ({
      hosts,
      projects,
      sessions,
      loops,
      customActions,
      promptTemplates,
      project,
      parentProject,
      session,
      loop,
      hasRecentClaudeTask,
      globalSessions: shortcutSessions,
      ancestorProject,
      ancestorProjectSessions,
      ancestorProjectWorktrees,
      ancestorProjectActions,
      ancestorProjectTemplates,
      ancestorProjectHasRecentClaude,
      ancestorHostProjects,
      ancestorHostSessions,
    }),
    [hosts, projects, sessions, loops, customActions, promptTemplates, project, parentProject, session, loop, hasRecentClaudeTask, shortcutSessions, ancestorProject, ancestorProjectSessions, ancestorProjectWorktrees, ancestorProjectActions, ancestorProjectTemplates, ancestorProjectHasRecentClaude, ancestorHostProjects, ancestorHostSessions],
  );

  // In local mode at global level, resolve as host level with the online host
  const effectiveContext = useMemo((): PaletteContext => {
    if (isLocal && ctx.level === "global") {
      const onlineHost = hosts.find((h) => h.status === "online");
      if (onlineHost) {
        return { level: "host", hostId: onlineHost.id, hostName: onlineHost.hostname };
      }
    }
    return ctx;
  }, [ctx, isLocal, hosts]);

  const actions = useMemo(
    () => resolveActions(effectiveContext, deps, resolveData),
    [effectiveContext, deps, resolveData],
  );

  // Build lookup map from cmdk value string -> action
  const actionsByValue = useMemo(() => {
    const map = new Map<string, PaletteAction>();
    for (const action of actions) {
      const value = [action.label, ...(action.keywords ?? [])].join(" ");
      map.set(value, action);
    }
    return map;
  }, [actions]);

  const hasDrillDownItems = actions.some((a) => a.drillDown);

  // Group actions: current level items (no sourceLevel)
  const currentActions = actions.filter((a) => !a.sourceLevel && a.group === "actions");
  const currentNavigate = actions.filter((a) => !a.sourceLevel && a.group === "navigate");

  // Ancestor level items (sourceLevel is set), ordered
  const levelOrder: ContextLevel[] = ["session", "project", "worktree", "host"];
  const ancestorLevels = levelOrder.filter((level) =>
    actions.some((a) => a.sourceLevel === level),
  );

  // Global items
  const globalGroup = actions.filter((a) => a.group === "global");

  /** Capitalize the first letter of a level name */
  function capitalizeLevel(level: string): string {
    return level.charAt(0).toUpperCase() + level.slice(1);
  }

  return (
    <>
      <Command.Dialog
        open={open}
        onOpenChange={setOpen}
        overlayClassName="fixed inset-0 bg-black/50 z-50"
        contentClassName="fixed inset-0 z-50 flex items-start justify-center pt-[20vh]"
        className="relative w-full max-w-lg overflow-hidden rounded-xl border border-border bg-bg-secondary shadow-2xl"
        loop
        label="Command palette"
        aria-describedby={undefined}
        onKeyDown={(e) => {
          // Read selected item directly from DOM (cmdk marks it with data-selected="true")
          const selectedEl = document.querySelector('[cmdk-item][data-selected="true"]');
          const selectedValue = selectedEl?.getAttribute("data-value") ?? "";
          const selected = actionsByValue.get(selectedValue);

          // Tab or Right Arrow (empty query): drill down into selected item
          if (
            (e.key === "Tab" && !e.shiftKey) ||
            (e.key === "ArrowRight" && query === "")
          ) {
            if (selected?.drillDown) {
              e.preventDefault();
              pushContext(selected.drillDown);
            }
          }

          // Shift+Tab or Left Arrow (empty query): go back one level
          if (
            (e.key === "Tab" && e.shiftKey) ||
            (e.key === "ArrowLeft" && query === "")
          ) {
            if (contextStack.length > 1) {
              e.preventDefault();
              popContext();
            }
          }
        }}
      >
        <CommandPaletteInput
          contextStack={contextStack}
          query={query}
          onQueryChange={setQuery}
          onPopContext={popContext}
          onJumpToIndex={jumpToIndex}
        />

        <Command.List className="max-h-80 overflow-auto p-2">
          <Command.Empty className="px-3 py-6 text-center text-sm text-text-tertiary">
            No results found
          </Command.Empty>

          {currentActions.length > 0 && (
            <Command.Group heading="Actions" className={GROUP_HEADING_CLASS}>
              {currentActions.map((action) => (
                <CommandPaletteItem key={action.id} action={action} />
              ))}
            </Command.Group>
          )}

          {currentNavigate.length > 0 && (
            <Command.Group heading="Navigate" className={GROUP_HEADING_CLASS}>
              {currentNavigate.map((action) => (
                <CommandPaletteItem key={action.id} action={action} />
              ))}
            </Command.Group>
          )}

          {ancestorLevels.map((level) => {
            const levelActions = actions.filter((a) => a.sourceLevel === level);
            const sourceLabel = levelActions[0]?.sourceLabel ?? "";
            const heading = `${capitalizeLevel(level)} \u00B7 ${sourceLabel}`;
            return (
              <Command.Group key={`ancestor-${level}`} heading={heading} className={GROUP_HEADING_CLASS}>
                {levelActions.map((action) => (
                  <CommandPaletteItem key={action.id} action={action} />
                ))}
              </Command.Group>
            );
          })}

          {globalGroup.length > 0 && (
            <Command.Group heading="Global" className={GROUP_HEADING_CLASS}>
              {globalGroup.map((action) => (
                <CommandPaletteItem key={action.id} action={action} />
              ))}
            </Command.Group>
          )}
        </Command.List>

        <CommandPaletteFooter
          canGoBack={contextStack.length > 1}
          canDrillDown={hasDrillDownItems}
          contextLevel={effectiveContext.level}
        />
      </Command.Dialog>

      {addProjectHostId && (
        <AddProjectDialog
          hostId={addProjectHostId}
          open={true}
          onClose={() => setAddProjectHostId(null)}
        />
      )}

      {claudeDialogProject && (
        <StartClaudeDialog
          projectName={claudeDialogProject.name}
          projectPath={claudeDialogProject.path}
          hostId={claudeDialogProject.host_id}
          projectId={claudeDialogProject.id}
          onClose={() => setClaudeDialogProject(null)}
        />
      )}

      {runPromptState && (
        <RunPromptDialog
          template={runPromptState.template}
          projectId={runPromptState.project.id}
          projectPath={runPromptState.project.path}
          hostId={runPromptState.project.host_id}
          projectName={runPromptState.project.name}
          onClose={() => setRunPromptState(null)}
        />
      )}

      {actionInputState && (
        <ActionInputDialog
          action={actionInputState.action}
          projectId={actionInputState.project.id}
          hostId={actionInputState.project.host_id}
          onClose={() => setActionInputState(null)}
        />
      )}
    </>
  );
}

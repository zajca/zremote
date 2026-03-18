import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { Command } from "cmdk";
import { useCommandPaletteStore } from "../../stores/command-palette-store";
import { useCommandPaletteContext } from "../../hooks/useCommandPaletteContext";
import { useDoubleShift } from "../../hooks/useDoubleShift";
import { useMode } from "../../hooks/useMode";
import { useHosts } from "../../hooks/useHosts";
import { api } from "../../lib/api";
import type { Project, ProjectAction, Session } from "../../lib/api";
import type { AgenticLoop } from "../../types/agentic";
import { resolveActions, type ResolveData } from "./actions/registry";
import type { ActionDeps, PaletteAction, PaletteContext } from "./types";
import { CommandPaletteInput } from "./CommandPaletteInput";
import { CommandPaletteItem } from "./CommandPaletteItem";
import { CommandPaletteFooter } from "./CommandPaletteFooter";
import { AddProjectDialog } from "../AddProjectDialog";
import { StartClaudeDialog } from "../StartClaudeDialog";
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

  // Fetched entity state
  const [projects, setProjects] = useState<Project[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loops, setLoops] = useState<AgenticLoop[]>([]);
  const [customActions, setCustomActions] = useState<ProjectAction[]>([]);
  const [project, setProject] = useState<Project | null>(null);
  const [parentProject, setParentProject] = useState<Project | null>(null);
  const [session, setSession] = useState<Session | null>(null);
  const [loop, setLoop] = useState<AgenticLoop | null>(null);
  const [hasRecentClaudeTask, setHasRecentClaudeTask] = useState(false);

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

    // Ctrl+N → New session (on first online host)
    const targetHost = hosts.find((h) => h.status === "online");
    if (targetHost) {
      sa.push({
        shortcut: { mod: true, key: "n" },
        onSelect: () => {
          void (async () => {
            try {
              const s = await api.sessions.create(targetHost.id);
              void navigate(`/hosts/${targetHost.id}/sessions/${s.id}`);
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

    return sa;
  }, [shortcutSessions, hosts, navigate, setOpen]);

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

      try {
        switch (effectiveCtx.level) {
          case "global":
            // Just needs hosts, which come from useHosts()
            setProjects([]);
            setSessions([]);
            setLoops([]);
            setCustomActions([]);
            setProject(null);
            setParentProject(null);
            setSession(null);
            setLoop(null);
            setHasRecentClaudeTask(false);
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
            setLoops([]);
            setCustomActions([]);
            setProject(null);
            setParentProject(null);
            setSession(null);
            setLoop(null);
            setHasRecentClaudeTask(false);
            break;
          }

          case "project":
          case "worktree": {
            if (!effectiveCtx.projectId) break;
            const [p, pSessions, pWorktrees, pActions] = await Promise.all([
              api.projects.get(effectiveCtx.projectId),
              api.projects.sessions(effectiveCtx.projectId).catch(() => [] as Session[]),
              api.projects.worktrees(effectiveCtx.projectId).catch(() => [] as Project[]),
              api.projects.actions(effectiveCtx.projectId).catch(() => ({ actions: [] as ProjectAction[] })),
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
            setLoops([]);
            setSession(null);
            setLoop(null);
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
            break;
          }

          case "session": {
            if (!effectiveCtx.sessionId || !effectiveCtx.hostId) break;
            const [s, sLoops, sibSessions] = await Promise.all([
              api.sessions.get(effectiveCtx.sessionId),
              api.loops.list({ session_id: effectiveCtx.sessionId }),
              api.sessions.list(effectiveCtx.hostId),
            ]);
            if (cancelled) return;
            setSession(s);
            setLoops(sLoops);
            setProjects([]);
            setSessions(sibSessions);
            setCustomActions([]);
            setProject(null);
            setParentProject(null);
            setLoop(null);
            setHasRecentClaudeTask(false);
            break;
          }

          case "loop": {
            if (!effectiveCtx.loopId) break;
            const l = await api.loops.get(effectiveCtx.loopId);
            if (cancelled) return;
            setLoop(l);
            setProjects([]);
            setSessions([]);
            setLoops([]);
            setCustomActions([]);
            setProject(null);
            setParentProject(null);
            setSession(null);
            setHasRecentClaudeTask(false);
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
      project,
      parentProject,
      session,
      loop,
      hasRecentClaudeTask,
      globalSessions: shortcutSessions,
    }),
    [hosts, projects, sessions, loops, customActions, project, parentProject, session, loop, hasRecentClaudeTask, shortcutSessions],
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

  // Group actions
  const actionGroup = actions.filter((a) => a.group === "actions");
  const navigateGroup = actions.filter((a) => a.group === "navigate");
  const globalGroup = actions.filter((a) => a.group === "global");

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

          {actionGroup.length > 0 && (
            <Command.Group heading="Actions" className={GROUP_HEADING_CLASS}>
              {actionGroup.map((action) => (
                <CommandPaletteItem key={action.id} action={action} />
              ))}
            </Command.Group>
          )}

          {navigateGroup.length > 0 && (
            <Command.Group heading="Navigate" className={GROUP_HEADING_CLASS}>
              {navigateGroup.map((action) => (
                <CommandPaletteItem key={action.id} action={action} />
              ))}
            </Command.Group>
          )}

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
    </>
  );
}

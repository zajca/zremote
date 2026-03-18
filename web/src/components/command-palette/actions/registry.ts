import type { Host, Project, ProjectAction, Session } from "../../../lib/api";
import type { AgenticLoop } from "../../../types/agentic";
import type { ShortcutSession } from "../../../hooks/useShortcutSessions";
import type { ActionDeps, PaletteAction, PaletteContext } from "../types";
import { getGlobalActions } from "./global-actions";
import { getHostActions } from "./host-actions";
import { getLoopActions } from "./loop-actions";
import { getProjectActions } from "./project-actions";
import { getSessionActions } from "./session-actions";
import { getWorktreeActions } from "./worktree-actions";

export interface ResolveData {
  hosts: Host[];
  projects: Project[];
  sessions: Session[];
  loops: AgenticLoop[];
  customActions: ProjectAction[];
  // Resolved entities
  project: Project | null;
  parentProject: Project | null;
  session: Session | null;
  loop: AgenticLoop | null;
  hasRecentClaudeTask: boolean;
  globalSessions: ShortcutSession[];
}

export function resolveActions(
  context: PaletteContext,
  deps: ActionDeps,
  data: ResolveData,
): PaletteAction[] {
  const globalActions = getGlobalActions(data.hosts, data.globalSessions, deps);

  switch (context.level) {
    case "global":
      return globalActions;

    case "host": {
      if (!context.hostId) return globalActions;
      const hostActions = getHostActions(
        context.hostId,
        context.hostName,
        data.projects,
        data.sessions,
        deps,
      );
      return [...hostActions, ...globalActions.map((a) => ({ ...a, group: "global" as const }))];
    }

    case "project": {
      if (!context.projectId || !data.project) return globalActions;
      const projectActions = getProjectActions(
        context.projectId,
        data.project,
        data.sessions,
        data.projects.filter((p) => p.parent_project_id === context.projectId),
        data.customActions,
        data.hasRecentClaudeTask,
        deps,
      );
      return [...projectActions, ...globalActions.map((a) => ({ ...a, group: "global" as const }))];
    }

    case "worktree": {
      if (!context.projectId || !data.project) return globalActions;
      const worktreeActions = getWorktreeActions(
        context.projectId,
        data.project,
        data.parentProject,
        data.sessions,
        data.customActions,
        data.hasRecentClaudeTask,
        deps,
      );
      return [...worktreeActions, ...globalActions.map((a) => ({ ...a, group: "global" as const }))];
    }

    case "session": {
      if (!context.sessionId || !context.hostId || !data.session) return globalActions;
      const sessionActions = getSessionActions(
        context.sessionId,
        data.session,
        context.hostId,
        data.loops,
        data.sessions,
        deps,
      );
      return [...sessionActions, ...globalActions.map((a) => ({ ...a, group: "global" as const }))];
    }

    case "loop": {
      if (!context.loopId || !context.sessionId || !context.hostId || !data.loop) return globalActions;
      const loopActions = getLoopActions(
        context.loopId,
        data.loop,
        context.sessionId,
        context.hostId,
        deps,
      );
      return [...loopActions, ...globalActions.map((a) => ({ ...a, group: "global" as const }))];
    }

    default:
      return globalActions;
  }
}

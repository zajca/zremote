import type { Host, Project, ProjectAction, Session } from "../../../lib/api";
import type { AgenticLoop } from "../../../types/agentic";
import type { PromptTemplate } from "../../../types/prompt";
import type { ShortcutSession } from "../../../hooks/useShortcutSessions";
import type { ActionDeps, ContextLevel, PaletteAction, PaletteContext } from "../types";
import { getGlobalActions } from "./global-actions";
import { getHostActions } from "./host-actions";
import { getLoopActions } from "./loop-actions";
import { getProjectActions } from "./project-actions";
import { getSessionActions } from "./session-actions";
import { getWorktreeActions } from "./worktree-actions";
import { getClipboardActions } from "./clipboard-actions";

export interface ResolveData {
  hosts: Host[];
  projects: Project[];
  sessions: Session[];
  loops: AgenticLoop[];
  customActions: ProjectAction[];
  promptTemplates: PromptTemplate[];
  // Resolved entities
  project: Project | null;
  parentProject: Project | null;
  session: Session | null;
  loop: AgenticLoop | null;
  hasRecentClaudeTask: boolean;
  globalSessions: ShortcutSession[];
  // Ancestor data
  ancestorProject: Project | null;
  ancestorProjectSessions: Session[];
  ancestorProjectWorktrees: Project[];
  ancestorProjectActions: ProjectAction[];
  ancestorProjectTemplates: PromptTemplate[];
  ancestorProjectHasRecentClaude: boolean;
  ancestorHostProjects: Project[];
  ancestorHostSessions: Session[];
}

/**
 * Tag actions from an ancestor level: keep only "actions" group items, set sourceLevel/sourceLabel.
 * Navigate/drill-down items from ancestors are intentionally excluded to avoid clutter
 * and duplication with the current level's navigation items.
 */
function tagAncestorActions(
  actions: PaletteAction[],
  sourceLevel: ContextLevel,
  sourceLabel: string,
): PaletteAction[] {
  return actions
    .filter((a) => a.group === "actions")
    .map((a) => ({ ...a, sourceLevel, sourceLabel }));
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
        data.promptTemplates,
        data.hasRecentClaudeTask,
        deps,
      );

      // Host ancestor actions
      const hostAncestorActions = context.hostId
        ? tagAncestorActions(
            getHostActions(context.hostId, context.hostName, data.ancestorHostProjects, data.ancestorHostSessions, deps),
            "host",
            context.hostName ?? "Host",
          )
        : [];

      return [...projectActions, ...hostAncestorActions, ...globalActions.map((a) => ({ ...a, group: "global" as const }))];
    }

    case "worktree": {
      if (!context.projectId || !data.project) return globalActions;
      const worktreeActions = getWorktreeActions(
        context.projectId,
        data.project,
        data.parentProject,
        data.sessions,
        data.customActions,
        data.promptTemplates,
        data.hasRecentClaudeTask,
        deps,
      );

      // Host ancestor actions
      const hostAncestorActions = context.hostId
        ? tagAncestorActions(
            getHostActions(context.hostId, context.hostName, data.ancestorHostProjects, data.ancestorHostSessions, deps),
            "host",
            context.hostName ?? "Host",
          )
        : [];

      return [...worktreeActions, ...hostAncestorActions, ...globalActions.map((a) => ({ ...a, group: "global" as const }))];
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

      // Project ancestor actions (only if session has a project)
      const projectAncestorActions = data.ancestorProject
        ? tagAncestorActions(
            getProjectActions(
              data.ancestorProject.id,
              data.ancestorProject,
              data.ancestorProjectSessions,
              data.ancestorProjectWorktrees,
              data.ancestorProjectActions,
              data.ancestorProjectTemplates,
              data.ancestorProjectHasRecentClaude,
              deps,
            ),
            "project",
            data.ancestorProject.name,
          )
        : [];

      // Host ancestor actions
      const hostAncestorActions = context.hostId
        ? tagAncestorActions(
            getHostActions(context.hostId, context.hostName, data.ancestorHostProjects, data.ancestorHostSessions, deps),
            "host",
            context.hostName ?? "Host",
          )
        : [];

      return [...sessionActions, ...projectAncestorActions, ...hostAncestorActions, ...globalActions.map((a) => ({ ...a, group: "global" as const }))];
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

      // Session ancestor actions (if we have session data)
      const sessionAncestorActions = data.session
        ? tagAncestorActions(
            getSessionActions(
              context.sessionId,
              data.session,
              context.hostId,
              data.loops,
              data.sessions,
              deps,
            ),
            "session",
            data.session.name ?? `Session ${context.sessionId.slice(0, 8)}`,
          )
        : [];

      // Project ancestor actions (only if session has a project)
      const projectAncestorActions = data.ancestorProject
        ? tagAncestorActions(
            getProjectActions(
              data.ancestorProject.id,
              data.ancestorProject,
              data.ancestorProjectSessions,
              data.ancestorProjectWorktrees,
              data.ancestorProjectActions,
              data.ancestorProjectTemplates,
              data.ancestorProjectHasRecentClaude,
              deps,
            ),
            "project",
            data.ancestorProject.name,
          )
        : [];

      // Host ancestor actions
      const hostAncestorActions = context.hostId
        ? tagAncestorActions(
            getHostActions(context.hostId, context.hostName, data.ancestorHostProjects, data.ancestorHostSessions, deps),
            "host",
            context.hostName ?? "Host",
          )
        : [];

      return [...loopActions, ...sessionAncestorActions, ...projectAncestorActions, ...hostAncestorActions, ...globalActions.map((a) => ({ ...a, group: "global" as const }))];
    }

    case "clipboard": {
      const clipboardActions = getClipboardActions(deps);
      return [...clipboardActions, ...globalActions.map((a) => ({ ...a, group: "global" as const }))];
    }

    default:
      return globalActions;
  }
}

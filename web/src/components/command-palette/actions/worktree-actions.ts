import {
  ArrowUp,
  Bot,
  BookOpen,
  FileText,
  Monitor,
  Play,
  RefreshCw,
  Settings,
  Terminal,
  Trash2,
  Zap,
} from "lucide-react";
import type { Project, ProjectAction, Session } from "../../../lib/api";
import type { PromptTemplate } from "../../../types/prompt";
import { api, startClaudeForProject } from "../../../lib/api";
import { showToast } from "../../layout/Toast";
import { hasScope } from "../../project/action-utils";
import type { ActionDeps, PaletteAction } from "../types";

export function getWorktreeActions(
  worktreeId: string,
  worktree: Project,
  parentProject: Project | null,
  sessions: Session[],
  customActions: ProjectAction[],
  promptTemplates: PromptTemplate[],
  hasRecentClaudeTask: boolean,
  deps: ActionDeps,
): PaletteAction[] {
  const actions: PaletteAction[] = [];

  // Navigate to parent project
  if (parentProject) {
    actions.push({
      id: `worktree:${worktreeId}:parent`,
      label: `Go to ${parentProject.name}`,
      icon: ArrowUp,
      keywords: ["parent", "project", parentProject.name],
      group: "navigate",
      onSelect: () => {
        deps.navigate(`/projects/${parentProject.id}`);
        deps.close();
      },
    });
  }

  // Actions group
  actions.push({
    id: `worktree:${worktreeId}:start-claude`,
    label: "Start Claude",
    icon: Bot,
    keywords: ["claude", "ai", "start", "agent"],
    group: "actions",
    onSelect: async () => {
      try {
        const { settings } = await api.projects.getSettings(worktreeId);
        const defaults = settings?.claude;
        const { hostId, sessionId } = await startClaudeForProject(
          worktree.host_id,
          worktree.path,
          worktree.id,
          {
            model: defaults?.model,
            allowedTools: defaults?.allowed_tools,
            skipPermissions: defaults?.skip_permissions,
            customFlags: defaults?.custom_flags,
          },
        );
        deps.navigate(`/hosts/${hostId}/sessions/${sessionId}`);
        deps.close();
      } catch (err) {
        showToast(`Failed to start Claude: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  if (hasRecentClaudeTask) {
    actions.push({
      id: `worktree:${worktreeId}:resume-claude`,
      label: "Resume last Claude task",
      icon: Play,
      keywords: ["resume", "claude", "continue", "task"],
      group: "actions",
      onSelect: async () => {
        try {
          const tasks = await api.claudeTasks.list({ project_id: worktreeId, status: "completed" });
          const latest = tasks[0];
          if (latest) {
            await api.claudeTasks.resume(latest.id);
            showToast("Claude task resumed", "success");
            deps.close();
          }
        } catch (err) {
          showToast(`Failed to resume: ${err instanceof Error ? err.message : String(err)}`, "error");
        }
      },
    });
  }

  actions.push({
    id: `worktree:${worktreeId}:new-session`,
    label: "New terminal session",
    icon: Terminal,
    keywords: ["terminal", "session", "new", "shell"],
    group: "actions",
    onSelect: async () => {
      try {
        const session = await api.sessions.create(worktree.host_id, {
          workingDir: worktree.path,
        });
        deps.navigate(`/hosts/${worktree.host_id}/sessions/${session.id}`);
        deps.close();
      } catch (err) {
        showToast(`Failed to create session: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  actions.push({
    id: `worktree:${worktreeId}:refresh-git`,
    label: "Refresh git info",
    icon: RefreshCw,
    keywords: ["git", "refresh", "update"],
    group: "actions",
    onSelect: async () => {
      try {
        await api.projects.refreshGit(worktreeId);
        showToast("Git info refreshed", "success");
        deps.close();
      } catch (err) {
        showToast(`Failed to refresh: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  actions.push({
    id: `worktree:${worktreeId}:configure-claude`,
    label: "Configure with Claude",
    icon: Settings,
    keywords: ["configure", "claude", "setup"],
    group: "actions",
    onSelect: async () => {
      try {
        await api.projects.configureWithClaude(worktreeId);
        showToast("Configuration started", "success");
        deps.close();
      } catch (err) {
        showToast(`Failed to configure: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  actions.push({
    id: `worktree:${worktreeId}:kb-index`,
    label: "Trigger KB indexing",
    icon: BookOpen,
    keywords: ["knowledge", "index", "kb"],
    group: "actions",
    onSelect: async () => {
      try {
        await api.knowledge.triggerIndex(worktreeId);
        showToast("KB indexing triggered", "success");
        deps.close();
      } catch (err) {
        showToast(`Failed to trigger indexing: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  actions.push({
    id: `worktree:${worktreeId}:settings`,
    label: "Worktree settings",
    icon: Settings,
    keywords: ["settings", "preferences", "config", "worktree"],
    group: "actions",
    onSelect: () => {
      deps.navigate(`/projects/${worktreeId}`);
      deps.close();
    },
  });

  actions.push({
    id: `worktree:${worktreeId}:delete`,
    label: "Delete worktree",
    icon: Trash2,
    keywords: ["delete", "remove", "worktree"],
    group: "actions",
    dangerous: true,
    onSelect: async () => {
      if (!worktree.parent_project_id) return;
      try {
        await api.projects.deleteWorktree(worktree.parent_project_id, worktreeId);
        showToast("Worktree deleted", "success");
        deps.navigate(parentProject ? `/projects/${parentProject.id}` : "/");
        deps.close();
      } catch (err) {
        showToast(`Failed to delete: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  // Navigate group: sessions
  const activeSessions = sessions.filter((s) => s.status === "active" || s.status === "suspended");
  for (const session of activeSessions) {
    const name = session.name ?? `Session ${session.id.slice(0, 8)}`;
    actions.push({
      id: `worktree:${worktreeId}:session:${session.id}`,
      label: name,
      icon: Monitor,
      keywords: ["session", "terminal", name],
      group: "navigate",
      onSelect: () => {
        deps.pushContext({
          level: "session",
          hostId: worktree.host_id,
          sessionId: session.id,
          sessionName: name,
        });
      },
      drillDown: {
        level: "session",
        hostId: worktree.host_id,
        sessionId: session.id,
        sessionName: name,
      },
    });
  }

  // Custom actions (only command_palette-scoped)
  const paletteActions = customActions.filter((a) => hasScope(a, "command_palette"));
  for (const action of paletteActions) {
    // If action has custom inputs, open the input dialog
    if (action.inputs && action.inputs.length > 0) {
      actions.push({
        id: `worktree:${worktreeId}:action:${action.name}`,
        label: action.description ?? action.name,
        icon: Zap,
        keywords: ["action", "custom", action.name],
        group: "actions",
        onSelect: () => {
          deps.openActionInput(action, { id: worktree.id, host_id: worktree.host_id });
        },
      });
    } else {
      actions.push({
        id: `worktree:${worktreeId}:action:${action.name}`,
        label: action.description ?? action.name,
        icon: Zap,
        keywords: ["action", "custom", action.name],
        group: "actions",
        onSelect: async () => {
          try {
            const result = await api.projects.runAction(worktreeId, action.name, {
              worktree_path: worktree.path,
              branch: worktree.git_branch ?? undefined,
            });
            if (result.session_id) {
              deps.navigate(`/hosts/${worktree.host_id}/sessions/${result.session_id}`);
            }
            showToast(`Action "${action.name}" started`, "success");
            deps.close();
          } catch (err) {
            showToast(`Action failed: ${err instanceof Error ? err.message : String(err)}`, "error");
          }
        },
      });
    }
  }

  // Prompt templates
  for (const tmpl of promptTemplates) {
    actions.push({
      id: `worktree:${worktreeId}:prompt:${tmpl.name}`,
      label: tmpl.description ?? tmpl.name,
      icon: FileText,
      keywords: ["prompt", "template", tmpl.name],
      group: "actions",
      onSelect: () => {
        deps.openRunPrompt(tmpl, {
          id: worktree.id,
          name: worktree.name,
          path: worktree.path,
          host_id: worktree.host_id,
        });
      },
    });
  }

  return actions;
}

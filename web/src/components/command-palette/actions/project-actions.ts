import {
  Bot,
  FileText,
  GitBranch,
  Monitor,
  Play,
  RefreshCw,
  Settings,
  Terminal,
  Trash2,
  BookOpen,
  Zap,
  FolderOpen,
} from "lucide-react";
import type { Project, ProjectAction, Session } from "../../../lib/api";
import type { PromptTemplate } from "../../../types/prompt";
import { api, startClaudeForProject } from "../../../lib/api";
import { showToast } from "../../layout/Toast";
import { hasScope } from "../../project/action-utils";
import type { ActionDeps, PaletteAction } from "../types";

export function getProjectActions(
  projectId: string,
  project: Project,
  sessions: Session[],
  worktrees: Project[],
  customActions: ProjectAction[],
  promptTemplates: PromptTemplate[],
  hasRecentClaudeTask: boolean,
  deps: ActionDeps,
): PaletteAction[] {
  const actions: PaletteAction[] = [];

  // Actions group
  actions.push({
    id: `project:${projectId}:start-claude`,
    label: "Start Claude",
    icon: Bot,
    keywords: ["claude", "ai", "start", "agent"],
    group: "actions",
    onSelect: async () => {
      try {
        const { settings } = await api.projects.getSettings(projectId);
        const defaults = settings?.claude;
        const { hostId, sessionId } = await startClaudeForProject(
          project.host_id,
          project.path,
          project.id,
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
      id: `project:${projectId}:resume-claude`,
      label: "Resume last Claude task",
      icon: Play,
      keywords: ["resume", "claude", "continue", "task"],
      group: "actions",
      onSelect: async () => {
        try {
          const tasks = await api.claudeTasks.list({ project_id: projectId, status: "completed" });
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
    id: `project:${projectId}:new-session`,
    label: "New terminal session",
    icon: Terminal,
    keywords: ["terminal", "session", "new", "shell"],
    group: "actions",
    onSelect: async () => {
      try {
        const session = await api.sessions.create(project.host_id, {
          workingDir: project.path,
        });
        deps.navigate(`/hosts/${project.host_id}/sessions/${session.id}`);
        deps.close();
      } catch (err) {
        showToast(`Failed to create session: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  actions.push({
    id: `project:${projectId}:create-worktree`,
    label: "Create worktree",
    icon: GitBranch,
    keywords: ["worktree", "branch", "git", "create"],
    group: "actions",
    onSelect: () => {
      // Navigate to project page where worktree creation UI exists
      deps.navigate(`/projects/${projectId}`);
      deps.close();
    },
  });

  actions.push({
    id: `project:${projectId}:refresh-git`,
    label: "Refresh git info",
    icon: RefreshCw,
    keywords: ["git", "refresh", "update", "pull"],
    group: "actions",
    onSelect: async () => {
      try {
        await api.projects.refreshGit(projectId);
        showToast("Git info refreshed", "success");
        deps.close();
      } catch (err) {
        showToast(`Failed to refresh: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  actions.push({
    id: `project:${projectId}:configure-claude`,
    label: "Configure with Claude",
    icon: Settings,
    keywords: ["configure", "claude", "setup"],
    group: "actions",
    onSelect: async () => {
      try {
        await api.projects.configureWithClaude(projectId);
        showToast("Configuration started", "success");
        deps.close();
      } catch (err) {
        showToast(`Failed to configure: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  actions.push({
    id: `project:${projectId}:kb-index`,
    label: "Trigger KB indexing",
    icon: BookOpen,
    keywords: ["knowledge", "index", "kb"],
    group: "actions",
    onSelect: async () => {
      try {
        await api.knowledge.triggerIndex(projectId);
        showToast("KB indexing triggered", "success");
        deps.close();
      } catch (err) {
        showToast(`Failed to trigger indexing: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  actions.push({
    id: `project:${projectId}:settings`,
    label: "Project settings",
    icon: Settings,
    keywords: ["settings", "preferences", "config", "project"],
    group: "actions",
    onSelect: () => {
      deps.navigate(`/projects/${projectId}`);
      deps.close();
    },
  });

  actions.push({
    id: `project:${projectId}:delete`,
    label: "Delete project",
    icon: Trash2,
    keywords: ["delete", "remove", "project"],
    group: "actions",
    dangerous: true,
    onSelect: async () => {
      try {
        await api.projects.delete(projectId);
        showToast("Project deleted", "success");
        deps.navigate("/");
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
      id: `project:${projectId}:session:${session.id}`,
      label: name,
      icon: Monitor,
      keywords: ["session", "terminal", name],
      group: "navigate",
      onSelect: () => {
        deps.pushContext({
          level: "session",
          hostId: project.host_id,
          sessionId: session.id,
          projectName: project.name,
          sessionName: name,
        });
      },
      drillDown: {
        level: "session",
        hostId: project.host_id,
        sessionId: session.id,
        projectName: project.name,
        sessionName: name,
      },
    });
  }

  // Navigate group: worktrees
  for (const wt of worktrees) {
    actions.push({
      id: `project:${projectId}:worktree:${wt.id}`,
      label: `${wt.name} (${wt.git_branch ?? "worktree"})`,
      icon: FolderOpen,
      keywords: ["worktree", wt.name, wt.git_branch ?? ""],
      group: "navigate",
      onSelect: () => {
        deps.pushContext({
          level: "worktree",
          hostId: project.host_id,
          projectId: wt.id,
          projectName: wt.name,
        });
      },
      drillDown: {
        level: "worktree",
        hostId: project.host_id,
        projectId: wt.id,
        projectName: wt.name,
      },
    });
  }

  // Custom actions (only command_palette-scoped)
  const paletteActions = customActions.filter((a) => hasScope(a, "command_palette"));
  for (const action of paletteActions) {
    // If action has custom inputs, open the input dialog
    if (action.inputs && action.inputs.length > 0) {
      actions.push({
        id: `project:${projectId}:action:${action.name}`,
        label: action.description ?? action.name,
        icon: Zap,
        keywords: ["action", "custom", action.name],
        group: "actions",
        onSelect: () => {
          deps.openActionInput(action, { id: project.id, host_id: project.host_id });
        },
      });
    } else {
      actions.push({
        id: `project:${projectId}:action:${action.name}`,
        label: action.description ?? action.name,
        icon: Zap,
        keywords: ["action", "custom", action.name],
        group: "actions",
        onSelect: async () => {
          try {
            const result = await api.projects.runAction(projectId, action.name);
            if (result.session_id) {
              deps.navigate(`/hosts/${project.host_id}/sessions/${result.session_id}`);
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
      id: `project:${projectId}:prompt:${tmpl.name}`,
      label: tmpl.description ?? tmpl.name,
      icon: FileText,
      keywords: ["prompt", "template", tmpl.name],
      group: "actions",
      onSelect: () => {
        deps.openRunPrompt(tmpl, {
          id: project.id,
          name: project.name,
          path: project.path,
          host_id: project.host_id,
        });
      },
    });
  }

  return actions;
}

import {
  FolderPlus,
  FolderSearch,
  FolderOpen,
  Terminal,
  Monitor,
} from "lucide-react";
import type { Project, Session } from "../../../lib/api";
import { api } from "../../../lib/api";
import { showToast } from "../../layout/Toast";
import type { ActionDeps, PaletteAction } from "../types";

export function getHostActions(
  hostId: string,
  hostName: string | undefined,
  projects: Project[],
  sessions: Session[],
  deps: ActionDeps,
): PaletteAction[] {
  const actions: PaletteAction[] = [];

  // Actions group
  actions.push({
    id: `host:${hostId}:new-session`,
    label: "New terminal session",
    icon: Terminal,
    keywords: ["terminal", "session", "new", "shell"],
    group: "actions",
    shortcut: { alt: true, key: "n" },
    onSelect: async () => {
      try {
        const session = await api.sessions.create(hostId);
        deps.navigate(`/hosts/${hostId}/sessions/${session.id}`);
        deps.close();
      } catch (err) {
        showToast(`Failed to create session: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  actions.push({
    id: `host:${hostId}:scan-projects`,
    label: "Scan for projects",
    icon: FolderSearch,
    keywords: ["scan", "discover", "find", "projects"],
    group: "actions",
    onSelect: async () => {
      try {
        await api.projects.scan(hostId);
        showToast("Project scan started", "success");
        deps.close();
      } catch (err) {
        showToast(`Scan failed: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  actions.push({
    id: `host:${hostId}:add-project`,
    label: "Add project",
    icon: FolderPlus,
    keywords: ["add", "project", "new"],
    group: "actions",
    onSelect: () => {
      deps.openAddProject(hostId);
    },
  });

  // Navigate group: projects
  for (const project of projects ?? []) {
    actions.push({
      id: `host:${hostId}:project:${project.id}`,
      label: project.name,
      icon: FolderOpen,
      keywords: ["project", project.name, project.path],
      group: "navigate",
      onSelect: () => {
        deps.pushContext({
          level: project.parent_project_id ? "worktree" : "project",
          hostId,
          projectId: project.id,
          hostName: hostName,
          projectName: project.name,
        });
      },
      drillDown: {
        level: project.parent_project_id ? "worktree" : "project",
        hostId,
        projectId: project.id,
        hostName: hostName,
        projectName: project.name,
      },
    });
  }

  // Navigate group: sessions
  const activeSessions = (sessions ?? []).filter((s) => s.status === "active" || s.status === "suspended");
  for (const session of activeSessions) {
    const name = session.name ?? `Session ${session.id.slice(0, 8)}`;
    actions.push({
      id: `host:${hostId}:session:${session.id}`,
      label: name,
      icon: Monitor,
      keywords: ["session", "terminal", name],
      group: "navigate",
      onSelect: () => {
        deps.pushContext({
          level: "session",
          hostId,
          sessionId: session.id,
          hostName: hostName,
          sessionName: name,
        });
      },
      drillDown: {
        level: "session",
        hostId,
        sessionId: session.id,
        hostName: hostName,
        sessionName: name,
      },
    });
  }

  return actions;
}

import {
  Edit3,
  FolderOpen,
  Laptop,
  Monitor,
  Trash2,
} from "lucide-react";
import type { Session } from "../../../lib/api";
import { api } from "../../../lib/api";
import { showToast } from "../../layout/Toast";
import type { AgenticLoop } from "../../../types/agentic";
import type { ActionDeps, PaletteAction } from "../types";

export function getSessionActions(
  sessionId: string,
  session: Session,
  hostId: string,
  loops: AgenticLoop[],
  deps: ActionDeps,
): PaletteAction[] {
  const actions: PaletteAction[] = [];

  // Actions group
  actions.push({
    id: `session:${sessionId}:rename`,
    label: "Rename session",
    icon: Edit3,
    keywords: ["rename", "name", "edit"],
    group: "actions",
    onSelect: () => {
      // Navigate to the session page where rename functionality exists
      deps.navigate(`/hosts/${hostId}/sessions/${sessionId}`);
      deps.close();
    },
  });

  actions.push({
    id: `session:${sessionId}:close`,
    label: "Close session",
    icon: Trash2,
    keywords: ["close", "end", "kill", "terminate"],
    group: "actions",
    dangerous: true,
    onSelect: async () => {
      try {
        await api.sessions.close(sessionId);
        showToast("Session closed", "success");
        deps.navigate(`/hosts/${hostId}`);
        deps.close();
      } catch (err) {
        showToast(`Failed to close session: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
  });

  // Navigate group
  actions.push({
    id: `session:${sessionId}:go-host`,
    label: "Go to host",
    icon: Laptop,
    keywords: ["host", "back", "up"],
    group: "navigate",
    onSelect: () => {
      deps.navigate(`/hosts/${hostId}`);
      deps.close();
    },
  });

  if (session.project_id) {
    actions.push({
      id: `session:${sessionId}:go-project`,
      label: "Go to project",
      icon: FolderOpen,
      keywords: ["project", "back"],
      group: "navigate",
      onSelect: () => {
        deps.navigate(`/projects/${session.project_id}`);
        deps.close();
      },
    });
  }

  // Loop drill-down items
  for (const loop of loops) {
    const label = loop.task_name ?? loop.tool_name ?? `Loop ${loop.id.slice(0, 8)}`;
    actions.push({
      id: `session:${sessionId}:loop:${loop.id}`,
      label,
      icon: Monitor,
      keywords: ["loop", "agentic", label],
      group: "navigate",
      onSelect: () => {
        deps.pushContext({
          level: "loop",
          hostId,
          sessionId,
          loopId: loop.id,
          sessionName: session.name ?? `Session ${sessionId.slice(0, 8)}`,
        });
      },
      drillDown: {
        level: "loop",
        hostId,
        sessionId,
        loopId: loop.id,
      },
    });
  }

  return actions;
}

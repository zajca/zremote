import {
  Check,
  Eye,
  FolderOpen,
  Monitor,
  Pause,
  Play,
  Square,
  X,
} from "lucide-react";
import { api } from "../../../lib/api";
import { showToast } from "../../layout/Toast";
import type { AgenticLoop } from "../../../types/agentic";
import type { ActionDeps, PaletteAction } from "../types";

export function getLoopActions(
  loopId: string,
  loop: AgenticLoop,
  sessionId: string,
  hostId: string,
  deps: ActionDeps,
): PaletteAction[] {
  const actions: PaletteAction[] = [];

  // Actions group (conditional on loop status)
  if (loop.status === "waiting_for_input") {
    actions.push({
      id: `loop:${loopId}:approve`,
      label: "Approve pending",
      icon: Check,
      keywords: ["approve", "accept", "yes", "allow"],
      group: "actions",
      onSelect: async () => {
        try {
          await api.loops.action(loopId, "approve");
          showToast("Approved", "success");
          deps.close();
        } catch (err) {
          showToast(`Failed: ${err instanceof Error ? err.message : String(err)}`, "error");
        }
      },
    });

    actions.push({
      id: `loop:${loopId}:reject`,
      label: "Reject pending",
      icon: X,
      keywords: ["reject", "deny", "no"],
      group: "actions",
      onSelect: async () => {
        try {
          await api.loops.action(loopId, "reject");
          showToast("Rejected", "success");
          deps.close();
        } catch (err) {
          showToast(`Failed: ${err instanceof Error ? err.message : String(err)}`, "error");
        }
      },
    });
  }

  if (loop.status === "working") {
    actions.push({
      id: `loop:${loopId}:pause`,
      label: "Pause loop",
      icon: Pause,
      keywords: ["pause", "hold"],
      group: "actions",
      onSelect: async () => {
        try {
          await api.loops.action(loopId, "pause");
          showToast("Loop paused", "success");
          deps.close();
        } catch (err) {
          showToast(`Failed: ${err instanceof Error ? err.message : String(err)}`, "error");
        }
      },
    });
  }

  if (loop.status === "paused") {
    actions.push({
      id: `loop:${loopId}:resume`,
      label: "Resume loop",
      icon: Play,
      keywords: ["resume", "continue", "unpause"],
      group: "actions",
      onSelect: async () => {
        try {
          await api.loops.action(loopId, "resume");
          showToast("Loop resumed", "success");
          deps.close();
        } catch (err) {
          showToast(`Failed: ${err instanceof Error ? err.message : String(err)}`, "error");
        }
      },
    });
  }

  // Stop is available for working, waiting, paused
  if (loop.status === "working" || loop.status === "waiting_for_input" || loop.status === "paused") {
    actions.push({
      id: `loop:${loopId}:stop`,
      label: "Stop loop",
      icon: Square,
      keywords: ["stop", "kill", "terminate", "end"],
      group: "actions",
      dangerous: true,
      onSelect: async () => {
        try {
          await api.loops.action(loopId, "stop");
          showToast("Loop stopped", "success");
          deps.close();
        } catch (err) {
          showToast(`Failed: ${err instanceof Error ? err.message : String(err)}`, "error");
        }
      },
    });
  }

  actions.push({
    id: `loop:${loopId}:view-transcript`,
    label: "View transcript",
    icon: Eye,
    keywords: ["transcript", "view", "conversation", "log"],
    group: "actions",
    onSelect: () => {
      deps.navigate(`/hosts/${hostId}/sessions/${sessionId}/loops/${loopId}`);
      deps.close();
    },
  });

  // Navigate group
  actions.push({
    id: `loop:${loopId}:go-session`,
    label: "Go to session",
    icon: Monitor,
    keywords: ["session", "back", "up"],
    group: "navigate",
    onSelect: () => {
      deps.navigate(`/hosts/${hostId}/sessions/${sessionId}`);
      deps.close();
    },
  });

  if (loop.project_path) {
    actions.push({
      id: `loop:${loopId}:go-project`,
      label: "Go to project",
      icon: FolderOpen,
      keywords: ["project"],
      group: "navigate",
      onSelect: () => {
        // We don't have project ID here, so navigate to session
        deps.navigate(`/hosts/${hostId}/sessions/${sessionId}`);
        deps.close();
      },
    });
  }

  return actions;
}

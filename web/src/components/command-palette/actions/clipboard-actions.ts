import { ClipboardCopy, Trash2 } from "lucide-react";
import { useClipboardStore } from "../../../stores/clipboard-store";
import { useActiveTerminalStore } from "../../../stores/active-terminal-store";
import { showToast } from "../../layout/Toast";
import type { ActionDeps, PaletteAction } from "../types";

function formatRelativeTime(timestamp: number): string {
  const diff = Date.now() - timestamp;
  const seconds = Math.floor(diff / 1000);
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${String(minutes)}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${String(hours)}h ago`;
  const days = Math.floor(hours / 24);
  return `${String(days)}d ago`;
}

export function getClipboardActions(deps: ActionDeps): PaletteAction[] {
  const entries = useClipboardStore.getState().entries;
  const activeTerminal = useActiveTerminalStore.getState();
  const hasActiveTerminal = !!(activeTerminal.sendInput && activeTerminal.sessionId);

  // Empty state
  if (entries.length === 0) {
    return [{
      id: "clipboard:empty",
      label: "No clipboard history",
      description: "Select text in a terminal to start copying",
      icon: ClipboardCopy,
      keywords: [],
      group: "actions" as const,
      onSelect: () => { deps.close(); },
    }];
  }

  const actions: PaletteAction[] = entries.map((entry) => {
    // Show what will happen on select
    const actionHint = hasActiveTerminal ? "paste into terminal" : "copy to system clipboard";
    const sourceName = entry.source?.sessionName ?? "terminal";

    return {
      id: `clipboard:${entry.id}`,
      label: entry.preview,
      description: `${sourceName} · ${formatRelativeTime(entry.timestamp)} · ${actionHint}`,
      icon: ClipboardCopy,
      keywords: [entry.text.slice(0, 200)],
      group: "actions" as const,
      onSelect: () => {
        if (hasActiveTerminal) {
          activeTerminal.sendInput!(entry.text);
          deps.close();
          showToast("Pasted into terminal", "success");
        } else {
          void navigator.clipboard.writeText(entry.text).then(
            () => showToast("Copied to clipboard", "success"),
            () => showToast("Failed to copy", "error"),
          );
          deps.close();
        }
      },
    };
  });

  // Clear all action at the end
  actions.push({
    id: "clipboard:clear-all",
    label: "Clear clipboard history",
    icon: Trash2,
    keywords: ["clear", "delete", "remove", "reset"],
    group: "actions" as const,
    dangerous: true,
    onSelect: () => {
      useClipboardStore.getState().clearAll();
      deps.close();
      showToast("Clipboard history cleared", "success");
    },
  });

  return actions;
}

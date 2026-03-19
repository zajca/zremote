import { ArrowLeftRight, BarChart3, Clock, HelpCircle, Laptop, Monitor, Search, Settings } from "lucide-react";
import type { Host } from "../../../lib/api";
import type { ShortcutSession } from "../../../hooks/useShortcutSessions";
import type { ActionDeps, PaletteAction } from "../types";

export function getGlobalActions(
  hosts: Host[],
  globalSessions: ShortcutSession[],
  deps: ActionDeps,
): PaletteAction[] {
  const actions: PaletteAction[] = [];

  // Actions group
  actions.push({
    id: "global:search-transcripts",
    label: "Search transcripts",
    icon: Search,
    keywords: ["search", "find", "transcript", "history"],
    group: "actions",
    onSelect: () => {
      deps.navigate("/history");
      deps.close();
    },
  });

  // Navigate group
  actions.push({
    id: "global:analytics",
    label: "Open Analytics",
    icon: BarChart3,
    keywords: ["analytics", "stats", "statistics", "charts"],
    group: "navigate",
    onSelect: () => {
      deps.navigate("/analytics");
      deps.close();
    },
  });

  actions.push({
    id: "global:history",
    label: "Open History",
    icon: Clock,
    keywords: ["history", "past", "log"],
    group: "navigate",
    onSelect: () => {
      deps.navigate("/history");
      deps.close();
    },
  });

  actions.push({
    id: "global:settings",
    label: "Open Settings",
    icon: Settings,
    keywords: ["settings", "preferences", "config"],
    group: "navigate",
    shortcut: { mod: true, key: "," },
    onSelect: () => {
      deps.navigate("/settings");
      deps.close();
    },
  });

  actions.push({
    id: "global:help",
    label: "Show keyboard shortcuts",
    icon: HelpCircle,
    keywords: ["help", "shortcuts", "keyboard", "keys", "hotkeys"],
    group: "actions",
    shortcut: { key: "?" },
    onSelect: () => {
      deps.close();
      deps.openHelp();
    },
  });

  // Switch Session action (before session items)
  actions.push({
    id: "global:switch-session",
    label: "Switch Session",
    icon: ArrowLeftRight,
    keywords: ["switch", "session", "jump", "terminal"],
    group: "actions",
    shortcut: { mod: true, shift: true, key: "s" },
    onSelect: () => deps.openWithFilter("sessions"),
  });

  // Sessions (shown at global level with Ctrl+1-9 shortcuts)
  for (let i = 0; i < globalSessions.length; i++) {
    const s = globalSessions[i];
    if (!s) continue;
    const label = s.hostName ? `${s.hostName}: ${s.name}` : s.name;

    // Build description: projectName + hostName (server mode) + workingDir
    const descParts: string[] = [];
    if (s.projectName) descParts.push(s.projectName);
    if (s.hostName) descParts.push(s.hostName);
    if (s.workingDir) descParts.push(s.workingDir);
    const description = descParts.length > 0 ? descParts.join(" \u00B7 ") : undefined;

    actions.push({
      id: `global:session:${s.sessionId}`,
      label,
      icon: Monitor,
      keywords: ["session", "terminal", s.name, s.hostName ?? "", s.projectName ?? "", s.workingDir ?? ""],
      group: "navigate",
      shortcut: i < 9 ? { mod: true, key: String(i + 1) } : undefined,
      description,
      statusColor: s.status === "active" ? "bg-green-400" : "bg-amber-400",
      showAgenticIndicator: s.hasAgenticLoop === true,
      onSelect: () => {
        deps.navigate(`/hosts/${s.hostId}/sessions/${s.sessionId}`);
        deps.close();
      },
      drillDown: {
        level: "session",
        hostId: s.hostId,
        sessionId: s.sessionId,
        hostName: s.hostName,
        sessionName: s.name,
      },
    });
  }

  // Host drill-down items (server mode only)
  if (!deps.isLocal) {
    for (const host of hosts) {
      actions.push({
        id: `global:host:${host.id}`,
        label: host.hostname,
        icon: Laptop,
        keywords: ["host", "server", "machine", host.hostname],
        group: "navigate",
        onSelect: () => {
          deps.pushContext({
            level: "host",
            hostId: host.id,
            hostName: host.hostname,
          });
        },
        drillDown: { level: "host", hostId: host.id, hostName: host.hostname },
      });
    }
  }

  return actions;
}

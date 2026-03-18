import { BarChart3, Clock, Laptop, Search, Settings } from "lucide-react";
import type { Host } from "../../../lib/api";
import type { ActionDeps, PaletteAction } from "../types";

export function getGlobalActions(
  hosts: Host[],
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
    onSelect: () => {
      deps.navigate("/settings");
      deps.close();
    },
  });

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

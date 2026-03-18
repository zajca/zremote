import type { LucideIcon } from "lucide-react";

export type ContextLevel = "global" | "host" | "project" | "worktree" | "session" | "loop";

export interface PaletteContext {
  level: ContextLevel;
  hostId?: string;
  projectId?: string;
  sessionId?: string;
  loopId?: string;
  // Display names (resolved by the palette when available)
  hostName?: string;
  projectName?: string;
  sessionName?: string;
}

export interface KeyboardShortcut {
  mod?: boolean; // Ctrl on Linux/Win, Cmd on Mac
  shift?: boolean;
  alt?: boolean;
  key: string; // e.g. "1", "n", ","
}

export interface PaletteAction {
  id: string;
  label: string;
  icon: LucideIcon;
  keywords?: string[];
  group: "actions" | "navigate" | "global";
  onSelect: () => void;
  drillDown?: PaletteContext;
  dangerous?: boolean;
  shortcut?: KeyboardShortcut;
}

export interface ActionDeps {
  navigate: (path: string) => void;
  close: () => void;
  pushContext: (ctx: PaletteContext) => void;
  isLocal: boolean;
  openAddProject: (hostId: string) => void;
  openStartClaude: (project: { id: string; name: string; path: string; host_id: string }) => void;
  openHelp: () => void;
}

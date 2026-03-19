import type { LucideIcon } from "lucide-react";
import type { ProjectAction } from "../../lib/api";
import type { PromptTemplate } from "../../types/prompt";

export type ContextLevel = "global" | "host" | "project" | "worktree" | "session" | "loop" | "clipboard";

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
  /** Which ancestor context level generated this action */
  sourceLevel?: ContextLevel;
  /** Display name for the source context (e.g., "MyProject", "my-server") */
  sourceLabel?: string;
  /** Secondary text line (e.g. project + host + path) */
  description?: string;
  /** CSS class for status dot (e.g. "bg-green-400") */
  statusColor?: string;
  /** Show sparkle icon when AI is running */
  showAgenticIndicator?: boolean;
}

export interface ActionDeps {
  navigate: (path: string) => void;
  close: () => void;
  pushContext: (ctx: PaletteContext) => void;
  isLocal: boolean;
  openAddProject: (hostId: string) => void;
  openStartClaude: (project: { id: string; name: string; path: string; host_id: string }) => void;
  openRunPrompt: (template: PromptTemplate, project: { id: string; name: string; path: string; host_id: string }) => void;
  openActionInput: (action: ProjectAction, project: { id: string; host_id: string }) => void;
  openHelp: () => void;
  openWithFilter: (mode: "sessions") => void;
}

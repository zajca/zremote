import type { ContextLevel } from "./types";

const LEVEL_LABELS: Record<ContextLevel, string> = {
  global: "Global",
  host: "Host",
  project: "Project",
  worktree: "Worktree",
  session: "Session",
  loop: "Loop",
};

interface CommandPaletteFooterProps {
  canGoBack: boolean;
  canDrillDown: boolean;
  contextLevel: ContextLevel;
}

export function CommandPaletteFooter({ canGoBack, canDrillDown, contextLevel }: CommandPaletteFooterProps) {
  return (
    <div className="flex items-center justify-between border-t border-border px-3 py-1.5 text-xs text-text-tertiary">
      <div className="flex items-center gap-3">
        <span className="flex items-center gap-1">
          <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px]">
            &uarr;&darr;
          </kbd>
          <span>Navigate</span>
        </span>
        <span className="flex items-center gap-1">
          <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px]">
            &crarr;
          </kbd>
          <span>Select</span>
        </span>
        {canDrillDown && (
          <span className="flex items-center gap-1">
            <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px]">
              Tab
            </kbd>
            <span>Drill down</span>
          </span>
        )}
        {canGoBack && (
          <span className="flex items-center gap-1">
            <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px]">
              &lArr;
            </kbd>
            <span>Back</span>
          </span>
        )}
      </div>
      <div className="flex items-center gap-2">
        <span className="flex items-center gap-1">
          <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px]">
            Shift+Alt+N
          </kbd>
          <span>Quick Claude</span>
        </span>
        <span className="text-text-tertiary">{LEVEL_LABELS[contextLevel]}</span>
        <span className="flex items-center gap-1">
          <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px]">
            Esc
          </kbd>
          <span>Close</span>
        </span>
      </div>
    </div>
  );
}

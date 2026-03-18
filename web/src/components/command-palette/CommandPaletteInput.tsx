import { Search } from "lucide-react";
import { Command } from "cmdk";
import type { PaletteContext } from "./types";

interface CommandPaletteInputProps {
  contextStack: PaletteContext[];
  query: string;
  onQueryChange: (q: string) => void;
  onPopContext: () => void;
  onJumpToIndex: (index: number) => void;
}

function getContextLabel(ctx: PaletteContext, index: number): string {
  if (index === 0 && ctx.level === "global") return "Global";
  if (ctx.hostName) return ctx.hostName;
  if (ctx.projectName) return ctx.projectName;
  if (ctx.sessionName) return ctx.sessionName;
  if (ctx.level === "loop") return "Loop";
  // Fallback
  if (ctx.level === "host") return "Host";
  if (ctx.level === "project") return "Project";
  if (ctx.level === "worktree") return "Worktree";
  if (ctx.level === "session") return "Session";
  return ctx.level;
}

export function CommandPaletteInput({
  contextStack,
  query,
  onQueryChange,
  onPopContext,
  onJumpToIndex,
}: CommandPaletteInputProps) {
  const showBreadcrumbs = contextStack.length > 1;

  return (
    <div className="flex items-center gap-2 border-b border-border px-3">
      <Search size={14} className="shrink-0 text-text-tertiary" />
      {showBreadcrumbs && (
        <div className="flex shrink-0 items-center gap-1">
          {contextStack.map((ctx, i) => (
            <span key={i} className="flex items-center gap-1">
              {i > 0 && (
                <span className="text-text-tertiary text-xs">&gt;</span>
              )}
              <button
                type="button"
                onClick={() => onJumpToIndex(i)}
                className="cursor-pointer rounded bg-bg-tertiary px-1.5 py-0.5 text-xs text-text-secondary transition-colors duration-150 hover:bg-bg-hover"
              >
                {getContextLabel(ctx, i)}
              </button>
            </span>
          ))}
        </div>
      )}
      <Command.Input
        placeholder="Search commands..."
        value={query}
        onValueChange={onQueryChange}
        onKeyDown={(e) => {
          if (
            e.key === "Backspace" &&
            query === "" &&
            contextStack.length > 1
          ) {
            e.preventDefault();
            onPopContext();
          }
        }}
        className="h-10 w-full bg-transparent text-sm text-text-primary placeholder:text-text-tertiary focus:outline-none"
      />
    </div>
  );
}

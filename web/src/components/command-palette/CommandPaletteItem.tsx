import { ChevronRight, Sparkles } from "lucide-react";
import { Command } from "cmdk";
import type { KeyboardShortcut, PaletteAction } from "./types";

const KBD_CLASS =
  "rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px]";

function formatShortcutParts(shortcut: KeyboardShortcut): string[] {
  const isMac =
    typeof navigator !== "undefined" &&
    /Mac|iPod|iPhone|iPad/.test(navigator.platform);
  const parts: string[] = [];
  if (shortcut.mod) parts.push(isMac ? "\u2318" : "Ctrl");
  if (shortcut.shift) parts.push(isMac ? "\u21E7" : "Shift");
  if (shortcut.alt) parts.push(isMac ? "\u2325" : "Alt");
  parts.push(
    shortcut.key.length === 1 ? shortcut.key.toUpperCase() : shortcut.key,
  );
  return parts;
}

function ShortcutBadge({ shortcut }: { shortcut: KeyboardShortcut }) {
  const parts = formatShortcutParts(shortcut);
  return (
    <span className="ml-auto flex items-center gap-0.5">
      {parts.map((part, i) => (
        <kbd key={i} className={KBD_CLASS}>
          {part}
        </kbd>
      ))}
    </span>
  );
}

interface CommandPaletteItemProps {
  action: PaletteAction;
}

export function CommandPaletteItem({ action }: CommandPaletteItemProps) {
  const Icon = action.icon;
  const hasRichLayout = action.description != null || action.statusColor != null;

  return (
    <Command.Item
      value={[action.label, ...(action.keywords ?? [])].join(" ")}
      onSelect={action.onSelect}
      className="flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors duration-75 data-[selected=true]:bg-bg-hover data-[selected=true]:text-text-primary"
    >
      {hasRichLayout ? (
        <>
          {action.statusColor && (
            <span
              className={`shrink-0 rounded-full w-1.5 h-1.5 ${action.statusColor}`}
            />
          )}
          <Icon
            size={14}
            className={action.dangerous ? "text-red-400" : "text-text-tertiary"}
          />
          <div className="flex flex-col min-w-0">
            <span className="flex items-center gap-1">
              <span
                className={
                  action.dangerous ? "text-red-400 truncate" : "text-text-secondary truncate"
                }
              >
                {action.label}
              </span>
              {action.showAgenticIndicator && (
                <Sparkles size={12} className="shrink-0 text-accent" />
              )}
            </span>
            {action.description && (
              <span className="text-xs text-text-tertiary truncate">
                {action.description}
              </span>
            )}
          </div>
          {action.shortcut ? (
            <ShortcutBadge shortcut={action.shortcut} />
          ) : action.drillDown ? (
            <ChevronRight size={12} className="ml-auto text-text-tertiary" />
          ) : null}
        </>
      ) : (
        <>
          <Icon
            size={14}
            className={action.dangerous ? "text-red-400" : "text-text-tertiary"}
          />
          <span
            className={
              action.dangerous ? "text-red-400" : "text-text-secondary"
            }
          >
            {action.label}
          </span>
          {action.shortcut ? (
            <ShortcutBadge shortcut={action.shortcut} />
          ) : action.drillDown ? (
            <ChevronRight size={12} className="ml-auto text-text-tertiary" />
          ) : null}
        </>
      )}
    </Command.Item>
  );
}

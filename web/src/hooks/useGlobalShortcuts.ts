import { useEffect } from "react";
import type { KeyboardShortcut } from "../components/command-palette/types";

export interface ShortcutAction {
  shortcut: KeyboardShortcut;
  onSelect: () => void;
}

function matchesShortcut(
  e: KeyboardEvent,
  shortcut: KeyboardShortcut,
): boolean {
  const modKey = e.metaKey || e.ctrlKey;
  if ((shortcut.mod ?? false) !== modKey) return false;
  if ((shortcut.shift ?? false) !== e.shiftKey) return false;
  if ((shortcut.alt ?? false) !== e.altKey) return false;
  return e.key.toLowerCase() === shortcut.key.toLowerCase();
}

function isEditableTarget(el: EventTarget | null): boolean {
  if (!(el instanceof HTMLElement)) return false;
  const tag = el.tagName;
  if (tag === "INPUT") return true;
  if (tag === "TEXTAREA" && !el.closest(".xterm")) return true;
  if (el.isContentEditable) return true;
  return false;
}

export function useGlobalShortcuts(actions: ShortcutAction[]): void {
  useEffect(() => {
    function handler(e: KeyboardEvent) {
      // Skip if typing in an editable field (unless inside command palette)
      if (isEditableTarget(e.target)) {
        const el = e.target as HTMLElement;
        if (!el.closest("[cmdk-root]")) return;
      }

      for (const action of actions) {
        if (matchesShortcut(e, action.shortcut)) {
          e.preventDefault();
          action.onSelect();
          return;
        }
      }
    }

    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [actions]);
}

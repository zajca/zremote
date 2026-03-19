# RFC-002: Clipboard — Copy-on-Select + Command Palette Integration

## Status: Draft

## Problem Statement

Terminal UI does not support text copying. Users cannot select text in the terminal and have it copied to the system clipboard. There is also no clipboard history or way to paste previously copied text into a terminal session.

## Goals

1. Selected text in the terminal automatically copies to system clipboard + saves to in-app history
2. Clipboard history accessible via command palette (Ctrl+K -> "Clipboard" drilldown) and shortcut Alt+V
3. From command palette, entries can be pasted into active terminal or copied to system clipboard

## Architecture

```
Terminal (xterm.js)
  |-- onSelectionChange (debounced 300ms)
  |     |-- navigator.clipboard.writeText()
  |     |-- ClipboardStore.addEntry()
  |     |-- showToast("Copied to clipboard")
  |
  |-- register with ActiveTerminalStore on mount
  |-- unregister on unmount

Command Palette (Ctrl+K / Alt+V)
  |-- "clipboard" context level
  |-- Lists ClipboardStore entries
  |-- onSelect:
  |     |-- If active terminal: paste via ActiveTerminalStore.sendInput()
  |     |-- Else: navigator.clipboard.writeText()

Sidebar
  |-- Clipboard button -> opens palette at clipboard context

Stores (zustand + localStorage)
  |-- ClipboardStore: entries[], addEntry, removeEntry, clearAll
  |-- ActiveTerminalStore: sessionId, sendInput, register, unregister
```

## Phase 1: Clipboard Store

### CREATE `web/src/stores/clipboard-store.ts`

Zustand store with localStorage persistence (pattern from `session-mru-store.ts`):

```typescript
interface ClipboardEntry {
  id: string;           // crypto.randomUUID()
  text: string;         // max 5000 chars, truncated
  preview: string;      // first 100 chars, newlines -> spaces
  timestamp: number;
  source: {
    sessionId: string;
    sessionName?: string;
  };
}

interface ClipboardState {
  entries: ClipboardEntry[];
  addEntry(text: string, source: { sessionId: string; sessionName?: string }): void;
  removeEntry(id: string): void;
  clearAll(): void;
}
```

**Constraints:**
- Max 30 entries, FIFO eviction (oldest removed when full)
- Deduplication: if the last entry has the same text, update its timestamp instead of creating new
- Persist to localStorage key `"zremote:clipboard-history"`
- Text max 5000 chars — truncate with `"… (truncated)"` marker
- Preview: first 100 chars, newlines replaced with spaces

### CREATE `web/src/stores/clipboard-store.test.ts`

Tests:
1. `addEntry` creates entry with correct fields (id, text, preview, timestamp, source)
2. `addEntry` truncates text longer than 5000 chars
3. `addEntry` generates preview (100 chars, newlines -> spaces)
4. `addEntry` deduplicates — same text as last entry updates timestamp
5. `addEntry` evicts oldest when at 30 entries
6. `removeEntry` removes by id
7. `clearAll` empties entries array
8. localStorage persistence — entries survive store rehydration

## Phase 2: Active Terminal Tracking

### CREATE `web/src/stores/active-terminal-store.ts`

Tiny store for tracking which terminal is active (for paste from clipboard):

```typescript
interface ActiveTerminalState {
  sessionId: string | null;
  sendInput: ((data: string) => void) | null;
  register(sessionId: string, sender: (data: string) => void): void;
  unregister(sessionId: string): void;
}
```

**Behavior:**
- `register` sets sessionId and sendInput callback
- `unregister` clears only if the sessionId matches (prevents race on unmount/remount)

### CREATE `web/src/stores/active-terminal-store.test.ts`

Tests:
1. `register` sets sessionId and sendInput
2. `unregister` clears state when sessionId matches
3. `unregister` does NOT clear state when sessionId does not match (race protection)
4. Initial state has null sessionId and null sendInput

## Phase 3: Terminal Copy-on-Select

### MODIFY `web/src/components/Terminal.tsx`

1. **Add `sessionName?: string` to `TerminalProps`** (line 11)

2. **onSelectionChange handler** — add after `inputDisposable` registration (line 276):
   ```typescript
   let selectionTimer: ReturnType<typeof setTimeout> | null = null;
   const selectionDisposable = term.onSelectionChange(() => {
     if (selectionTimer) clearTimeout(selectionTimer);
     selectionTimer = setTimeout(() => {
       if (!term.hasSelection()) return;
       const text = term.getSelection().trim();
       if (!text || text.length < 2) return;
       // System clipboard
       void navigator.clipboard.writeText(text).catch(() => {});
       // App history
       useClipboardStore.getState().addEntry(text, { sessionId, sessionName });
       showToast("Copied to clipboard", "success");
     }, 300);
   });
   ```

3. **Register with ActiveTerminalStore on mount:**
   ```typescript
   useActiveTerminalStore.getState().register(sessionId, (data) => {
     if (wsRef.current?.readyState === WebSocket.OPEN && !closedRef.current) {
       wsRef.current.send(JSON.stringify({ type: "input", pane_id: paneId, data }));
     }
   });
   ```

4. **Alt+V pass-through** — add to custom key handler (line 244) so the browser/command palette can catch it:
   ```typescript
   if (e.altKey && !e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "v") {
     return false;
   }
   ```

5. **Cleanup** — add to cleanup function:
   - `selectionDisposable.dispose()`
   - Clear `selectionTimer` if pending
   - `useActiveTerminalStore.getState().unregister(sessionId)`

**Important:** All store access via imperative `getState()` — no hook subscriptions = no re-renders.

### MODIFY `web/src/pages/SessionPage.tsx`

Pass `sessionName` to Terminal component (around line 250-253):
```tsx
<Terminal
  sessionId={sessionId}
  sessionName={session.name || session.shell || "shell"}
  onPaneEvent={handlePaneEvent}
/>
```

## Phase 4: Command Palette Integration

### MODIFY `web/src/components/command-palette/types.ts`

Add `"clipboard"` to `ContextLevel` union type:
```typescript
export type ContextLevel = "global" | "host" | "project" | "worktree" | "session" | "loop" | "clipboard";
```

### CREATE `web/src/components/command-palette/actions/clipboard-actions.ts`

```typescript
import { ClipboardCopy } from "lucide-react";
import { useClipboardStore } from "../../../stores/clipboard-store";
import { useActiveTerminalStore } from "../../../stores/active-terminal-store";
import { showToast } from "../../layout/Toast";
import type { ActionDeps, PaletteAction } from "../types";

export function getClipboardActions(deps: ActionDeps): PaletteAction[] {
  const entries = useClipboardStore.getState().entries;
  const activeTerminal = useActiveTerminalStore.getState();

  return entries.map((entry) => ({
    id: `clipboard:${entry.id}`,
    label: entry.preview,
    description: `${entry.source.sessionName ?? "terminal"} · ${formatRelativeTime(entry.timestamp)}`,
    icon: ClipboardCopy,
    keywords: [entry.text.slice(0, 200)],
    group: "actions" as const,
    onSelect: () => {
      if (activeTerminal.sendInput && activeTerminal.sessionId) {
        activeTerminal.sendInput(entry.text);
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
  }));
}
```

**Helper function** (same file or shared utils):
```typescript
function formatRelativeTime(timestamp: number): string {
  const diff = Date.now() - timestamp;
  const seconds = Math.floor(diff / 1000);
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}
```

### MODIFY `web/src/components/command-palette/actions/registry.ts`

- Import `getClipboardActions`
- Add `case "clipboard"` to `resolveActions` switch:
  ```typescript
  case "clipboard": {
    const clipboardActions = getClipboardActions(deps);
    return [...clipboardActions, ...globalActions.map((a) => ({ ...a, group: "global" as const }))];
  }
  ```

### MODIFY `web/src/components/command-palette/actions/global-actions.ts`

Add "Clipboard History" action with drillDown:
```typescript
actions.push({
  id: "global:clipboard",
  label: "Clipboard History",
  icon: ClipboardList,
  keywords: ["clipboard", "copy", "paste", "history"],
  group: "actions",
  shortcut: { alt: true, key: "v" },
  onSelect: () => {
    deps.pushContext({ level: "clipboard" });
  },
  drillDown: { level: "clipboard" },
});
```

Import: `import { ClipboardList } from "lucide-react";`

### MODIFY `web/src/components/command-palette/CommandPaletteFooter.tsx`

Add clipboard label to `LEVEL_LABELS`:
```typescript
const LEVEL_LABELS: Record<ContextLevel, string> = {
  // ... existing entries
  clipboard: "Clipboard",
};
```

### MODIFY `web/src/components/command-palette/CommandPalette.tsx`

1. **Alt+V global shortcut** — add to `globalShortcutActions` (around line 130):
   ```typescript
   sa.push({
     shortcut: { alt: true, key: "v" },
     onSelect: () => {
       const store = useCommandPaletteStore.getState();
       store.setOpen(true);
       store.pushContext({ level: "clipboard" });
     },
   });
   ```

2. **fetchContextData** — add `case "clipboard": break;` (no fetch needed, data is local in zustand).

## Phase 5: Sidebar + Help

### MODIFY `web/src/components/layout/Sidebar.tsx`

Add clipboard button to footer section (between History and Settings, around line 87-90):
```tsx
<button
  onClick={() => {
    const store = useCommandPaletteStore.getState();
    store.setOpen(true);
    store.pushContext({ level: "clipboard" });
  }}
  className="flex h-8 w-full items-center gap-2 rounded-md px-2 text-[13px] text-text-secondary transition-colors duration-150 hover:bg-bg-hover hover:text-text-primary"
  aria-label="Open clipboard history"
>
  <ClipboardList size={16} />
  <span className="flex-1 text-left">Clipboard</span>
  <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px] text-text-tertiary">
    Alt+V
  </kbd>
</button>
```

Import: `import { ClipboardList } from "lucide-react";`

### MODIFY `web/src/components/HelpModal.tsx`

Add to Global Shortcuts section (after line 125):
```tsx
<ShortcutRow
  keys={<Kbd>Alt+V</Kbd>}
  description="Clipboard history"
/>
```

## Files Summary

| File | Action | Phase |
|------|--------|-------|
| `web/src/stores/clipboard-store.ts` | CREATE | 1 |
| `web/src/stores/clipboard-store.test.ts` | CREATE | 1 |
| `web/src/stores/active-terminal-store.ts` | CREATE | 2 |
| `web/src/stores/active-terminal-store.test.ts` | CREATE | 2 |
| `web/src/components/Terminal.tsx` | MODIFY | 3 |
| `web/src/pages/SessionPage.tsx` | MODIFY | 3 |
| `web/src/components/command-palette/types.ts` | MODIFY | 4 |
| `web/src/components/command-palette/actions/clipboard-actions.ts` | CREATE | 4 |
| `web/src/components/command-palette/actions/registry.ts` | MODIFY | 4 |
| `web/src/components/command-palette/actions/global-actions.ts` | MODIFY | 4 |
| `web/src/components/command-palette/CommandPalette.tsx` | MODIFY | 4 |
| `web/src/components/command-palette/CommandPaletteFooter.tsx` | MODIFY | 4 |
| `web/src/components/layout/Sidebar.tsx` | MODIFY | 5 |
| `web/src/components/HelpModal.tsx` | MODIFY | 5 |

## Dependencies Between Phases

```
Phase 1 (clipboard store) ─┐
                            ├─> Phase 3 (Terminal.tsx uses both stores)
Phase 2 (active terminal) ─┘        │
                                     v
                              Phase 4 (command palette uses both stores)
                                     │
                                     v
                              Phase 5 (sidebar + help, independent of 3/4)
```

- Phases 1 and 2 are independent — can be implemented in parallel
- Phase 3 depends on both 1 and 2
- Phase 4 depends on 1 and 2 (reads stores)
- Phase 5 is independent (only UI wiring, no store dependency beyond imports)

## Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|------------|
| `navigator.clipboard.writeText` requires secure context (HTTPS) | Medium | Fails silently with `.catch(() => {})` — app history still works |
| Selection events fire rapidly during drag | Low | 300ms debounce timer clears on each event |
| Large selections (>5000 chars) | Low | Truncation with marker, preview capped at 100 chars |
| ActiveTerminalStore race on quick tab switch | Low | `unregister` checks sessionId match before clearing |
| localStorage quota | Very Low | 30 entries * 5KB max = 150KB, well within limits |

## Verification

1. `cd web && bun run typecheck` — no type errors
2. `cd web && bun run test` — all tests pass including new store tests
3. Manual: open terminal, select text -> toast "Copied to clipboard", text in system clipboard
4. Manual: Alt+V opens command palette at clipboard context, entries visible, search works
5. Manual: Ctrl+K -> type "clip" -> drilldown into Clipboard History
6. Manual: select entry -> pastes into active terminal (or copies to system clipboard if no terminal)
7. Manual: sidebar clipboard button works
8. Manual: Help modal shows Alt+V shortcut

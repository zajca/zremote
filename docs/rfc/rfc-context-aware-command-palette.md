# RFC: Context-Aware Command Palette

## Context

The current command palette (`CommandPalette.tsx`) is a basic flat list with static actions (navigate to Analytics/History/Settings, go to host, add project, start Claude). It's not context-aware -- the same actions appear regardless of where the user is in the app. The user wants a hierarchical, context-aware palette triggered by Double-Shift (like IntelliJ's "Search Everywhere") that shows relevant actions based on current location and allows navigating up/down the entity tree.

## Decisions (Confirmed)

1. **Replace** existing Ctrl+K palette -- one unified context-aware palette for both triggers
2. **Skip host level in local mode** -- global level shows projects/sessions directly (single implicit host)
3. **Full tree drill-down** -- navigate the entire entity tree: global -> host -> project -> session -> loop

## Design

### Trigger
- **Double-Shift**: Press Shift twice within 300ms (keydown, not keyup; ignore `event.repeat`; skip if focus is in input/textarea/xterm)
- **Ctrl+K / Cmd+K**: Preserved as alternative trigger (works even from terminal since it's a modified key)
- Both triggers open the same palette

### Context Detection
Parse `location.pathname` to determine current context level:

| Route | Context Level | Extracted IDs |
|-------|--------------|---------------|
| `/hosts/:hostId/sessions/:sessionId/loops/:loopId` | loop | hostId, sessionId, loopId |
| `/hosts/:hostId/sessions/:sessionId` | session | hostId, sessionId |
| `/hosts/:hostId` | host | hostId |
| `/projects/:projectId` | project/worktree | projectId (check `parent_project_id` for worktree) |
| `/`, `/analytics`, `/history`, `/settings` | global | none |

In **local mode**, global level behaves like host level (single implicit host) -- projects and sessions are shown directly.

### Hierarchical Navigation (Stack-Based)

The palette maintains a **context stack**. When it opens, the stack is initialized from the current route. The user can:

- **Drill down**: Select a host/project/session to enter its context (push onto stack)
- **Go up**: Press **Backspace on empty input** to pop one level up
- **Breadcrumb click**: Click any breadcrumb segment to jump back to that level

Breadcrumb display (inside the input area, before the search input):
```
[Global] > [my-server] > [my-project] > |search here...
```
Each segment is a clickable pill styled with `bg-bg-tertiary text-text-secondary rounded px-1.5 py-0.5 text-xs`.

### Actions per Level

**Global** (always visible at bottom):
- Navigate to: Analytics, History, Settings
- Browse hosts (drill-down, server mode only)
- Search transcripts

**Host level**:
- New terminal session
- Scan for projects
- Add project (opens AddProjectDialog)
- Browse projects (drill-down items)
- Browse sessions (drill-down items)

**Project level**:
- Start Claude task (opens StartClaudeDialog)
- Resume last Claude task
- New terminal session (in project dir)
- Create worktree
- Refresh git info
- Configure with Claude
- Trigger KB indexing
- Project settings
- Delete project
- **Custom project actions** -- fetched via `api.projects.actions(projectId)`, shown as a separate "Actions" group. Each action calls `api.projects.runAction(projectId, actionName)`.
- Browse sessions (drill-down)
- Browse worktrees (drill-down)

**Worktree level** (own level, NOT just "extends project"):
- Go to parent project (navigation up)
- Start Claude task
- Resume last Claude task
- New terminal session (in worktree dir)
- Refresh git info
- Configure with Claude
- Trigger KB indexing
- Worktree settings
- Delete worktree
- **Custom worktree actions** -- fetched via `api.projects.actions(worktreeId)`, worktrees are projects too so they have their own actions from `.zremote` config. Shown as "Actions" group.
- Browse sessions (drill-down)

**Session level**:
- Rename session
- Close session
- Go to host / Go to project (if linked)
- View active loops (drill-down)

**Loop level**:
- Approve / Reject pending actions
- Pause / Resume / Stop
- View transcript
- Go to session / Go to project

### UI Structure

Replace current `CommandPalette.tsx` entirely. New component tree:

```
CommandPalette (orchestrator: backdrop, open/close, dialog spawning)
+-- Command (cmdk root, loop=true)
|   +-- CommandPaletteInput (breadcrumb pills + Command.Input)
|   +-- Command.List
|   |   +-- Command.Empty ("No results found")
|   |   +-- Command.Group "Actions" (context-specific actions)
|   |   +-- Command.Group "Navigate" (drill-down entity items, with chevron)
|   |   +-- Command.Group "Global" (always: analytics, history, settings)
|   +-- CommandPaletteFooter (keyboard hints)
+-- AddProjectDialog (spawned on action)
+-- StartClaudeDialog (spawned on action)
```

### Keyboard Hints (Footer)
- `up/down` Navigate | `Enter` Select | `Esc` Close
- When drilled down: `Backspace` Back (shown only when stack depth > 1)

## Implementation Plan

### Phase 1: Core Infrastructure

**New files:**

1. `web/src/hooks/useDoubleShift.ts` -- Double-Shift detection hook
   - Track last Shift keydown timestamp via `useRef`
   - On Shift keydown: check `!event.repeat`, no other modifiers, not in input/textarea/xterm
   - If within 300ms of last Shift -> call callback, reset
   - Safety: `document.activeElement?.closest('.xterm')`, `instanceof HTMLInputElement`, etc.

2. `web/src/stores/command-palette-store.ts` -- Zustand store
   - `open: boolean`, `setOpen()`, `toggle()`
   - `contextStack: PaletteContext[]`, `pushContext()`, `popContext()`, `resetToRouteContext()`
   - `query: string`, `setQuery()`
   - Follow pattern from `agentic-store.ts`

3. `web/src/hooks/useCommandPaletteContext.ts` -- Route -> context detection
   - Uses `useLocation()` to parse pathname with regex
   - Returns `PaletteContext` with level + extracted entity IDs
   - For project/worktree distinction: accept optional project data param or resolve later

### Phase 2: Action Definitions

**New files:**

4. `web/src/components/command-palette/types.ts` -- Shared types
   - `ContextLevel = "global" | "host" | "project" | "worktree" | "session" | "loop"`
   - `PaletteContext { level, hostId?, projectId?, sessionId?, loopId?, hostName?, projectName?, sessionName?, isWorktree?, parentProjectId? }`
   - `PaletteAction { id, label, icon, keywords?, group, onSelect, drillDown?, dangerous? }`

5. `web/src/components/command-palette/actions/global-actions.ts`
   - Navigate to Analytics, History, Settings
   - Host drill-down items (server mode)

6. `web/src/components/command-palette/actions/host-actions.ts`
   - New session, scan projects, add project
   - Project/session drill-down items

7. `web/src/components/command-palette/actions/project-actions.ts`
   - Start Claude, resume Claude, new session, create worktree
   - Refresh git, configure, KB indexing, settings, delete
   - Custom project actions from `api.projects.actions(projectId)` -- fetched when context enters project level
   - Session/worktree drill-down items

8. `web/src/components/command-palette/actions/worktree-actions.ts`
   - Go to parent project
   - Start Claude, resume Claude, new session (in worktree dir)
   - Refresh git, configure, KB indexing, settings, delete worktree
   - Custom worktree actions from `api.projects.actions(worktreeId)` -- worktrees are projects, so they have their own `.zremote` actions
   - Session drill-down items
   - NOTE: No "create worktree" (worktrees don't nest)

9. `web/src/components/command-palette/actions/session-actions.ts`
   - Rename, close, navigate to host/project, loop drill-down

10. `web/src/components/command-palette/actions/loop-actions.ts`
    - Approve/reject, pause/resume/stop, transcript, navigate to session/project

11. `web/src/components/command-palette/actions/registry.ts`
    - `resolveActions(context, deps)` -> merges level-appropriate actions
    - For project: project actions + custom project actions
    - For worktree: worktree actions + custom worktree actions (separate from parent project)
    - Always includes global actions at the bottom

### Phase 3: UI Components

**New files:**

12. `web/src/components/command-palette/CommandPaletteItem.tsx`
    - Wraps `Command.Item` with icon, label, optional shortcut badge, chevron for drill-down, red text for dangerous
    - Reuses existing styling patterns from current `CommandItem`

13. `web/src/components/command-palette/CommandPaletteInput.tsx`
    - Breadcrumb pills rendered inline before `Command.Input`
    - `onKeyDown` handler: Backspace on empty -> `popContext()`
    - Each pill clickable to jump to that stack level
    - Search icon prefix (current pattern)

14. `web/src/components/command-palette/CommandPaletteFooter.tsx`
    - Keyboard hints: arrows, Enter, Esc, Backspace (conditional on stack depth)
    - Current context level indicator

15. `web/src/components/command-palette/CommandPalette.tsx` (NEW, replaces `layout/CommandPalette.tsx`)
    - Orchestrator: backdrop, `<Command>` wrapper, open/close state from store
    - Listens for Ctrl+K and Double-Shift (via `useDoubleShift`)
    - On open: `resetToRouteContext()` from `useCommandPaletteContext()`
    - Renders action groups from `resolveActions()`
    - Fetches custom actions via `api.projects.actions()` when at project/worktree level
    - Manages dialog spawning (AddProjectDialog, StartClaudeDialog)
    - Clears query on context change (push/pop)

### Phase 4: Integration

16. **Modify** `web/src/components/layout/AppLayout.tsx`
    - Update import path from `./CommandPalette` to `../command-palette/CommandPalette`

17. **Delete** `web/src/components/layout/CommandPalette.tsx` (old implementation)

### Phase 5: Tests

18. `web/src/hooks/useDoubleShift.test.ts`
    - Double-shift detection within timeout
    - No trigger beyond timeout
    - No trigger on held key (repeat)
    - No trigger with modifier keys
    - No trigger when focused in input/textarea
    - No trigger when focused in xterm

19. `web/src/hooks/useCommandPaletteContext.test.ts`
    - Route parsing for all route patterns
    - Global fallback for unknown routes

20. `web/src/stores/command-palette-store.test.ts`
    - Open/close/toggle
    - Push/pop context stack
    - Reset to route context
    - Query management

21. `web/src/components/command-palette/CommandPalette.test.tsx`
    - Opens on Ctrl+K and Double-Shift
    - Shows context-appropriate actions based on route
    - Drill-down navigation (click entity -> new actions)
    - Backspace on empty -> go up one level
    - Breadcrumb display and click
    - Closes on Escape and backdrop click
    - Dialog spawning preserved (AddProject, StartClaude)
    - Custom project/worktree actions loaded and displayed
    - Mode awareness (local vs server)
    - Search filtering works across action labels

22. **Delete** `web/src/components/layout/CommandPalette.test.tsx` (old tests)

## Critical Files

| File | Action | Purpose |
|------|--------|---------|
| `web/src/components/layout/CommandPalette.tsx` | DELETE | Old flat palette |
| `web/src/components/layout/CommandPalette.test.tsx` | DELETE | Old tests |
| `web/src/components/layout/AppLayout.tsx:4,137` | MODIFY | Update import path |
| `web/src/components/command-palette/` | CREATE (dir) | New palette module |
| `web/src/hooks/useDoubleShift.ts` | CREATE | Double-Shift hook |
| `web/src/hooks/useCommandPaletteContext.ts` | CREATE | Route -> context |
| `web/src/stores/command-palette-store.ts` | CREATE | Palette state |
| `web/src/lib/api.ts` | READ ONLY | API operations for actions |
| `web/src/components/AddProjectDialog.tsx` | READ ONLY | Spawned by palette |
| `web/src/components/StartClaudeDialog.tsx` | READ ONLY | Spawned by palette |

## Reuse

- `cmdk` library -- same as current, just with conditional rendering per context
- `AddProjectDialog`, `StartClaudeDialog` -- reuse existing dialogs as-is
- `api.*` namespace -- all mutation actions call existing API client
- Styling patterns -- same `data-[selected=true]`, `[cmdk-group-heading]` selectors
- Keyboard safety pattern -- same `closest('.xterm')`, `instanceof HTMLInputElement` checks from existing components
- `useHosts()`, `useProjects()`, `useMode()` hooks for entity data
- `showToast()` for action feedback
- `useAgenticStore`, `useClaudeTaskStore` for runtime loop/task data

## Verification

1. `cd web && bun run typecheck` -- no type errors
2. `cd web && bun run test` -- all tests pass (old + new)
3. `cd web && bun run dev` -- manual testing:
   - Double-Shift opens palette from any page
   - Ctrl+K still works
   - On `/` (global): shows navigation + host list
   - On `/hosts/:id`: shows host actions + projects/sessions
   - On `/projects/:id`: shows project actions + custom actions + worktrees/sessions
   - On worktree page: shows worktree actions + custom worktree actions
   - On session/loop pages: shows relevant actions
   - Backspace on empty input navigates up
   - Breadcrumb pills clickable
   - Drill-down into entities works
   - AddProject and StartClaude dialogs open correctly
   - No interference with terminal typing
   - Local mode shows projects directly at global level

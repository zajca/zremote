# Command Palette for GPUI Desktop Client

## Inspiration: IntelliJ Search Everywhere + Raycast

This idea targets the **native GPUI client**. The user comes from IntelliJ and wants to feel at home -- tabbed search with dedicated shortcuts per category, fast fuzzy navigation, Raycast-like item rendering.

### IntelliJ Search Everywhere

- Double-Shift opens unified search with **tabs**: All | Classes | Files | Symbols | Actions
- Dedicated shortcuts jump directly to a specific tab (Ctrl+N -> Classes, Ctrl+Shift+N -> Files, Ctrl+Shift+A -> Actions)
- Tab/Shift+Tab to switch between tabs while palette is open
- Recent items shown first when query is empty
- Fuzzy matching with score-based ranking

### Raycast

- Clean, fast, single input with instant filtering
- Rich item rendering: icon + title + subtitle + accessory (badge/shortcut hint)
- Category grouping in a flat scrollable list
- Smooth, polished feel -- every frame matters

---

## Triggers -- IntelliJ-style Dedicated Shortcuts

Ctrl+Shift combos are safe -- terminal encoder explicitly checks `!modifiers.shift` so these fall through. Ctrl+K conflicts with terminal readline (kill-to-end) -- must be intercepted in terminal's `on_key_down` BEFORE the encoder, same pattern as existing Ctrl+F interception for search.

| Shortcut | Opens tab | IntelliJ analogy | Mnemonic |
|---|---|---|---|
| **Double-Shift** | All | Search Everywhere | Universal search |
| **Ctrl+K** | All | -- | VS Code/Raycast standard |
| **Ctrl+Shift+E** | Sessions | Ctrl+E (Recent Files) | E = Execution environments |
| **Ctrl+Shift+P** | Projects | Ctrl+Shift+N (Go to File) | P = Projects |
| **Ctrl+Shift+A** | Actions | Ctrl+Shift+A (Find Action) | A = Actions (identical!) |

### Shortcut Interception Strategy

1. **Ctrl+K and Ctrl+Shift+E/P/A**: Intercepted in `terminal_panel.rs` `on_key_down` handler BEFORE the byte encoder (same block as existing Ctrl+F). Terminal emits event to MainView (e.g., `TerminalEvent::OpenCommandPalette { tab }`).
2. **Double-Shift**: Detected via `cx.observe_keystrokes()` on MainView (window-global observation, works regardless of focus). Not interceptable at terminal level since bare Shift is not a command.
3. **Empty state (no terminal active)**: MainView needs its own `FocusHandle` to capture shortcuts when no terminal exists. Without this, Ctrl+K does nothing on first launch.
4. **Search overlay open**: Close search overlay first, then open palette. Only one overlay at a time.

### Double-Shift Detection Algorithm

```
on Shift key_down:
  if event.repeat: ignore (held key auto-repeat)
  if any other modifier held (Ctrl, Alt, Meta): ignore
  record timestamp

on Shift key_up:
  if (now - last_shift_down) > 400ms: reset (this was a "held" Shift, not a tap)
  if second_tap AND (now - first_tap_up) < 300ms: TRIGGER PALETTE
  else: record as first_tap_up

on any non-Shift key_down:
  reset all Double-Shift state
```

**Edge cases handled:**
- Capital letters typed fast (Shift+A, Shift+B): non-Shift keydown between taps resets state -- no false trigger
- Left-Shift then Right-Shift: both produce `key == "shift"` in GPUI -- triggers palette (matches IntelliJ)
- CapsLock: separate key, does not produce Shift events -- no interaction
- Shift+Tab while palette open: Tab keydown resets state -- no false trigger
- Shift held >400ms: treated as "held, not tap" -- no false trigger

---

## UI Layout -- Raycast-style with IntelliJ Tabs

```
+------------------------------------------------------------------+
|  Sidebar  |                                                       |
|  (250px)  |     +---------- Command Palette -----------+          |
|           |     | [All] [Sessions] [Projects] [Actions] |          |
|           |     | [>] Search...                  Ctrl+K |          |
|           |     |                                       |          |
|           |     |  RECENT                               |          |
|           |     |  > myproject (bash)    active   0:45  |          |
|           |     |  > api-server (zsh)    active   2:12  |          |
|           |     |                                       |          |
|           |     |  SESSIONS                             |          |
|           |     |  > frontend (bash)     active   0:02  |          |
|           |     |  > database (bash)     suspended      |          |
|           |     |                                       |          |
|           |     |  PROJECTS                             |          |
|           |     |  > zremote     main  *dirty            |          |
|           |     |  > my-api      feat/auth               |          |
|           |     |                                       |          |
|           |     |  ACTIONS                               |          |
|           |     |  + New session          Ctrl+Shift+N  |          |
|           |     |  / Search terminal          Ctrl+F   |          |
|           |     |                                       |          |
|           |     |  [Up/Down] Navigate  [Enter] Select   |          |
|           |     |  [Tab] Next tab  [Esc] Close          |          |
|           |     +---------------------------------------+          |
|           |                                                       |
+------------------------------------------------------------------+
```

### Dimensions

- Width: `min(520px, available_width - 80px)`, min 360px
- Max height: `min(420px, available_height * 0.6)`
- Position: horizontally centered over the **full window** (not just terminal area -- centering over terminal area looks off-center when sidebar is visible), vertically ~20% from top
- Corner radius: 8px
- Background: `bg_secondary()` (#16161a) with `border()` 1px border
- Shadow: `0 8px 32px rgba(0,0,0,0.5), 0 2px 8px rgba(0,0,0,0.3)` (or single `0 8px 24px rgba(0,0,0,0.5)` if GPUI supports only one)
- Backdrop: full-window overlay, `bg_primary()` (#111113) at 40% opacity, click to dismiss. No blur.
- Internal padding: 0px (sections handle their own)

### Responsive Behavior

| Window width | Palette width | Adaptations |
|---|---|---|
| < 440px | Palette does not open | Too narrow for usable palette |
| 440-600px | `available - 80px` | Tab labels abbreviate (Sess, Proj, Act). Footer shows only `[Enter] Select [Esc] Close`. Input shortcut hint hidden. |
| 600-1920px | 520px (capped) | Full layout |
| > 1920px | 520px (never grows) | Wider palette would feel empty |

### Tab Bar

- **Height**: 36px
- **Horizontal padding**: 12px
- **Tab pill spacing**: 4px gap
- **Individual tab pill**: 6px horizontal padding, 4px vertical, 4px border-radius
  - **Font**: 12px, weight MEDIUM (500)
  - **Active**: `bg_tertiary()` background, `text_primary()` text, 2px bottom border `accent()`
  - **Inactive**: transparent background, `text_secondary()` text
  - **Hover (inactive)**: `bg_tertiary()` background, `text_primary()` text
- **Tab count badge**: inline text after label in `text_tertiary()`, e.g. "Sessions (4)". Only on non-All tabs. Updated live as user types.
- **Bottom border**: 1px solid `border()`

### Input Bar

- **Height**: 40px
- **Horizontal padding**: 12px
- **Search icon**: `Icon::Search`, 14px, `text_tertiary()`, 8px right margin
- **Input field**: `bg_primary()` background, 1px `border()` border, 4px radius, 8px horizontal padding, 13px font
  - Text: `text_primary()` when has text, `text_tertiary()` for placeholder
  - Placeholder per tab: "Search everything...", "Search sessions...", "Search projects...", "Search actions..."
  - Supports Ctrl+V paste from clipboard
  - No cursor movement (Left/Right) -- simple string with `.pop()` for backspace (matches SearchOverlay pattern)
  - No IME support in v1 (same limitation as SearchOverlay)
- **Shortcut hint**: right-aligned, 11px, `text_tertiary()`, no pill background
- **Bottom border**: 1px solid `border()`

### Results Area

- **Max height**: palette height minus tab bar (36px) minus input (40px) minus footer (28px) = `min(316px, remaining)`
- **Overflow**: `overflow_y_scroll()`
- **Vertical padding**: 4px top/bottom
- Scrollbar: GPUI default (thin overlay, OS-managed)
- **On filter change**: scroll resets to top, selection resets to first item
- **On tab switch**: scroll resets to top, selection resets to first item
- **Wrap-around**: Down on last item wraps to first, Up on first wraps to last

### Footer

- **Height**: 28px
- **Horizontal padding**: 12px
- **Top border**: 1px solid `border()`
- **Content**: `[Up/Down] Navigate  [Enter] Select  [Tab] Next tab  [Esc] Close`
- **Font**: 11px, `text_tertiary()`. Keys as small pills (`bg_primary()` background, 1px `border()`, 3px radius, 4px h-padding, 1px v-padding)
- **Contextual updates**: show `[Backspace] Back` in host-picker sub-flow

---

## Tabs and Content

### Data Model: Snapshot

When the palette opens, it captures a snapshot of sidebar data (hosts, sessions, projects). **No API calls**, no loading spinners, no data refresh while open. This guarantees instant open and stable list during navigation.

- Data is typically <100ms stale (sidebar refreshes via WebSocket events in real-time)
- If sidebar hasn't loaded yet (app just launched, < 500ms), palette opens with whatever is available -- Actions tab always has static commands
- ServerEvents arriving while palette is open are ignored by the palette; sidebar updates in background

### All Tab (default for Double-Shift and Ctrl+K)

**Empty query** -- groups in fixed order, empty groups silently omitted (no header, no placeholder):

1. **RECENT** -- Last 10 accessed sessions, timestamp descending. Only sessions that still exist and are active/suspended. Stale entries silently pruned.
2. **SESSIONS** -- Active sessions not in RECENT, then suspended. Alphabetical within each status group. NOT sub-grouped by host (host in subtitle).
3. **PROJECTS** -- Pinned first (alphabetical), then unpinned (alphabetical).
4. **ACTIONS** -- Context-aware static commands (see Actions section).

**With query** -- flat list, no group headers. Pure fuzzy-score ordering. Items from all categories interleaved by score. Tiebreaker: recent > sessions > projects > actions. Within same category and score: recency > alphabetical.

### Sessions Tab (Ctrl+Shift+E)

**Empty query** groups: RECENT SESSIONS, ACTIVE, SUSPENDED. Empty groups omitted.

**With query**: flat list by fuzzy score. Tiebreaker: recency > alphabetical.

Each item:
- Icon: `SquareTerminal`
- Title: `session.name` or `"Session {id[..8]}"`. Shell hint `"({shell})"` appended only if name is set.
- Subtitle:
  - Server mode: `"{hostname} / {project_name}"` or `"{hostname} / {last_2_path_segments}"` or `"{hostname}"`
  - Local mode: `"{project_name}"` or `"{last_2_path_segments}"` or no subtitle
- Right accessory: status dot (green active, yellow suspended, gray creating) + duration
- Duration format: `"M:SS"` (< 1h), `"Hh Mm"` (< 24h), `"Nd Hh"` (>= 24h)
- Action: switch to session

### Projects Tab (Ctrl+Shift+P)

**Empty query** groups: PINNED, ALL PROJECTS. If no pinned, omit PINNED header -- show all under ALL PROJECTS.

**With query**: flat list by score. Pinned/unpinned distinction disappears. Tiebreaker: pinned > alphabetical.

Each item:
- Icon: `FolderGit`
- Title: project name
- Subtitle: compact path (`~` for home dir, truncate middle if >50 chars: `"~/Code/.../deep/project"`)
  - Server mode: `"{hostname} · {path}"`
  - Local mode: `"{path}"`
- Right accessory: git branch pill (with `GitBranch` icon, 10px, `bg_tertiary()` background, 3px radius, max 120px truncate) + dirty indicator (yellow dot)
- Action: create new session in project directory

### Actions Tab (Ctrl+Shift+A)

Single flat list, no subgroups. Context-aware -- hidden actions are not shown (no grayed-out items).

| Action | Icon | Shortcut hint | Condition | Priority when active |
|---|---|---|---|---|
| Close Current Session | `X` | -- | Has active session | 1 |
| Search in Terminal | `Search` | `Ctrl+F` | Has active session | 2 |
| New Session | `Plus` | -- | Always | 3 |
| Pin/Unpin Project | `Pin`/`PinOff` | -- | Has projects | 4 |
| Reconnect | `Wifi` | -- | Always | 5 |

When no terminal active: New Session first, then Pin/Unpin, Reconnect.

**"New Session" in server mode with multiple hosts**: palette transitions to host picker sub-view. Title changes to "Select host for new session", list shows only online hosts. Backspace on empty query returns to main view (not close). In local mode or single host: creates immediately.

---

## Navigation -- IntelliJ-style

### Within a Tab

- **Up/Down** arrows: move selection (wraps top<->bottom)
- **Ctrl+J/Ctrl+K**: vim-style up/down (alternative)
- **Enter**: execute selected item, close palette instantly (no animation, no flash)
- **Escape**: close palette, focus returns to terminal

### Between Tabs

- **Tab**: next tab (wraps: All -> Sessions -> Projects -> Actions -> All)
- **Shift+Tab**: previous tab
- Selection resets to first item when switching tabs
- Scroll resets to top when switching tabs
- Query persists across tab switches (same query, different filter scope)

### Selection Model

- **Keyboard selection**: sticky, moved by Up/Down. Rendered with `bg_tertiary()` + 2px `accent()` left border. This is what Enter activates.
- **Mouse hover**: independent from keyboard selection. Shows lighter `bg_tertiary()` background, no left border. Does NOT change keyboard selection.
- **Mouse click**: activates immediately (same as Enter on that item).
- **When filtered list changes**: if selected item ID is still in new list, keep it selected. Otherwise, select index 0.

### Dismissal

| Trigger | Behavior |
|---|---|
| **Escape** | Close, focus -> terminal |
| **Click backdrop** | Close, focus -> terminal (via `on_blur` or backdrop `on_click`) |
| **Backspace on empty query** | Close (Raycast behavior) |
| **Enter on item** | Execute action, then close |
| **Ctrl+K while open** | Toggle: close palette |
| **Double-Shift while open** | Toggle: close palette |
| **Ctrl+Shift+X while on tab X** | Close (same tab shortcut = toggle) |
| **Ctrl+Shift+X while on different tab** | Switch to tab X (don't close) |
| **Window loses focus** | Keep palette open (user may be checking another window) |
| **Click sidebar item** | Close (sidebar click moves focus, palette detects blur) |

### Special

- Query clears when palette reopens (fresh start each time)
- All unrecognized Ctrl+letter combos consumed by palette -- never leak to terminal

---

## Fuzzy Search

Custom Rust implementation (~60 lines), no external crate needed for this dataset size.

### Search Target

Fuzzy matcher runs against concatenated `"{title} {subtitle}"`. Match indices split across both strings for highlighting.

- **Title matches**: full score
- **Subtitle matches**: 0.5x multiplier on position-based scores (subtitle should not outrank title matches)

### Scoring

| Factor | Score | Description |
|---|---|---|
| Exact prefix | +100 | Query is a prefix of target |
| Consecutive match | +10/char | Bonus for adjacent matched characters |
| Word boundary start | +15 | Match starts at `_`, `-`, `/`, camelCase transition |
| CamelCase transition | +8 | Match at uppercase after lowercase |
| Position penalty | -1/pos | Earlier matches score higher |
| Gap penalty | -3/gap | Penalty for gaps between matched chars |
| Case-exact bonus | +5 | Exact case match |

### Secondary Sort

Within same score band: recency -> frequency -> alphabetical.

### Highlight Rendering

- **Title**: matched chars in `accent()` + `FontWeight::SEMIBOLD`, rest in `text_primary()` + normal weight
- **Subtitle**: matched chars in `accent()` + normal weight (preserve visual hierarchy -- subtitle never outshouts title)
- **1-character queries**: score-based ordering only, no highlighting (too noisy)
- **2+ character queries**: full highlighting

### Output

```rust
pub struct FuzzyMatch {
    pub score: i32,
    pub matched_indices: Vec<usize>, // for highlight rendering
}

pub fn fuzzy_match(query: &str, target: &str) -> Option<FuzzyMatch>;
```

### Performance

~100 items, sub-microsecond per keystroke. No debouncing needed.

---

## Item Rendering -- Raycast-style

### Item Row (32px height)

```
┌──────────────────────────────────────────────────────────────┐
│ 12px [icon 16x16] 8px [title/subtitle stack]  ... [access.] 12px │
│                        ┌─ Title: [highlighted text]              │
│                        └─ Subtitle: [muted text, 1px top margin] │
│                                                   [dot 6px]      │
│                                                   [badge text]   │
│                                                   [shortcut pill]│
└──────────────────────────────────────────────────────────────┘
```

- **Left accent border** (selected only): 2px `accent()`, inside 12px left padding (no content shift)
- **Icon**: 16x16px, flex-shrink-0, `text_secondary()`
- **Title/subtitle stack**: flex-col, flex-1, min-w-0, overflow-hidden
  - Title: single line, truncate with ellipsis, 13px, `text_primary()`
  - Subtitle: single line, truncate, 11px, `text_secondary()`, 1px top margin
- **Accessories**: flex row, items-center, gap 6px, flex-shrink-0
  - Status dot: 6x6px, rounded-full
  - Duration/branch text: 11px, `text_secondary()`
  - Shortcut pill: `bg_primary()` background, 1px `border()`, 3px radius, 4px h-padding, 1px v-padding, 11px `text_tertiary()`
- **Hover**: full-width `bg_tertiary()` background, 4px border-radius
- **Cursor**: pointer on hover

### Category Headers (20px height, not selectable)

- Top margin: 8px (first category: 4px)
- Bottom margin: 2px
- Left padding: 12px
- Text: UPPERCASE, 11px, `text_tertiary()`, SEMIBOLD

### Typography Hierarchy

| Element | Size | Weight | Color |
|---------|------|--------|-------|
| Tab labels | 12px | MEDIUM (500) | Active: `text_primary()`, Inactive: `text_secondary()` |
| Search input | 13px | NORMAL (400) | `text_primary()` / `text_tertiary()` (placeholder) |
| Category headers | 11px | SEMIBOLD (600) | `text_tertiary()`, UPPERCASE |
| Item title | 13px | NORMAL (400) | `text_primary()` |
| Title fuzzy match | 13px | SEMIBOLD (600) | `accent()` |
| Item subtitle | 11px | NORMAL (400) | `text_secondary()` |
| Accessories | 11px | NORMAL (400) | `text_secondary()` / `text_tertiary()` |
| Footer hints | 11px | NORMAL (400) | `text_tertiary()` |

---

## Empty States

### No data (first launch)

| Tab | Icon | Primary text | Secondary text |
|-----|------|-------------|----------------|
| All | `SquareTerminal` | "No sessions or projects yet" | "Create a new session to get started" |
| Sessions | `SquareTerminal` | "No sessions yet" | "Create a new session from the sidebar or Actions tab" |
| Projects | `FolderGit` | "No projects discovered" | "Projects appear when the agent scans your host" |
| Actions | *(never empty)* | -- | -- |

Layout: icon 24px `text_tertiary()` centered, primary 13px `text_secondary()`, secondary 11px `text_tertiary()`. All centered vertically and horizontally in results area.

### No search results

- Primary: `"No results for "{query}""` (13px, `text_secondary()`)
- Secondary: `"Try a different search term"` (11px, `text_tertiary()`)
- Query truncated at 30 chars with ellipsis in display

### Empty group

Silently omitted -- no header rendered. Other groups fill the space.

---

## Recent Items

### What Counts as Access

Recorded on:
- Session switch via sidebar click (`SessionSelected` event)
- Session switch via command palette selection
- New session creation (immediately counts)

NOT recorded on:
- Auto-restore on app launch
- Background `ServerEvent` updates

### Storage

```rust
pub struct RecentSession {
    pub session_id: String,
    pub timestamp: i64,  // unix seconds
}
```

In `GuiState` (persistence.rs): `recent_sessions: Vec<RecentSession>`, max 10.

### Update Algorithm

1. Remove existing entry with same `session_id` (dedup)
2. Push new entry to front with current timestamp
3. Truncate to 10
4. Persist via `Persistence::update()`

### Stale Entry Handling

When building palette snapshot: filter against current session list, only include active/suspended sessions. Don't modify persisted list (closed session may reappear after agent reconnects). Clean up persisted list on app shutdown.

### Projects -- No Recency Tracking in MVP

Project access patterns differ from sessions -- you create a session in a project and work in that session. Pinned + alphabetical ordering is sufficient.

---

## Server vs Local Mode

| Aspect | Server mode | Local mode |
|---|---|---|
| Session subtitle | `"{hostname} / {project}"` | `"{project}"` or `"{last_2_path_segments}"` |
| Project subtitle | `"{hostname} · {path}"` | `"{path}"` |
| "New Session" action | Host picker sub-flow (if multiple hosts) | Creates directly |
| Host grouping | None (flat list, host in subtitle) | N/A (single host) |

---

## Multi-Host Scaling (Server Mode)

**Strategy: flat scored list, not grouped by host.** Users remember session/project names, not which host they're on. Fuzzy search + subtitle disambiguation handles 50+ items well.

- Host appears in subtitle for disambiguation (searchable)
- Active sessions surface first in ordering
- Typing 2-3 characters narrows from 50+ to 3-5 items
- ~12 items visible without scrolling (at 32px/row in 316px results area)

---

## Focus Flow

### State transitions

```
Terminal focused → shortcut pressed → Terminal emits event → MainView creates palette
  → CommandPalette.focus_handle.focus(window) → palette has focus

Palette focused → dismiss trigger → palette emits Close → MainView drops entity
  → Terminal.render() sees !focused → auto-focuses → terminal has focus

No terminal (empty state) → MainView.focus_handle captures shortcut → creates palette
  → same flow as above
```

### Key Requirements

1. **MainView needs its own FocusHandle** for empty state (no terminal active)
2. **Terminal selection preserved** across palette open/close (no clear on open)
3. **Focus loss detection**: palette uses `cx.on_blur()` to auto-close when backdrop clicked or sidebar item clicked
4. **Search overlay conflict**: if search is open when palette shortcut pressed, close search first, then open palette

---

## Animation

- **Open**: instant (no fade/scale). GPUI has limited animation support. The visual weight of backdrop + palette border provides sufficient "entrance."
- **Close**: instant (entity dropped in one frame)
- **Tab switch**: content replaces instantly (Raycast pattern -- instant feels faster)
- **Selection movement**: instant background change (no slide/fade)
- **Item filtering**: instant appear/disappear during typing
- **Scroll-into-view**: instant jump (no smooth scroll)

---

## Action Execution

- **Instant close**: palette closes in same frame as action dispatch. No animation, no flash, no delay.
- **Async actions** (create session, close session): palette closes immediately, operation runs in background. Sidebar shows loading state via existing Loader icon.
- **Failed actions**: error logged via `tracing::error!`. No in-palette error feedback (palette is already closed). Future: toast/notification system at MainView level.

---

## Architecture (GPUI)

### Entity Pattern (following SearchOverlay)

- `CommandPalette` struct with `FocusHandle`, query, selected_index, active_tab, data snapshot
- `Render` + `Focusable` + `EventEmitter<CommandPaletteEvent>` impls
- Character-by-character input via `on_key_down` (GPUI has no native text input)

### Events Emitted to MainView

```rust
pub enum CommandPaletteEvent {
    SelectSession { session_id: String, host_id: String },
    CreateSessionInProject { host_id: String, working_dir: String },
    CreateSession { host_id: String },
    CloseSession { session_id: String },
    OpenSearch,
    ToggleProjectPin { project_id: String, pinned: bool },
    Close,
}
```

### Data Flow

1. Shortcut pressed -> terminal emits event / MainView detects Double-Shift
2. MainView creates `Entity<CommandPalette>` with sidebar data snapshot + initial tab
3. User types/navigates -> palette state updates internally, `cx.notify()` on every change
4. User selects item -> palette emits event -> MainView handles -> palette dropped
5. Escape/backdrop -> palette emits Close -> MainView drops entity -> terminal auto-refocuses

### Focus

Palette takes focus via FocusHandle on render. When dropped, terminal auto-focuses in its next render cycle (existing behavior). Zero coupling needed.

---

## Edge Cases

| Scenario | Behavior |
|---|---|
| Very long session names | Truncate with ellipsis (`.truncate()`), fuzzy match on full string |
| Unicode in paths | Full Unicode support, path compaction splits on `/` (safe) |
| 50+ sessions | Scrollable list, fuzzy search narrows to 3-5 items in 2 keystrokes |
| Same project name on different hosts | Subtitle disambiguates (`{hostname} · {path}`) |
| Session closed while palette open | Palette uses snapshot; stale selection handled gracefully by terminal panel |
| Palette opened during app cold start (< 500ms) | Opens with whatever data is available; Actions tab always works |
| Rapid open/close/open | Instant, no animation, fresh state each time |
| Window resize while open | Palette re-layouts via GPUI, selection preserved |
| Ctrl+K in vim/htop | Always intercepted for palette (escape hatch to switch sessions is more important) |

---

## User Flow Examples

| Goal | Fastest path | Keystrokes |
|------|-------------|------------|
| Switch to known session | `Ctrl+K` -> type 2-3 chars -> `Enter` | 4-5 |
| Switch to recent session | `Ctrl+Shift+E` -> `Enter` (or Down+Enter) | 3-4 |
| New session in project | `Ctrl+Shift+P` -> type 2-3 chars -> `Enter` | 5-7 |
| Close current session | `Ctrl+Shift+A` -> type "clo" -> `Enter` | 6 |
| Search in terminal | `Ctrl+F` (direct, bypasses palette) | 1 |

---

## Files to Create/Modify

| File | Action | Description |
|---|---|---|
| `crates/zremote-gui/src/views/command_palette.rs` | **Create** | ~600-700 lines, main palette implementation |
| `crates/zremote-gui/assets/icons/command.svg` | **Create** | Command icon from Lucide |
| `crates/zremote-gui/assets/icons/folder.svg` | **Create** | Folder icon from Lucide |
| `crates/zremote-gui/assets/icons/zap.svg` | **Create** | Zap icon from Lucide |
| `crates/zremote-gui/src/views/main_view.rs` | **Modify** | FocusHandle for empty state, shortcut handlers (Ctrl+K, Double-Shift via `observe_keystrokes`, Ctrl+Shift+E/P/A), overlay rendering, event routing |
| `crates/zremote-gui/src/views/terminal_panel.rs` | **Modify** | Intercept Ctrl+K/Ctrl+Shift+E/P/A before encoder (same block as Ctrl+F), emit event to MainView |
| `crates/zremote-gui/src/views/sidebar.rs` | **Modify** | Public accessor methods (`hosts()`, `sessions()`, `projects()`) |
| `crates/zremote-gui/src/icons.rs` | **Modify** | New Icon variants (Command, Folder, Zap, Search) |
| `crates/zremote-gui/src/persistence.rs` | **Modify** | `recent_sessions: Vec<RecentSession>` field in GuiState |
| `crates/zremote-gui/src/views/mod.rs` | **Modify** | Add `pub mod command_palette` |

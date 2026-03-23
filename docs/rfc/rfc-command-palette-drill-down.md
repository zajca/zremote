# RFC: Command Palette Hierarchical Drill-Down Navigation

## Status: Draft
## Author: UX Design Team (3 perspectives: Raycast/macOS, IntelliJ/VSCode, Terminal Power-User)

---

## 1. Problem Statement

The command palette currently supports only flat navigation: Up/Down to select items, Enter to execute, Tab to switch tabs. There is no way to explore the **context** of an item before acting on it.

### Pain Points

1. **Blind session creation**: Selecting a project in the palette always creates a new terminal session. The user cannot see which sessions already exist in that project.
2. **No project-scoped actions**: To pin/unpin a project, the user must switch to the Actions tab and find it. There is no "right-click" equivalent for contextual actions.
3. **No session management from palette**: Closing a specific session (not just "current") requires the sidebar. The palette only offers "Close Current Session."
4. **Context switching**: To see a project's sessions, the user must mentally cross-reference the Sessions tab with the Projects tab. There is no hierarchical view.

### User Request

> "When I press Right Arrow on a project, I want to see project actions and sessions within that project. Like drilling into a sub-menu."

---

## 2. Design Research

Three independent UX design perspectives were consulted:

### 2.1 Raycast / macOS Perspective
- Raycast uses a **stack-based push/pop model** for extension detail views
- Items are containers that can be explored, not just things to select
- Right Arrow pushes a detail view, Left Arrow pops back
- Escape always closes entirely (not just back)
- Search is scoped to the current level
- No animations -- instant transitions feel fast at 520px palette width

### 2.2 IntelliJ / VS Code Perspective
- Compared three approaches: pure Right Arrow, prefix-based (`project:zremote/`), and hybrid
- **Recommendation: pure Right Arrow** -- prefix syntax is overkill for <100 items and not discoverable
- VS Code's prefix system works because it has tens of thousands of symbols; ZRemote has dozens
- The spatial mental model ("items to the left, actions to the right") is more intuitive
- ChevronRight affordance is instantly discoverable

### 2.3 Terminal Power-User Perspective
- Keystroke count analysis shows 1-4 fewer keystrokes for common workflows
- No h/j/k/l for drill (palette has text input, typing `l` would add to query)
- Ctrl+J/K already work for up/down; arrow keys are correct for drill because palette is input-mode context
- No preview panel -- the drill-down itself IS the preview, using full 520px width
- Path stability: `Ctrl+K → "myr" → Right → Enter` = always same result for same data

### 2.4 Consensus

All three perspectives converged on the same core design:

| Decision | Consensus |
|----------|-----------|
| Navigation model | Stack-based (not column view, not prefix) |
| Drill-in gesture | Right Arrow |
| Drill-out gesture | Left Arrow / Backspace on empty query |
| Escape behavior | Always closes entirely |
| Tabs in drill-down | Hidden, replaced by breadcrumb |
| Search in drill-down | Scoped to current level |
| Query on transition | Cleared on drill, restored on pop |
| Max depth | 2 (Root → Item → Sub-item) |
| Animations | None (instant) |
| HostPicker | Migrated into navigation stack |

---

## 3. Design Specification

### 3.1 Navigation Model

Replace the current binary `PaletteSubView` (Main/HostPicker) with a **navigation stack**. Each drill-down pushes a context; going back pops it.

```
nav_stack: Vec<DrillDownLevel>   // empty = root level

enum DrillDownLevel {
    Project { project_idx: usize },
    Session { session_idx: usize },
    HostPicker,                    // migrated from PaletteSubView
}
```

State is saved on push and restored on pop:

```
struct SavedLevelState {
    query: String,
    selected_index: usize,
    active_tab: PaletteTab,       // only meaningful at root
}
```

### 3.2 Drillability Rules

| Item Type | Drillable? | Visual Affordance |
|-----------|-----------|-------------------|
| Project | Always | ChevronRight icon (12px, `text_tertiary()`) |
| Session | Always | ChevronRight icon |
| Action | Never | No chevron |
| Host (in host detail) | Yes (server mode) | ChevronRight icon |

On selection, the chevron shifts to `text_secondary()` for emphasis.

### 3.3 Complete Keyboard Interaction Map

**Root Level (existing behavior preserved + Right Arrow added):**

| Key | Action |
|-----|--------|
| Up / Ctrl+K | Move selection up (wraps) |
| Down / Ctrl+J | Move selection down (wraps) |
| Enter | Execute selected item (unchanged) |
| **Right Arrow** | **Drill into selected item (if drillable)** |
| Tab | Next tab |
| Shift+Tab | Previous tab |
| Backspace (empty query) | Close palette |
| Backspace (with query) | Delete last char |
| Escape | Close palette |
| Ctrl+K | Toggle palette |
| Ctrl+Shift+E/P/A | Switch to tab |
| Ctrl+V | Paste from clipboard |
| Printable chars | Append to query |

**Drill-Down Level:**

| Key | Action |
|-----|--------|
| Up / Ctrl+K | Move selection up (wraps within level) |
| Down / Ctrl+J | Move selection down (wraps within level) |
| Enter | Execute selected item in context |
| **Right Arrow** | Drill deeper (if item is drillable, e.g., session within project) |
| **Left Arrow** | Go back one level (restore parent state) |
| **Backspace (empty query)** | Go back one level |
| Backspace (with query) | Delete last char |
| **Escape** | **Close palette entirely (NOT just back)** |
| Tab / Shift+Tab | No-op (consumed, not leaked to terminal) |
| Printable chars | Append to query (filters within current level) |

### 3.4 Drill-Down Content

#### 3.4.1 Project Drill-Down

When Right Arrow is pressed on a project:

```
+------------------------------------------------------+
| [<] Projects / zremote                               |
+------------------------------------------------------+
| [Search] Search in zremote...                        |
+------------------------------------------------------+
| [git-branch] main  *dirty                            |  <- info header, not selectable
+------------------------------------------------------+
| SESSIONS (2)                                         |
| > [terminal] Session abc123 (zsh)   * active   2:45 |  <- drillable (ChevronRight)
|   [terminal] Session def456 (bash)  * suspended      |  <- drillable
| ACTIONS                                              |
|   [+] New Session in zremote                         |
|   [pin] Unpin zremote                                |
+------------------------------------------------------+
| [Left/Bksp] Back  [Enter] Select  [Esc] Close       |
+------------------------------------------------------+
```

**Content:**
- **Git info header** (non-selectable): branch name + dirty indicator. 28px height, `bg_tertiary()` background.
- **SESSIONS**: Sessions where `session.project_id == project.id`, active + suspended only. Each session is drillable (Right → session detail).
- **ACTIONS**: "New Session in {project}" (emits `CreateSessionInProject`), "Pin/Unpin {project}" (emits `ToggleProjectPin`).

**Empty project** (no sessions): Shows only ACTIONS section. "New Session" is pre-selected. `Right → Enter` immediately useful.

#### 3.4.2 Session Drill-Down

When Right Arrow is pressed on a session (from root or from within project):

```
+------------------------------------------------------+
| [<] Sessions / abc123 (zsh)                          |
+------------------------------------------------------+
| [Search] Session actions...                          |
+------------------------------------------------------+
| STATUS: active   UPTIME: 2h 45m                     |  <- info header, not selectable
| HOST: localhost   PROJECT: zremote                   |
+------------------------------------------------------+
| ACTIONS                                              |
| > [terminal] Switch to Session                       |  <- default selection
|   [X] Close Session                                  |
+------------------------------------------------------+
| [Left/Bksp] Back  [Enter] Select  [Esc] Close       |
+------------------------------------------------------+
```

**Content:**
- **Info header** (non-selectable): status, uptime, host, project, shell, PID, working dir. Compact key-value layout.
- **ACTIONS**: "Switch to Session" (emits `SelectSession`, default selection), "Close Session" (emits `CloseSession`).
- If the session is the currently active one, "Switch to Session" is replaced with "Already Active" (grayed, non-selectable) and "Close Session" becomes default.

#### 3.4.3 Host Drill-Down (server mode, future)

Available when hosts are rendered as items (server mode with multiple hosts):

```
+------------------------------------------------------+
| [<] Hosts / myhost                                   |
+------------------------------------------------------+
| [Search] Search on myhost...                         |
+------------------------------------------------------+
| PROJECTS                                             |
|   [folder] zremote     ~/Code/Me/zremote  main  *  >|  <- drillable
|   [folder] other       ~/Code/other       dev      >|
| SESSIONS                                             |
|   [terminal] orphan-session (bash)  * active   1:20  |
| ACTIONS                                              |
|   [+] New Session on myhost                          |
+------------------------------------------------------+
```

This is the maximum depth path: Root → Host → Project → Session (depth 3). Consider limiting to depth 2 by making projects in host detail execute `CreateSessionInProject` on Enter instead of drilling further.

### 3.5 Visual Design

#### 3.5.1 Breadcrumb Header (replaces tab bar in drill-down)

```
Height: 36px (same as tab bar)
Layout: [ChevronLeft 14px] [gap 6px] [parent "Projects" 12px text_secondary] [" / " 11px text_tertiary] [item "zremote" 13px text_primary semibold]
Background: same as tab bar
Bottom border: 1px border()
Click on ChevronLeft or parent label: go back
```

#### 3.5.2 Info Header (git info, session metadata)

```
Height: 28px per row (auto-height for multi-row)
Padding: 12px horizontal, 6px vertical
Background: bg_tertiary()
Top/bottom margins: 0px (flush with surrounding content)
Text: 11px, labels in text_tertiary(), values in text_secondary()
Not selectable, not part of navigation index
```

#### 3.5.3 ChevronRight Affordance on Drillable Items

```
Position: right edge of item row, after existing accessories
Size: 12px
Color: text_tertiary() default, text_secondary() when row is selected
Spacing: 4px gap before chevron, 12px right padding
```

The chevron does not appear on Action items. This visual distinction communicates drillability without text labels.

#### 3.5.4 Footer Hints (dynamic)

| Context | Footer |
|---------|--------|
| Root, non-drillable selected | `[Up/Down] Navigate [Enter] Select [Tab] Next tab [Esc] Close` |
| Root, drillable selected | `[Up/Down] Navigate [Right] Open [Enter] Select [Tab] Tab [Esc] Close` |
| Drill-down, non-drillable selected | `[Left] Back [Up/Down] Navigate [Enter] Select [Esc] Close` |
| Drill-down, drillable selected | `[Left] Back [Up/Down] Navigate [Right] Open [Enter] Select [Esc] Close` |

The footer dynamically checks `is_item_drillable(selected_index)` and conditionally shows the `[Right] Open` hint.

### 3.6 Search Behavior

**Root level**: Unchanged. Fuzzy search across all items filtered by active tab.

**Drill-down level**: Search is **scoped to items at the current level only**. Typing "zsh" in a project drill-down matches only that project's sessions and actions.

- Same `fuzzy_match_item()` function, applied to drill-down item list
- Placeholder text reflects scope: "Search in {project_name}..."
- Query **cleared on drill transition** (both in and out)
- Query **restored on pop** (saved in `SavedLevelState`)

**No global search from drill-down**: If a query matches nothing at the current level, show empty state for that context. Do not silently search the parent level -- this would be disorienting. User can press Left to go back and search globally.

### 3.7 Mouse Interaction

| Target | Action |
|--------|--------|
| Click ChevronRight area on drillable item | Drill into item (same as Right Arrow) |
| Click rest of item row | Execute item (same as Enter) |
| Click breadcrumb back button (`<`) | Go back (same as Left Arrow) |
| Click backdrop | Close palette |

Implementation note: The chevron needs a separate `Stateful<Div>` with its own `on_click` handler for the split click target.

### 3.8 Transition Behavior

**No animations.** Instant transitions for all state changes:
- Drill in: tab bar replaced by breadcrumb, results replaced, query cleared -- all in one frame
- Drill out: breadcrumb replaced by tab bar, results restored, query restored -- all in one frame
- Visual continuity provided by breadcrumb header (user sees where they are)

This is consistent with existing palette behavior (no open/close animation) and matches Raycast.

---

## 4. Keystroke Efficiency Analysis

### 4.1 Common Workflows Compared

**Switch to existing session in a project:**

| Method | Keystrokes | Sequence | Notes |
|--------|-----------|----------|-------|
| Sidebar click | 2-3 clicks | Mouse to sidebar, expand project, click session | Requires mouse |
| Current palette | 5-8 keys | Ctrl+K → type "session-name" → Down × N → Enter | Must know session name |
| **Proposed** | **4-6 keys** | **Ctrl+K → type "proj" → Right → Enter** | See all sessions in context |

**Create new session in a project:**

| Method | Keystrokes | Sequence | Notes |
|--------|-----------|----------|-------|
| Current palette | 5-7 keys | Ctrl+K → type "proj" → Enter | Creates blindly |
| **Proposed** | **5-7 keys** | **Ctrl+K → type "proj" → Right → Down to "New" → Enter** | User sees existing sessions first |

Same keystroke count, but the proposed approach gives the user informed choice.

**Close a session in a specific project:**

| Method | Keystrokes | Notes |
|--------|-----------|-------|
| Current palette | N/A | Only "Close Current Session" exists |
| **Proposed** | 6-8 keys | Ctrl+K → type "proj" → Right → select session → Right → "Close" → Enter |

**Previously impossible via palette.** The drill-down enables this workflow.

**Quick check project git status:**

| Method | Notes |
|--------|-------|
| Current | Look at sidebar (must find project, may be scrolled off) |
| **Proposed** | Ctrl+K → type "proj" → Right → git info visible in header |

### 4.2 Path Stability

Power users build muscle memory around predictable paths:

- `Ctrl+K → "myr" → Right → Enter` = always "first session in myremote project"
- Fuzzy match ordering is deterministic for the same data
- Drill-down item ordering is fixed (Sessions by status then creation time, Actions in fixed order)
- Selection always starts at index 0 on drill -- no "remembered last position" that could shift unpredictably

---

## 5. Edge Cases

| Scenario | Behavior |
|----------|----------|
| Right Arrow on Action item | No-op. No chevron visible, no visual feedback |
| Right Arrow when no items visible | No-op |
| Empty project (no sessions) | Show only ACTIONS. "New Session" pre-selected |
| Session closed while drill-down open | Palette uses snapshot (captured at open time). Stale data is acceptable -- palette is ephemeral |
| Rapid Right-Left-Right navigation | All transitions are synchronous (snapshot data), no race conditions |
| Search query + Right Arrow | Selected (filtered) item is drilled into. Parent query saved. Detail query starts empty |
| Very long project/session names | Truncated with ellipsis in breadcrumb. Full name searchable |
| Drill-down from "All" tab | Returns to "All" tab on pop (tab preserved in saved state) |
| Window resize while drilled down | GPUI re-layouts, selection preserved |
| 50+ sessions in one project | Scrollable list within drill-down, fuzzy search narrows in 2 keystrokes |

---

## 6. Integration with Existing Systems

### 6.1 Tab System

- **Root level**: Tabs work exactly as today (All, Sessions, Projects, Actions)
- **Drill-down level**: Tabs hidden, replaced by breadcrumb header
- **Tab memory**: Active tab preserved in `SavedLevelState`, restored on pop
- **Tab shortcuts (Ctrl+Shift+E/P/A)**: No-op during drill-down (avoids confusing state jumps)

### 6.2 Event System

No new `CommandPaletteEvent` variants needed. All drill-down actions map to existing events:

| Drill-Down Action | Existing Event |
|-------------------|----------------|
| Switch to session | `SelectSession { session_id, host_id }` |
| Close session | `CloseSession { session_id }` |
| New session in project | `CreateSessionInProject { host_id, working_dir }` |
| Pin/Unpin project | `ToggleProjectPin { project_id, pinned }` |

### 6.3 HostPicker Migration

The existing HostPicker sub-view becomes `DrillDownLevel::HostPicker` in the navigation stack. Behavior is identical:
- "New Session" action in multi-host server mode pushes HostPicker onto nav_stack
- Query clears, host list shown, Backspace pops back
- This unifies the code path -- no separate `handle_host_picker_key` method

### 6.4 Snapshot Data

Drill-downs reference the same `PaletteSnapshot` captured at palette open time. No additional API calls, no loading states, no staleness concerns beyond what already exists.

---

## 7. Implementation Plan

### Phase 1: State Model Changes

**File: `crates/zremote-gui/src/views/command_palette.rs`**

1. Define `DrillDownLevel` enum:
   ```rust
   #[derive(Debug, Clone)]
   enum DrillDownLevel {
       Project { project_idx: usize },
       Session { session_idx: usize },
       HostPicker,
   }
   ```

2. Define `SavedLevelState` struct:
   ```rust
   struct SavedLevelState {
       query: String,
       selected_index: usize,
       active_tab: PaletteTab,
   }
   ```

3. Replace `sub_view: PaletteSubView` with:
   ```rust
   nav_stack: Vec<DrillDownLevel>,
   nav_saved_state: Vec<SavedLevelState>,
   ```

4. Implement navigation methods:
   - `push_drill_down(&mut self, level: DrillDownLevel)` -- save current state, clear query, reset selection to 0, push level, recompute results
   - `pop_drill_down(&mut self)` -- pop level, restore state from `nav_saved_state`, recompute results
   - `is_drilled_down(&self) -> bool` -- `!nav_stack.is_empty()`
   - `current_level(&self) -> Option<&DrillDownLevel>`
   - `is_item_drillable(&self, item: &PaletteItem) -> bool` -- true for Project/Session, false for Action
   - `drill_down_context_for_selected(&self) -> Option<DrillDownLevel>` -- maps selected item to level

5. Migrate HostPicker: replace all `self.sub_view == PaletteSubView::HostPicker` checks with `matches!(self.nav_stack.last(), Some(DrillDownLevel::HostPicker))`

### Phase 2: Drill-Down Item Builders

**File: `crates/zremote-gui/src/views/command_palette.rs`**

6. Implement `build_project_detail_items(&self, project_idx: usize) -> Vec<ResultItem>`:
   - Filter `self.session_items` where session's `project_id` matches the project
   - Create action items: "New Session in {project.name}", "Pin/Unpin {project.name}"
   - Return grouped: SESSIONS category, ACTIONS category

7. Implement `build_session_detail_items(&self, session_idx: usize) -> Vec<ResultItem>`:
   - Create action items: "Switch to Session" (or "Already Active" if current), "Close Session"
   - Return with ACTIONS category

8. Modify `recompute_results()` to dispatch based on `nav_stack.last()`:
   ```rust
   fn recompute_results(&mut self) {
       match self.nav_stack.last() {
           None => {
               // Existing root logic (tabs, grouped/scored)
           }
           Some(DrillDownLevel::Project { project_idx }) => {
               let items = self.build_project_detail_items(*project_idx);
               self.results = if self.query.is_empty() {
                   self.group_detail_items(&items)
               } else {
                   self.score_detail_items(&items)
               };
           }
           Some(DrillDownLevel::Session { session_idx }) => {
               let items = self.build_session_detail_items(*session_idx);
               // same pattern
           }
           Some(DrillDownLevel::HostPicker) => {
               // Existing host picker logic, refactored
           }
       }
   }
   ```

### Phase 3: Keyboard Handling

**File: `crates/zremote-gui/src/views/command_palette.rs`**

9. Add Right Arrow handler in root-level `handle_key_down`:
   ```rust
   "right" if !mods.control && !mods.alt => {
       if let Some(level) = self.drill_down_context_for_selected() {
           self.push_drill_down(level);
           cx.notify();
       }
   }
   ```

10. Create `handle_drill_down_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>)`:
    - `"escape"` → `self.dismiss(cx)` (close entirely)
    - `"left"` → `self.pop_drill_down(); cx.notify()`
    - `"backspace"` + empty query → `self.pop_drill_down(); cx.notify()`
    - `"backspace"` + query → `self.query.pop(); self.recompute_results(); cx.notify()`
    - `"enter"` → execute selected drill-down item
    - `"right"` → drill deeper if selected item is drillable
    - `"up"` / `"down"` → navigate within level
    - `"tab"` / `"shift+tab"` → no-op (consumed)
    - Printable → append to query, recompute

11. Update `handle_key_down` entry to check drill-down state first:
    ```rust
    fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.is_drilled_down() {
            self.handle_drill_down_key(event, cx);
            return;
        }
        // ... existing root handling ...
    }
    ```

12. Remove separate `handle_host_picker_key` -- HostPicker is now handled via `handle_drill_down_key` with level-specific behavior

### Phase 4: Rendering

**File: `crates/zremote-gui/src/views/command_palette.rs`**

13. Implement `render_breadcrumb_header(&self, level: &DrillDownLevel) -> impl IntoElement`:
    - ChevronLeft icon (14px, `text_secondary()`, clickable)
    - Parent context label (12px, `text_secondary()`) -- "Projects", "Sessions", etc.
    - Separator "/" (11px, `text_tertiary()`)
    - Item name (13px, `text_primary()`, `FontWeight::SEMIBOLD`)
    - Height: 36px, bottom border: 1px `border()`
    - Click on ChevronLeft → `pop_drill_down()`

14. Implement `render_info_header(&self, entries: &[(&str, &str)]) -> impl IntoElement`:
    - Non-selectable key-value display
    - 28px height, `bg_tertiary()` background, 12px horizontal padding
    - Labels: 11px `text_tertiary()`, values: 11px `text_secondary()`
    - Used for git info (project) and session metadata (session)

15. Implement `render_drill_down_results(&self, cx: &mut Context<Self>) -> impl IntoElement`:
    - Uses existing `render_item_row` for selectable items
    - Inserts `render_info_header` at appropriate positions
    - Handles empty state: "No sessions in {project}" with action available

16. Modify `render_item_row` to show ChevronRight on drillable items:
    - Add 12px `Icon::ChevronRight` at right edge of row
    - Color: `text_tertiary()` default, `text_secondary()` when selected
    - Only shown when `is_item_drillable(item)` returns true

17. Update `render_footer` to be context-aware:
    - Check `is_drilled_down()` and `is_item_drillable(selected)` for dynamic hints
    - Root with drillable: add `[Right] Open`
    - Drill-down: replace `[Tab] Next tab` with `[Left] Back`

18. Update `Render::render()` to dispatch:
    ```rust
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = if let Some(level) = self.nav_stack.last() {
            match level {
                DrillDownLevel::HostPicker => self.render_host_picker(cx),
                _ => div()
                    .child(self.render_breadcrumb_header(level))
                    .child(self.render_input_bar())  // with contextual placeholder
                    .child(self.render_drill_down_results(cx))
                    .child(self.render_footer()),
            }
        } else {
            // Existing root rendering
            div()
                .child(self.render_tab_bar())
                .child(self.render_input_bar())
                .child(self.render_results(cx))
                .child(self.render_footer())
        };
        // ... wrap in palette container ...
    }
    ```

**File: `crates/zremote-gui/src/icons.rs`**

19. Add `ChevronLeft` variant to `Icon` enum, mapped to `"icons/chevron-left.svg"`

**File: `crates/zremote-gui/assets/icons/chevron-left.svg`**

20. Download from Lucide: `https://raw.githubusercontent.com/lucide-icons/lucide/main/icons/chevron-left.svg`

### Phase 5: Mouse Support

**File: `crates/zremote-gui/src/views/command_palette.rs`**

21. Split click target in `render_item_row` for drillable items:
    - Main row area → execute (Enter equivalent)
    - ChevronRight area (separate `Stateful<Div>`) → drill (Right Arrow equivalent)

22. Add click handler on breadcrumb ChevronLeft → `pop_drill_down()`

23. Existing backdrop click → close palette (unchanged)

### Phase 6: Tests

**File: `crates/zremote-gui/src/views/command_palette.rs` (test module)**

24. `test_push_pop_drill_down_state` -- verify query/selection save and restore
25. `test_build_project_detail_items` -- verify sessions filtered by `project_id`
26. `test_build_project_detail_empty` -- verify empty project shows only actions
27. `test_build_session_detail_items` -- verify correct actions generated
28. `test_build_session_detail_active` -- verify "Already Active" for current session
29. `test_is_item_drillable` -- Project=true, Session=true, Action=false
30. `test_right_arrow_drills_into_project` -- verify state after Right on project
31. `test_right_arrow_noop_on_action` -- verify no state change on Right on action
32. `test_left_arrow_pops_drill_down` -- verify pop and state restore
33. `test_backspace_empty_pops_drill_down` -- verify pop (not dismiss)
34. `test_escape_in_drill_down_closes` -- verify full dismiss
35. `test_tab_noop_in_drill_down` -- verify Tab does nothing at drill-down level
36. `test_search_scoped_to_drill_level` -- verify fuzzy match only on level items
37. `test_host_picker_migration` -- verify HostPicker works via nav_stack

---

## 8. Critical Files

| File | Change Type | Description |
|------|------------|-------------|
| `crates/zremote-gui/src/views/command_palette.rs` | Major modification | All core changes: navigation stack, key handling, item builders, rendering |
| `crates/zremote-gui/src/icons.rs` | Minor addition | `ChevronLeft` variant |
| `crates/zremote-gui/assets/icons/chevron-left.svg` | New file | Lucide SVG icon |
| `crates/zremote-gui/src/views/fuzzy.rs` | No change | Reused unchanged for drill-down search |
| `crates/zremote-gui/src/types.rs` | No change | Reference for Session.project_id, Project fields |
| `crates/zremote-gui/src/views/main_view.rs` | No change | Existing event routing covers all drill-down actions |

---

## 9. Verification

1. `cargo check -p zremote-gui` -- compilation
2. `cargo clippy -p zremote-gui` -- lint
3. `cargo test -p zremote-gui` -- unit tests (including all new drill-down tests)
4. Manual testing with `nix develop && cargo run -p zremote-gui`:
   - Open palette (Ctrl+K), navigate to a project, press Right → sessions + actions visible
   - Press Left → return to root, previous query and selection restored
   - Drill into session from project drill-down → session actions visible (depth 2)
   - Type search within drill-down → only current-level items filtered
   - Press Escape at any depth → palette closes entirely
   - Verify ChevronRight visible on projects/sessions, absent on actions
   - Verify footer hints update per level and per selected item drillability
   - Verify breadcrumb header shows correct parent context and item name
   - Verify HostPicker still works (server mode, "New Session" with multiple hosts)
   - Verify mouse: click chevron = drill, click row = execute, click `<` = back
5. Visual verification via `/visual-test` skill

---

## 10. Future Extensions

These are **not in scope** for this RFC but are enabled by the navigation stack architecture:

- **Host drill-down** (server mode): Root → Host → Projects/Sessions
- **Rename session** action in session drill-down
- **Open in editor** action in project drill-down
- **Session preview**: showing last N lines of terminal output in info header
- **Keyboard shortcut customization**: allowing users to remap drill-down keys
- **Breadcrumb trail**: for depth > 2, show full path like `Hosts / myhost / zremote / Session abc`

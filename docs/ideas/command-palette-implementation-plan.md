# Command Palette -- Implementation Plan

Detailed implementation plan for the GPUI command palette feature. Based on the idea spec (`docs/ideas/command-palette.md`), thorough codebase analysis, UX design reviews, and ASCII mockups.

**Companion documents:**
- Idea spec: `docs/ideas/command-palette.md`
- Mockups: embedded below per phase

---

## Codebase Analysis Summary

| File | Lines | Key findings |
|---|---|---|
| `main_view.rs` | 139 | No FocusHandle, no overlay system, no keystroke observation. Simple 3-field struct. |
| `terminal_panel.rs` | ~1000 | Ctrl+F intercepted at line 726 before encoder -- same pattern for Ctrl+K. Emits no events to MainView. `open_search()` is private. |
| `search_overlay.rs` | 212 | Reference pattern: FocusHandle + Render + EventEmitter. Char-by-char input via `on_key_down`. Auto-focus on render. |
| `sidebar.rs` | 803 | `hosts`, `sessions`, `projects` are private. `create_session(host_id, working_dir, cx)`, `close_session(session_id, cx)` are private. No pin toggle method. |
| `persistence.rs` | ~60 | `GuiState` has 5 fields. No `recent_sessions`. `is_default()` checks all fields. |
| `types.rs` | ~50 | `Host`, `Session`, `Project` are `Clone + Deserialize`. Session has `created_at: Option<String>`. |
| `icons.rs` | ~45 | 15 variants. Has `Search`. Needs `Command`, `Folder`, `Zap`. |
| `theme.rs` | ~50 | All 13 color functions available. No additions needed. |

---

## New Files

| File | Phase | ~Lines | Purpose |
|---|---|---|---|
| `views/command_palette.rs` | 2-4 | 600-700 | Main palette entity |
| `views/fuzzy.rs` | 1 | 80 | Fuzzy matching module |
| `views/double_shift.rs` | 3 | 30 | Double-Shift detection state machine |
| `assets/icons/command.svg` | 1 | 1 | Lucide icon |
| `assets/icons/folder.svg` | 1 | 1 | Lucide icon |
| `assets/icons/zap.svg` | 1 | 1 | Lucide icon |

## Modified Files

| File | Phase | Changes |
|---|---|---|
| `views/mod.rs` | 1 | Add `command_palette`, `fuzzy`, `double_shift` modules |
| `icons.rs` | 1 | Add `Command`, `Folder`, `Zap` variants |
| `persistence.rs` | 1 | Add `RecentSession` struct, `recent_sessions` field |
| `views/sidebar.rs` | 1 | Add pub accessors: `hosts()`, `sessions()`, `projects()`, `selected_session_id()`. Make `create_session()`, `close_session()` pub. |
| `views/terminal_panel.rs` | 3 | Add `TerminalPanelEvent` enum + `EventEmitter`. Intercept Ctrl+K, Ctrl+Shift+E/P/A before encoder. Make `open_search()` pub. |
| `views/main_view.rs` | 3 | Major rewrite: FocusHandle, Double-Shift, palette lifecycle, overlay rendering, event routing. ~139 -> ~300 lines. |

---

## Phase 1: Foundation & Data Layer

**Goal**: Fuzzy search testable, icons exist, persistence supports recent sessions, sidebar exposes data publicly. All infrastructure ready.

**Dependencies**: None.
**Complexity**: ~150 lines new, ~30 lines modifications. Low difficulty.

### 1.1 Create `views/fuzzy.rs`

```rust
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    pub score: i32,
    pub matched_indices: Vec<usize>,
}

/// Case-insensitive fuzzy match. Returns None if query chars not found in order.
pub fn fuzzy_match(query: &str, target: &str) -> Option<FuzzyMatch>;

/// Match against "title subtitle" with 0.5x multiplier on subtitle position scores.
pub fn fuzzy_match_item(query: &str, title: &str, subtitle: &str) -> Option<FuzzyMatch>;
```

Scoring factors:

| Factor | Score |
|---|---|
| Exact prefix | +100 |
| Consecutive match | +10/char |
| Word boundary start (`_`, `-`, `/`, camelCase) | +15 |
| CamelCase transition | +8 |
| Position penalty | -1/pos |
| Gap penalty | -3/gap |
| Case-exact bonus | +5 |

Unit tests (~40 lines):
- `test_exact_prefix` -- "my" matches "myproject"
- `test_no_match` -- "xyz" vs "myproject" returns None
- `test_word_boundary` -- "mp" matches "my-project"
- `test_camel_case` -- "mp" matches "MyProject"
- `test_consecutive_bonus` -- "proj" > "p_r_o_j"
- `test_case_exact_bonus` -- "My" > "my" against "MyProject"
- `test_position_penalty` -- start match > end match
- `test_empty_query` -- returns None
- `test_subtitle_multiplier` -- title match scores higher than subtitle match

### 1.2 Create SVG Icons

Download from Lucide and save to `crates/zremote-gui/assets/icons/`:
- `command.svg`
- `folder.svg`
- `zap.svg`

### 1.3 Modify `icons.rs`

Add 3 variants to `Icon` enum + path mappings:

```rust
Command,  // "icons/command.svg"
Folder,   // "icons/folder.svg"
Zap,      // "icons/zap.svg"
```

### 1.4 Modify `persistence.rs`

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentSession {
    pub session_id: String,
    pub timestamp: i64,  // unix seconds
}
```

Add to `GuiState`:
```rust
#[serde(default)]
pub recent_sessions: Vec<RecentSession>,
```

Update `is_default()`: add `&& self.recent_sessions.is_empty()`.

Add method to `Persistence`:
```rust
pub fn record_session_access(&mut self, session_id: &str) {
    self.state.recent_sessions.retain(|r| r.session_id != session_id);
    self.state.recent_sessions.insert(0, RecentSession {
        session_id: session_id.to_string(),
        timestamp: chrono::Utc::now().timestamp(),
    });
    self.state.recent_sessions.truncate(10);
    let _ = self.save_if_changed();
}
```

### 1.5 Modify `sidebar.rs`

Add public accessors:
```rust
pub fn hosts(&self) -> &[Host] { &self.hosts }
pub fn sessions(&self) -> &[Session] { &self.sessions }
pub fn projects(&self) -> &[Project] { &self.projects }
pub fn selected_session_id(&self) -> Option<&str> { self.selected_session_id.as_deref() }
```

Make existing methods public:
- `create_session` (line 134): `fn` -> `pub fn`
- `close_session` (line 190): `fn` -> `pub fn`

### 1.6 Modify `views/mod.rs`

Add:
```rust
pub mod command_palette;
pub mod fuzzy;
pub mod double_shift;
```

### Verification

```bash
cargo check -p zremote-gui
cargo test -p zremote-gui   # fuzzy search unit tests
cargo clippy -p zremote-gui
```

---

## Phase 2: Command Palette Entity

**Goal**: Self-contained GPUI entity that renders the full palette UI. Handles all internal state: text input, tab switching, filtering, selection, empty states. Emits events. **Not yet wired to shortcuts or MainView.**

**Dependencies**: Phase 1.
**Complexity**: ~600 lines. High difficulty (largest phase).

### 2.1 Type Definitions

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteTab {
    All, Sessions, Projects, Actions,
}

impl PaletteTab {
    pub fn label(self) -> &'static str;
    pub fn short_label(self) -> &'static str;    // "All", "Sess", "Proj", "Act"
    pub fn placeholder(self) -> &'static str;    // "Search everything...", etc.
    pub fn next(self) -> Self;                   // wrapping cycle
    pub fn prev(self) -> Self;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteCategory {
    Recent, Active, Suspended, Sessions,
    Pinned, AllProjects, Actions,
}

impl PaletteCategory {
    pub fn label(self) -> &'static str;  // "RECENT", "ACTIVE", etc.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteSubView { Main, HostPicker }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    CloseCurrentSession { session_id: String },
    SearchInTerminal,
    NewSession,
    ToggleProjectPin { project_id: String, project_name: String, currently_pinned: bool },
    Reconnect,
}

impl PaletteAction {
    pub fn title(&self) -> String;
    pub fn icon(&self) -> Icon;
    pub fn shortcut_hint(&self) -> Option<&'static str>;
    pub fn priority(&self) -> u8;  // lower = higher in list
}
```

### 2.2 Item Types

```rust
#[derive(Debug, Clone)]
pub enum PaletteItem {
    Session(SessionItem),
    Project(ProjectItem),
    Action(PaletteAction),
}

#[derive(Debug, Clone)]
pub struct SessionItem {
    pub session: Session,
    pub host_name: String,
    pub project_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectItem {
    pub project: Project,
    pub host_name: String,
}

impl PaletteItem {
    pub fn id(&self) -> String;                     // stable ID for selection tracking
    pub fn title(&self) -> String;                  // display title
    pub fn subtitle(&self, mode: &str) -> String;   // display subtitle (mode-aware)
    pub fn icon(&self) -> Icon;
    pub fn category_priority(&self) -> u8;          // tiebreaker: 0=recent, 1=session, 2=project, 3=action
}
```

### 2.3 Result Types

```rust
pub struct ResultItem {
    pub item: PaletteItem,
    pub title: String,          // pre-computed
    pub subtitle: String,       // pre-computed
    pub fuzzy_match: Option<FuzzyMatch>,
}

pub struct CategoryGroup {
    pub category: PaletteCategory,
    pub items: Vec<ResultItem>,
}

pub enum PaletteResults {
    Grouped(Vec<CategoryGroup>),   // empty query
    Scored(Vec<ResultItem>),       // with query
}

impl PaletteResults {
    pub fn selectable_count(&self) -> usize;
    pub fn item_at(&self, index: usize) -> Option<&ResultItem>;
    pub fn index_of(&self, item_id: &str) -> Option<usize>;
}
```

### 2.4 Snapshot

```rust
pub struct PaletteSnapshot {
    pub hosts: Vec<Host>,
    pub sessions: Vec<Session>,
    pub projects: Vec<Project>,
    pub mode: String,                       // "server" or "local"
    pub active_session_id: Option<String>,
    pub recent_sessions: Vec<RecentSession>,
}

impl PaletteSnapshot {
    pub fn capture(
        hosts: &[Host], sessions: &[Session], projects: &[Project],
        mode: &str, active_session_id: Option<&str>,
        recent_sessions: &[RecentSession],
    ) -> Self;

    pub fn host_name(&self, host_id: &str) -> String;
    pub fn project_name(&self, project_id: &str) -> Option<String>;
    pub fn is_recent(&self, session_id: &str) -> bool;
    pub fn online_hosts(&self) -> Vec<&Host>;
}
```

### 2.5 Events

```rust
pub enum CommandPaletteEvent {
    SelectSession { session_id: String, host_id: String },
    CreateSessionInProject { host_id: String, working_dir: String },
    CreateSession { host_id: String },
    CloseSession { session_id: String },
    OpenSearch,
    ToggleProjectPin { project_id: String, pinned: bool },
    Reconnect,
    Close,
}

impl EventEmitter<CommandPaletteEvent> for CommandPalette {}
```

### 2.6 Main Struct

```rust
pub struct CommandPalette {
    focus_handle: FocusHandle,
    query: String,
    active_tab: PaletteTab,
    selected_index: usize,
    hovered_index: Option<usize>,
    sub_view: PaletteSubView,
    snapshot: PaletteSnapshot,
    all_items: Vec<ResultItem>,     // all possible items, built once
    results: PaletteResults,        // current filtered view
}
```

### 2.7 Public API

```rust
impl CommandPalette {
    pub fn new(snapshot: PaletteSnapshot, initial_tab: PaletteTab, cx: &mut Context<Self>) -> Self;
    pub fn active_tab(&self) -> PaletteTab;
    pub fn switch_tab(&mut self, tab: PaletteTab, cx: &mut Context<Self>);
}

impl Focusable for CommandPalette { ... }
impl Render for CommandPalette { ... }
```

### 2.8 Private Methods

**State mutation:**
```rust
fn set_query(&mut self, query: String);
fn move_selection(&mut self, delta: i32);
fn execute_selected(&mut self, cx: &mut Context<Self>);
fn execute_item(&mut self, item: &PaletteItem, cx: &mut Context<Self>);
fn enter_host_picker(&mut self);
fn exit_host_picker(&mut self);
fn dismiss(&mut self, cx: &mut Context<Self>);
```

**Computation:**
```rust
fn build_all_items(snapshot: &PaletteSnapshot) -> Vec<ResultItem>;
fn recompute_results(&mut self);
fn compute_grouped(&self) -> Vec<CategoryGroup>;
fn compute_scored(&self) -> Vec<ResultItem>;
fn items_for_tab<'a>(&'a self, tab: PaletteTab) -> Vec<&'a ResultItem>;
fn categorize_item(&self, item: &PaletteItem) -> PaletteCategory;
```

**Rendering (~15 functions):**
```rust
fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement;
fn render_tab_pill(&self, tab: PaletteTab, count: usize, cx: &mut Context<Self>) -> impl IntoElement;
fn render_input_bar(&self) -> impl IntoElement;
fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement;
fn render_category_header(label: &str, is_first: bool) -> impl IntoElement;
fn render_item_row(&self, item: &ResultItem, index: usize, cx: &mut Context<Self>) -> impl IntoElement;
fn render_highlighted_title(title: &str, m: Option<&FuzzyMatch>) -> impl IntoElement;
fn render_highlighted_subtitle(subtitle: &str, title_len: usize, m: Option<&FuzzyMatch>) -> impl IntoElement;
fn render_session_accessory(session: &Session) -> impl IntoElement;
fn render_project_accessory(project: &Project) -> impl IntoElement;
fn render_action_accessory(action: &PaletteAction) -> impl IntoElement;
fn render_footer(&self) -> impl IntoElement;
fn render_key_pill(key: &str) -> impl IntoElement;
fn render_empty_state(&self) -> impl IntoElement;
fn render_host_picker(&self, cx: &mut Context<Self>) -> impl IntoElement;
```

**Utilities (module-level):**
```rust
fn format_duration(created_at: Option<&str>) -> String;
fn compact_path(path: &str) -> String;
```

### 2.9 Render Structure

```
CommandPalette::render()
├── auto-focus if !focused
├── on_key_down handler (ALL keyboard input)
│   ├── Escape → emit Close
│   ├── Enter → execute_selected
│   ├── Up/Down, Ctrl+J/K → move_selection
│   ├── Tab/Shift+Tab → switch tab
│   ├── Backspace → pop char or close/exit_host_picker on empty
│   ├── Ctrl+K → emit Close (toggle)
│   ├── Ctrl+Shift+E/P/A → switch tab or emit Close
│   ├── Ctrl+V → paste from clipboard
│   ├── Printable chars → append to query, recompute, notify
│   └── All other Ctrl+letter → consumed (never leak)
├── div (palette container)
│   ├── render_tab_bar()     -- 36px
│   ├── render_input_bar()   -- 40px
│   ├── render_results()     -- scrollable, max 316px
│   │   ├── Grouped: category headers + item rows
│   │   ├── Scored: flat item rows
│   │   └── Empty: render_empty_state()
│   └── render_footer()      -- 28px
```

### 2.10 Key Visual Specs

**Palette container:**
- `bg_secondary()`, 1px `border()`, 8px corner radius
- Shadow: `0 8px 24px rgba(0,0,0,0.5)`

**Item row (32px):**
```
┌──────────────────────────────────────────────────────────────┐
│ 12px [icon 16x16] 8px [title + subtitle stack]  [access.] 12px │
│      flex-shrink-0     flex-1 min-w-0 truncate  flex-shrink-0  │
└──────────────────────────────────────────────────────────────┘
```
- Selected: `bg_tertiary()` + 2px `accent()` left border
- Hovered: `bg_tertiary()` (no left border)
- Normal: transparent

**Category header (20px, not selectable):**
- 8px top margin (4px for first), 2px bottom margin
- 12px left padding, UPPERCASE, 11px SEMIBOLD, `text_tertiary()`

**Typography:**

| Element | Size | Weight | Color |
|---|---|---|---|
| Tab labels | 12px | MEDIUM | Active: `text_primary()`, Inactive: `text_secondary()` |
| Search input | 13px | NORMAL | `text_primary()` / `text_tertiary()` placeholder |
| Category headers | 11px | SEMIBOLD | `text_tertiary()`, UPPERCASE |
| Item title | 13px | NORMAL | `text_primary()` |
| Title fuzzy match | 13px | SEMIBOLD | `accent()` |
| Subtitle | 11px | NORMAL | `text_secondary()` |
| Accessories | 11px | NORMAL | `text_secondary()` / `text_tertiary()` |
| Footer | 11px | NORMAL | `text_tertiary()` |

**Status dots:** 6x6px, rounded-full. Green `success()` = active, yellow `warning()` = suspended/dirty, gray `text_tertiary()` = creating/offline.

**Git branch pill:** `bg_tertiary()`, 3px radius, 4px h-padding, `GitBranch` icon 10px + branch name, max 120px truncate.

**Shortcut pill:** `bg_primary()`, 1px `border()`, 3px radius, 4px h-padding, 11px `text_tertiary()`.

### Verification

```bash
cargo check -p zremote-gui
cargo clippy -p zremote-gui
# Cannot visually test yet (not wired to MainView)
```

---

## Phase 3: Shortcut Integration & MainView Wiring

**Goal**: All 5 shortcuts open the palette. Palette renders as overlay in MainView. Events route to sidebar/terminal actions. Full open/close lifecycle.

**Dependencies**: Phase 2.
**Complexity**: ~200 lines modifications. Medium difficulty.

### 3.1 Create `views/double_shift.rs`

```rust
use std::time::Instant;

pub struct DoubleShiftState {
    last_shift_down: Option<Instant>,
}

impl DoubleShiftState {
    pub fn new() -> Self;
    /// Returns true if Double-Shift detected. Called from observe_keystrokes.
    pub fn on_keystroke(&mut self, keystroke: &gpui::Keystroke) -> bool;
    fn reset(&mut self);
}
```

Algorithm:
- Shift key_down (not repeat, no other modifiers): check if < 400ms since last → trigger, else record
- Any non-Shift key_down: reset

### 3.2 Modify `terminal_panel.rs`

**Add event type:**
```rust
pub enum TerminalPanelEvent {
    OpenCommandPalette { tab: PaletteTab },
}
impl EventEmitter<TerminalPanelEvent> for TerminalPanel {}
```

**Add shortcut interceptions** in `on_key_down` handler, AFTER existing Ctrl+F check (line 731), BEFORE the search_open gate (line 734):

```rust
// Ctrl+K → palette All tab
if mods.control && !mods.shift && !mods.alt && key == "k" {
    let _ = entity.update(cx, |this, cx| {
        if this.search_open { this.close_search(cx); }
        cx.emit(TerminalPanelEvent::OpenCommandPalette { tab: PaletteTab::All });
    });
    return;
}

// Ctrl+Shift+E → palette Sessions tab
if mods.control && mods.shift && !mods.alt && key == "e" {
    let _ = entity.update(cx, |this, cx| {
        if this.search_open { this.close_search(cx); }
        cx.emit(TerminalPanelEvent::OpenCommandPalette { tab: PaletteTab::Sessions });
    });
    return;
}

// Ctrl+Shift+P → palette Projects tab (same pattern)
// Ctrl+Shift+A → palette Actions tab (same pattern)
```

**Make `open_search()` public** (line 495): `fn open_search` → `pub fn open_search`.

### 3.3 Rewrite `main_view.rs`

**New struct:**
```rust
pub struct MainView {
    app_state: Arc<AppState>,
    sidebar: Entity<SidebarView>,
    terminal: Option<Entity<TerminalPanel>>,
    focus_handle: FocusHandle,
    command_palette: Option<Entity<CommandPalette>>,
    double_shift: DoubleShiftState,
    _keystroke_sub: Subscription,
}
```

**Constructor:**
```rust
pub fn new(app_state, window, cx) -> Self {
    let sidebar = cx.new(|cx| SidebarView::new(app_state.clone(), cx));
    cx.subscribe(&sidebar, Self::on_sidebar_event).detach();
    Self::start_event_polling(&app_state, cx);
    let focus_handle = cx.focus_handle();
    let _keystroke_sub = cx.observe_keystrokes(Self::on_keystroke);
    Self {
        app_state, sidebar, terminal: None,
        focus_handle, command_palette: None,
        double_shift: DoubleShiftState::new(),
        _keystroke_sub,
    }
}
```

**New methods:**

```rust
fn open_command_palette(&mut self, tab: PaletteTab, cx: &mut Context<Self>) {
    // Toggle: if open on same tab, close
    if let Some(palette) = &self.command_palette {
        if palette.read(cx).active_tab() == tab {
            self.close_command_palette(cx);
            return;
        }
        // Different tab: switch
        palette.update(cx, |p, cx| p.switch_tab(tab, cx));
        return;
    }
    // Build snapshot from sidebar + persistence
    let snapshot = self.sidebar.update(cx, |sidebar, _cx| {
        PaletteSnapshot::capture(
            sidebar.hosts(), sidebar.sessions(), sidebar.projects(),
            &self.app_state.mode,
            sidebar.selected_session_id(),
            &self.app_state.persistence.lock().ok()
                .map(|p| p.state().recent_sessions.clone())
                .unwrap_or_default(),
        )
    });
    let palette = cx.new(|cx| CommandPalette::new(snapshot, tab, cx));
    cx.subscribe(&palette, Self::on_palette_event).detach();
    self.command_palette = Some(palette);
    cx.notify();
}

fn close_command_palette(&mut self, cx: &mut Context<Self>) {
    self.command_palette = None;
    cx.notify();
}

fn on_keystroke(&mut self, event: &KeystrokeEvent, _window: &mut Window, cx: &mut Context<Self>) {
    if self.double_shift.on_keystroke(&event.keystroke) {
        if self.command_palette.is_some() {
            self.close_command_palette(cx);
        } else {
            self.open_command_palette(PaletteTab::All, cx);
        }
    }
}

fn on_terminal_event(&mut self, _: Entity<TerminalPanel>, event: &TerminalPanelEvent, cx: &mut Context<Self>) {
    match event {
        TerminalPanelEvent::OpenCommandPalette { tab } => self.open_command_palette(*tab, cx),
    }
}

fn on_palette_event(&mut self, _: Entity<CommandPalette>, event: &CommandPaletteEvent, cx: &mut Context<Self>) {
    match event {
        SelectSession { session_id, host_id } => {
            self.record_recent_session(session_id);
            self.open_terminal(session_id, host_id, cx);
        }
        CreateSessionInProject { host_id, working_dir } => {
            self.sidebar.update(cx, |s, cx| s.create_session(host_id, Some(working_dir.clone()), cx));
        }
        CreateSession { host_id } => {
            self.sidebar.update(cx, |s, cx| s.create_session(host_id, None, cx));
        }
        CloseSession { session_id } => {
            self.sidebar.update(cx, |s, cx| s.close_session(session_id, cx));
        }
        OpenSearch => {
            if let Some(terminal) = &self.terminal {
                terminal.update(cx, |t, cx| t.open_search(cx));
            }
        }
        ToggleProjectPin { project_id, pinned } => {
            // API call to toggle pin (async)
        }
        Reconnect => {
            // Force sidebar reload / reconnect events_ws
        }
        Close => {}
    }
    self.close_command_palette(cx);
}

fn record_recent_session(&self, session_id: &str) {
    if let Ok(mut p) = self.app_state.persistence.lock() {
        p.record_session_access(session_id);
    }
}
```

**Modified `open_terminal()`** -- add terminal event subscription:
```rust
fn open_terminal(&mut self, session_id: &str, _host_id: &str, cx: &mut Context<Self>) {
    // ... existing WebSocket setup ...
    let terminal = cx.new(|cx| TerminalPanel::new(...));
    cx.subscribe(&terminal, Self::on_terminal_event).detach();  // NEW
    self.terminal = Some(terminal);
    cx.notify();
}
```

**Modified `on_sidebar_event`** -- record recent on session selection:
```rust
SidebarEvent::SessionSelected { session_id, host_id } => {
    self.record_recent_session(session_id);  // NEW
    self.open_terminal(session_id, host_id, cx);
}
```

**Modified Render** -- overlay system:
```rust
impl Render for MainView {
    fn render(&mut self, _window, cx) -> impl IntoElement {
        let mut root = div()
            .flex().size_full().bg(theme::bg_primary())
            .child(self.sidebar.clone())
            .child(if let Some(terminal) = &self.terminal {
                div().flex_1().child(terminal.clone()).into_any_element()
            } else {
                // Empty state with focus handle for shortcuts
                div().flex_1()
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(|this, event, _window, cx| {
                        // Ctrl+K, Ctrl+Shift+E/P/A handlers
                    }))
                    .child(self.render_empty_state(cx))
                    .into_any_element()
            });

        // Overlay
        if let Some(palette) = &self.command_palette {
            root = root.child(
                div().absolute().inset_0()
                    .bg(gpui::rgba(0x11111366))  // 40% opacity backdrop
                    .on_click(cx.listener(|this, _, _, cx| this.close_command_palette(cx)))
                    .child(
                        div().absolute()
                            .top(pct(20.0)).left(pct(50.0))
                            .w(px(520.0)).ml(px(-260.0))
                            .max_h(px(420.0))
                            .rounded(px(8.0)).border_1().border_color(theme::border())
                            .bg(theme::bg_secondary())
                            .shadow(/* ... */)
                            .overflow_hidden()
                            .on_click(|_, _, _| {})  // stop propagation
                            .child(palette.clone()),
                    ),
            );
        }
        root
    }
}
```

### Verification

```bash
cargo check -p zremote-gui
cargo clippy -p zremote-gui
# Visual test: Ctrl+K opens/closes palette
# Visual test: Double-Shift opens/closes
# Visual test: Ctrl+Shift+E/P/A opens specific tabs
# Visual test: Escape closes, backdrop click closes
# Visual test: session selection switches terminal
# Visual test: empty states render correctly
```

---

## Phase 4: Polish & Edge Cases

**Goal**: All edge cases handled, host picker sub-flow, responsive behavior, full visual polish.

**Dependencies**: Phase 3.
**Complexity**: ~100 lines modifications. Low-medium difficulty.

### 4.1 Host Picker Sub-flow

In `command_palette.rs`:
- When "New Session" selected in server mode with multiple hosts: `enter_host_picker()`
- Sets `sub_view = PaletteSubView::HostPicker`
- Renders online hosts as selectable items
- Title: "Select host for new session"
- Backspace on empty → `exit_host_picker()`
- Footer shows `[Backspace] Back`

Visual mockup:
```
┌─────────────────────────────────────────────────────────┐
│  Select host for new session                            │
│─────────────────────────────────────────────────────────│
│  🔍  Search hosts...|                                   │
│─────────────────────────────────────────────────────────│
│                                                         │
│  ┃ [S] prod-host-1                      ● online       │
│         Ubuntu 22.04 · x86_64                          │
│    [S] dev-host                         ● online       │
│         Arch Linux · x86_64                            │
│                                                         │
│─────────────────────────────────────────────────────────│
│  ↑↓ Navigate   ⏎ Select   ⌫ Back   Esc Close         │
└─────────────────────────────────────────────────────────┘
```

### 4.2 Responsive Tab Labels

Track palette width. If < 400px, use `short_label()` ("Sess", "Proj", "Act").

### 4.3 Scroll-into-View

When `selected_index` changes via Up/Down, ensure selected item visible in scroll container.

### 4.4 on_blur Safety

Add `cx.on_blur()` on focus_handle to emit Close -- catches edge cases beyond backdrop click.

### 4.5 Local Mode Host Resolution

When `CreateSession` or `CreateSessionInProject` needs a `host_id` in local mode:
```rust
let host_id = self.snapshot.hosts.first()
    .map(|h| h.id.clone())
    .unwrap_or_default();
```

### Verification

Full visual test matrix:

| Test | Expected |
|---|---|
| Ctrl+K | Opens/closes palette on All tab |
| Double-Shift | Opens/closes palette on All tab |
| Ctrl+Shift+E | Opens Sessions tab |
| Ctrl+Shift+P | Opens Projects tab |
| Ctrl+Shift+A | Opens Actions tab |
| Typing | Filters items with fuzzy highlights |
| Up/Down | Moves selection with wrap-around |
| Enter | Executes selected item |
| Tab/Shift+Tab | Switches tabs, query persists |
| Escape | Closes palette |
| Backdrop click | Closes palette |
| Backspace empty | Closes palette |
| Session select | Switches terminal |
| Project select | Creates new session in dir |
| New Session (multi-host) | Shows host picker |
| Host picker Backspace | Returns to main view |
| Empty state (no sessions) | Icon + message centered |
| No results | "No results for..." message |
| Recent items | Shown first in All/Sessions tabs |
| Server mode | Host in subtitles |
| Local mode | No host in subtitles |

---

## Dependency Graph

```
Phase 1 (Foundation)     ~150 lines  ──→  cargo test passes
    │
    ▼
Phase 2 (Palette Entity) ~600 lines  ──→  cargo check passes
    │
    ▼
Phase 3 (Integration)    ~200 lines  ──→  visual test: palette opens/closes
    │
    ▼
Phase 4 (Polish)         ~100 lines  ──→  full test matrix passes
```

Strictly sequential. Total: ~1050 lines new/modified code.

---

## Risk Assessment

| Risk | Severity | Mitigation |
|---|---|---|
| `observe_keystrokes` may not fire for bare Shift | Medium | Fallback: track Shift in `on_key_down`/`on_key_up` on root div |
| GPUI overlay z-ordering | Medium | Test early in Phase 3. Fallback: GPUI `overlay()` element |
| Focus management on palette close | Low | Terminal auto-focuses on render (existing behavior). Verified in SearchOverlay pattern. |
| Scroll-into-view API | Low | Manual offset tracking if GPUI lacks programmatic scroll-to |
| `sidebar.read()` vs `.update()` API | Low | Use `.update(cx, \|sidebar, _cx\| ...)` which is known to work |

---

## Completeness Checklist

### From idea spec

- [x] IntelliJ-style tabs (All, Sessions, Projects, Actions)
- [x] 5 trigger shortcuts (Double-Shift, Ctrl+K, Ctrl+Shift+E/P/A)
- [x] Double-Shift detection algorithm with edge cases
- [x] Shortcut interception before terminal encoder
- [x] MainView FocusHandle for empty state
- [x] Snapshot data model (no API calls)
- [x] Fuzzy search with scoring factors
- [x] Subtitle matching with 0.5x multiplier
- [x] 1-char query: score only, no highlight
- [x] Tab bar with count badges
- [x] Input bar with per-tab placeholder
- [x] Footer with contextual hints
- [x] Item row: icon + title/subtitle + accessories
- [x] Selection model: sticky keyboard, independent hover
- [x] Category headers: UPPERCASE, 11px, SEMIBOLD
- [x] Status dots: 6px, green/yellow/gray
- [x] Git branch pills
- [x] Shortcut hint pills
- [x] Empty states: no data, no results, empty group
- [x] Host picker sub-flow (server mode, multi-host)
- [x] Recent items tracking (max 10, dedup, prune stale)
- [x] Server vs local mode differences
- [x] Dismissal matrix (Esc, backdrop, backspace, toggle)
- [x] Wrap-around navigation (Up/Down)
- [x] Query persists across tab switches
- [x] Scroll/selection reset on tab switch
- [x] All Ctrl+letter combos consumed (no leak to terminal)
- [x] Ctrl+V paste support
- [x] Responsive behavior (narrow window)
- [x] Instant open/close (no animation)
- [x] Duration format: M:SS, Hh Mm, Nd Hh
- [x] Path compaction: ~ for home, truncate middle

### From codebase patterns

- [x] Follows SearchOverlay pattern (FocusHandle, EventEmitter, on_key_down)
- [x] Uses theme::*() for all colors
- [x] Uses icon() helper for all icons
- [x] Uses px() for sizing
- [x] EventEmitter pattern for parent communication
- [x] cx.subscribe() for event routing
- [x] cx.notify() for reactivity

### Missing / deferred

- [ ] `Reconnect` action: no reconnect mechanism exists in current codebase. Deferred -- will force sidebar reload when implemented.
- [ ] `ToggleProjectPin`: no pin toggle API call exists in current codebase. Will need `api.update_project()` or similar.
- [ ] Tooltip on truncated items: nice-to-have, not MVP.
- [ ] IME support: same limitation as SearchOverlay. Document it.

---

## Mockup Reference

See companion mockups (13 states) covering:

1. All Tab -- empty query (home state)
2. All Tab -- with query "api" (fuzzy highlights)
3. Sessions Tab -- empty query (3 groups)
4. Sessions Tab -- no results
5. Projects Tab -- empty query (pinned + all)
6. Projects Tab -- with query "rem"
7. Actions Tab -- terminal active (5 actions)
8. Actions Tab -- no terminal (3 actions)
9. Empty state -- no sessions
10. Host picker sub-flow
11. Selection vs hover states
12. Narrow window (< 500px)
13. Full app context (sidebar + backdrop + palette)

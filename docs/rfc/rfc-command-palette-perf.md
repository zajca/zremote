# RFC: Command Palette Performance Optimization

## Problem

The command palette is noticeably slow to appear and tab switching feels laggy. Root causes:

1. **Excessive cloning**: Every `build_session_items()` / `build_project_items()` / `build_action_items()` call clones all Session and Project structs. Called on every keystroke and tab switch.
2. **O(n*m) lookups**: `host_name()` and `project_name()` do linear scans through the hosts/projects vec for every session. `is_recent()` does a linear scan through recent_sessions for every session.
3. **Redundant builds**: `compute_grouped()` calls `build_project_items()` twice for Projects tab (line 451-453 builds all, then line 602 builds again). Items are rebuilt entirely on tab switch even though the underlying data hasn't changed.
4. **O(n) highlight lookup**: `render_highlighted_text()` uses `matched_indices.contains(&i)` which is O(indices_len) per character, making highlighting O(title_len * indices_len).
5. **Unnecessary allocations**: `format!()` for tab IDs, `String::new()` for empty subtitles, `Vec<char>` conversions in render path on every frame.

## Solution

### Phase 1: Pre-compute lookups in PaletteSnapshot (eliminates O(n*m) lookups)

**Files:** `command_palette.rs` (PaletteSnapshot)

Add `HashMap` caches built once at snapshot creation:
- `host_names: HashMap<String, String>` - host_id → hostname
- `project_names: HashMap<String, String>` - project_id → name
- `recent_set: HashSet<String>` - session IDs for O(1) is_recent check

Replace `host_name()`, `project_name()`, `is_recent()` with HashMap/HashSet lookups.

### Phase 2: Build items once, filter by reference (eliminates per-keystroke cloning)

**Files:** `command_palette.rs` (CommandPalette struct, recompute_results, compute_grouped, compute_scored)

- Build `all_session_items`, `all_project_items`, `all_action_items` once in `CommandPalette::new()` and store as fields.
- Change `PaletteResults` to hold indices into the pre-built vecs instead of cloned items.
- `recompute_results()` now only filters/sorts indices, never clones items.
- `compute_grouped()` partitions indices by category.
- `compute_scored()` runs fuzzy match on pre-built items, stores (index, FuzzyMatch) pairs sorted by score.

### Phase 3: Optimize render_highlighted_text (eliminates O(n*m) per item)

**Files:** `command_palette.rs` (render_highlighted_text)

- Convert `matched_indices` Vec to a `HashSet<usize>` (or use a sorted-vec binary search) before the render loop.
- Since matched_indices is already sorted from fuzzy matching, use a pointer/cursor approach: keep a `match_cursor` index, advance it when the current position matches.

### Phase 4: Reduce allocations in hot paths

**Files:** `command_palette.rs` (render_tab_bar, render_item_row, various)

- Use `SharedString::from("static")` for static tab IDs instead of `format!()`.
- Use `&str` / `SharedString` where possible instead of `String::clone()`.
- Avoid `to_string()` on static strings in footer/header rendering.

## Non-goals

- Real-time palette updates (data stays frozen at open time — this is correct behavior)
- Result list virtualization (item count is small enough, <100 typically)
- Debouncing keystrokes (GPUI re-render is fast enough once data work is eliminated)

## Implementation Order

Phase 1 → Phase 2 → Phase 3 → Phase 4 (each phase is independently valuable)

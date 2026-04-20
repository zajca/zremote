# UX Design — Git Diff Viewer + Review Comments

**Author:** ux-designer (rfc-git-diff-ui team)
**Status:** Draft — for team-lead review
**Target surface:** `zremote-gui` (GPUI desktop app)

## Scope recap

Read-only git diff viewer embedded in the ZRemote desktop app. Primary use
case: review of AI-generated code. User picks a diff source (working tree,
staged, commit, range, branch), reads the diff, leaves inline comments on
lines/ranges, and pushes the collected comments back to the owning agent
session as a structured message. Think "GitHub PR review, scoped to a local
checkout, routed back to the agent instead of a PR thread".

Out of scope for this RFC: writing to the worktree, resolving conflicts,
editing files, staging, committing, merging binary diffs, image diff.

## Context — existing UI surface

- Root layout: fixed 250px `SidebarView` + flex content area (`main_view.rs`).
- Sidebar groups: hosts -> projects -> worktrees -> sessions. Session rows
  show Claude Code state (bot icon, permission badge, context bar, model).
- Content area today only renders a single `TerminalPanel` or empty state.
- Command palette (`cmd+k`), session switcher, help modal, settings modal
  are all overlays stacked on the root.
- Projects are a first-class concept per-host (`project/mod.rs` +
  `scanner.rs`). Each project already owns git metadata via
  `GitInspector` (worktrees, branches, remotes). Git endpoints already
  exist per project on the agent.

This is the surface we extend — we are not introducing a new top-level
shell, just a new content-area mode and a few sidebar affordances.

---

## 1. Placement in the UI

### Options compared

**A) Dedicated view as a new content-area mode (per-project)**

```
┌──────────────┬─────────────────────────────────────────────┐
│  Sidebar     │  Breadcrumb: host / project / Diff          │
│              ├─────────────────────────────────────────────┤
│  host A      │                                             │
│   project X  │   [ Diff source picker ]                    │
│    [Diff]  <─┤                                             │
│    session 1 │   ┌─ files ─┐  ┌───── diff pane ─────────┐  │
│    session 2 │   │         │  │                         │  │
│   project Y  │   └─────────┘  └─────────────────────────┘  │
│              │                                             │
└──────────────┴─────────────────────────────────────────────┘
```

Pros: full-width canvas; natural home for file tree + diff; coexists with
terminal (user toggles between them); fits existing content-area pattern;
breadcrumb already in `main_view`.

Cons: switching from terminal to diff hides the terminal — review feedback
is most useful *while* the agent is running. Mitigated by (a) keeping
terminal tabs open in background, (b) keyboard shortcut to swap, (c) badge
on terminal session so user knows when to come back.

**B) Tab alongside terminal (split in content area)**

```
┌──────────────┬─────────────────────────────────────────────┐
│  Sidebar     │  [ Terminal ]  [ Diff ]    <-- tabs         │
│              ├─────────────────────────────────────────────┤
│              │                                             │
```

Pros: terminal and diff live side-by-side per session. Fast toggle.
Cons: Content area today is single-panel; adding tabs is a structural
change to `main_view`. Tabs are noisy when most sessions never open a
diff. Diff is per-project (not per-session) — pinning it to a terminal
tab creates a mismatch.

**C) Overlay / modal (like settings, help, command palette)**

Pros: zero layout changes; dismiss with Esc; always available.
Cons: explicitly rejected by the team lead; also the wrong primitive —
diff review is a sustained read-and-comment activity, not a
quick-action modal. Overlays also don't cohabit well with the terminal
the user is reviewing against.

### Recommended

**Option A — dedicated per-project view.** It is the right primitive for
a sustained, canvas-heavy activity, matches the existing sidebar→content
pattern, and keeps the diff decoupled from any single session (which
matches the data model: diffs are per-project, comments are routed to a
session as an outbound action).

### Entry points

1. **Sidebar row under each project** — new "Diff" item between the
   project header and the session list, rendered with a `GitBranch` icon
   (Lucide) and a badge. Indents at the same level as sessions.
2. **Keyboard shortcut** — `Cmd+D` (or `Ctrl+D` on Linux). Opens the diff
   view for the *currently active* project (derived below). If no
   project is active, focuses the sidebar project picker.
3. **Command palette** — new palette tab or global item "Open diff
   for <project>". Fuzzy-matches on project name.
4. **Session context menu** — right-click a session row: "Review
   changes". Opens diff for that session's owning project and, if the
   session emitted a review-ready signal, scrolls to the most recent
   commit.

"Active project" rule: if a terminal is open, its session's project
wins. Otherwise, the most recently interacted project wins. Otherwise,
the shortcut focuses the sidebar.

### Sidebar badge on sessions

When an agent signals "review ready" (see §3, "Agent-initiated review"),
the session row shows a small circular badge:

```
● session-abc    [3]    <-- teal dot + count of unread review hints
```

- Count = number of *unread* review hints from the agent (not user
  comments — user comments are local until sent).
- Colour: `theme::accent()` (teal). Falls back to `theme::success()` if
  accent collides with the CC bot colour.
- Tooltip: "3 files ready for review — click to open diff".
- Clicking the badge opens the diff view preloaded with the review
  range from the hint.

### Sidebar project-level badge

Project header shows a muted badge with *net* unreviewed changes across
all its sessions:

```
  myproject          [±42]
```

Smaller, always-present, inline with the project name. Shows hunk count
(added+removed) when there are uncommitted changes in the working tree
— a persistent ambient signal, not tied to agent events.

---

## 2. Diff view layout

### Overall frame

```
┌───────────────────────────────────────────────────────────────┐
│  [source picker]   [side-by-side | unified]   [refresh]  [x]  │  <- toolbar
├──────────┬────────────────────────────────────────────────────┤
│ files    │  path/to/file.rs                                   │  <- file header
│ ─────    ├────────────────────────────────────────────────────┤
│ [M] foo  │  ┌─ hunk 1 @@ 12,6 +12,8 @@ ─────────────────────┐ │
│ [A] bar  │  │                                                │ │
│ [D] baz  │  │  12   let x = 1;                               │ │
│ [M] qux  │  │  13 - let y = old();                           │ │
│  …       │  │  13 + let y = new();                           │ │
│          │  │  14   let z = x + y;                           │ │
│          │  └────────────────────────────────────────────────┘ │
│          │                                                    │
│          │  ┌─ hunk 2 @@ 80,3 +82,4 @@ ─────────────────────┐ │
│          │  │  …                                             │ │
│          │  └────────────────────────────────────────────────┘ │
├──────────┴────────────────────────────────────────────────────┤
│  Review drawer (collapsed)                 [2 pending]  [▲]   │  <- drawer
└───────────────────────────────────────────────────────────────┘
```

Layout parameters:
- File tree: min 180px, default 240px, max 400px — draggable splitter.
- Diff pane: fills remaining width.
- Review drawer: bottom, collapsed by default (32px), expands to 33% of
  viewport height when there are pending comments or on toggle.

### Source picker (toolbar)

Single-line, dense. First dropdown selects *kind* of diff, fields change
based on kind:

```
┌──────────────────────────────────────────────────────┐
│  [▼ Working tree]                                    │
│  [▼ Staged]                                          │
│  [▼ Commit]      [<sha / search>▼]                   │
│  [▼ Range]       [<from>▼] .. [<to>▼]                │
│  [▼ Branch vs]   [<base>▼]  (HEAD of current branch) │
│  [▼ HEAD~N]      [N: 1]                              │
└──────────────────────────────────────────────────────┘
```

- "Working tree" and "Staged" are the default pair — shown as a segmented
  toggle when no other source is selected:
  `[ Working tree | Staged ]`
- Commit/branch pickers are searchable (fuzzy on sha / message / ref
  name). Populated from the agent's git metadata — already exposed via
  `GitInspector::list_branches`. We'll need a new endpoint for
  commit log (see architecture RFC, §3).
- Recent selections pinned at top of the picker with a "Recent" header
  so repeated reviews are one-click.

### File tree (left)

- Flat list (not nested by directory) in v1 — most diffs are small and a
  nested tree adds visual noise. Leave room in the `FileListState` for
  optional tree mode later.
- Each row: status letter badge + path (ellipsised from the left so the
  filename is always visible) + additions/deletions counts.
  - `[M]` modified — `theme::warning()`
  - `[A]` added — `theme::success()`
  - `[D]` deleted — `theme::error()`
  - `[R]` renamed — `theme::accent()` + "old → new" tooltip
  - `[C]` copied — same colour as renamed
  - `[B]` binary — muted, not clickable for now
- Filter input at the top: "Filter files…" — fuzzy, case-insensitive,
  matches on path.
- Sticky footer row summarising totals:
  `4 files  +58 −21`.
- Selection syncs with the diff pane: clicking a file scrolls the diff
  to that file; scrolling the diff highlights the active file.

### Diff pane (right)

Toggle at the top right of the toolbar: `[ Side-by-side | Unified ]`.
Default: **side-by-side** for widths ≥ 1200px, **unified** below.
Remember user's choice per project in local persistence.

Side-by-side layout:
```
┌─────────────────────┬─────────────────────┐
│  before (old)       │  after (new)        │
│  12  let x = 1;     │  12  let x = 1;     │
│  13  let y = old(); │  13  let y = new(); │
│  14  let z = x + y; │  14  let z = x + y; │
└─────────────────────┴─────────────────────┘
```
- Gutter per side: line number + comment affordance (see §3).
- Mid-line word-level diff highlight (subtle bg). Use
  `similar::TextDiff` with word granularity.
- Syntax highlight via `tree-sitter` (already in workspace for terminal
  URL detection? — confirm; if not, `syntect` with bundled themes).
  Highlight must respect the GPUI theme's `syntax_*` tokens.
- Collapsed context: show `⋯ 24 unchanged lines` between hunks; click to
  expand ±10 lines or the full gap.

Unified layout: classic `-` / `+` gutter. Same gutter + comment
affordance on the left.

### Performance — large files

- Diffs over **2 000 rendered lines per file** render **collapsed by
  default**: file header shows, each hunk is a collapsed bar with its
  `@@` header + a "(142 lines)" count. User clicks to expand.
- Diffs over **50 000 lines per file** show a warning bar at the top:
  "This file is too large to diff interactively. Show anyway?" — the
  "show anyway" button reveals the hunks still in collapsed form.
- Minified / generated files (matched by `.min.`, lockfiles,
  `node_modules/`, `target/`) auto-collapse to a single "view raw diff"
  row.
- No virtualization in v1 — GPUI's `uniform_list` + per-hunk collapse is
  enough for realistic review workloads. Revisit if perf bites.

### Recommended

- **Side-by-side default on wide viewports, unified on narrow, remember
  per-project.** Consistent with how most diff tools (GitHub, GitLab,
  VS Code) behave; the default feels right but users can pin their
  preference.
- **Flat file list in v1.** Tree mode is a follow-up.
- **Collapse-per-hunk as the large-file strategy.** Chunked loading
  (virtualised rendering over a streaming endpoint) is a future
  optimisation; per-hunk collapse covers the 95% case without the
  plumbing cost.

### Empty / loading / error states

**Empty (no changes):**
```
    ┌─────────────────┐
    │    [ ⋯ icon ]   │
    │                 │
    │  Clean working  │
    │     tree.       │
    │                 │
    │ Switch source ↗ │
    └─────────────────┘
```
Centered, uses `Icon::GitCommit` or similar, action hint is a text
button that opens the source picker.

**Loading:** Skeleton rows in the file list (3 pulsing muted bars) plus
a small spinner next to the source picker. The toolbar and splitter
stay interactive — only the file list + diff pane blocked. **Zero
layout shift** when data arrives: file list keeps its width, first file
auto-selects.

**Error:** Inline card in the diff pane, not a toast. Red left border,
title (e.g. *"Couldn't load diff"*), message, `[ Retry ]` and
`[ Choose different source ]` buttons. Toast only for transient errors
(e.g. comment send failed) where the diff itself is fine.

### Recommended

- **Use `Icon` + message + single action hint** for empty and error
  states, centered. Never leave the diff pane silently empty.

---

## 3. Review flow

This is the new, non-obvious surface. It replaces the "create PR and
write inline comments" flow you'd use on GitHub.

### Gutter affordance

On hover over any diff line, the gutter shows a small `+` button
(themed, 14px):

```
  12   let x = 1;
  13 ⊕ let y = new();   <-- + appears on hover
  14   let z = x + y;
```

Clicking opens an inline comment composer directly below the line.
Shortcut: `C` on the focused line (when diff pane has focus) opens the
composer without needing the mouse.

### Multiline (range) comments

Click-drag on the gutter selects a range. Shift-click extends a range.
Range comments render as a left-side bracket spanning the rows:

```
  12   let x = 1;
  13 ┌ let y = new();
  14 │ let z = x + y;
  15 └ log(z);
         ┌──────────────────────────────┐
         │ comment composer              │
         └──────────────────────────────┘
```

### Inline comment composer

```
  15   log(z);
       ┌────────────────────────────────────────────────┐
       │  [ @author icon ]   you                        │
       │ ┌────────────────────────────────────────────┐ │
       │ │ type your comment…                         │ │
       │ └────────────────────────────────────────────┘ │
       │  [ markdown hints: ** _ ` ```  ]    [Cancel] [Save draft] │
       └────────────────────────────────────────────────┘
```
- Markdown-aware (reuse any markdown helper already in the GUI — if
  none exists, plain text is fine in v1, render as monospace on read).
- `Esc` cancels. `Cmd+Enter` saves as draft.
- Comment persists in local state (not sent to agent yet) — this is the
  "draft pending review" state.

### Rendering existing comments inline

```
  15   log(z);
       ┌────────────────────────────────────────────────┐
       │ you — draft                               [⋯]  │
       │ this should use `tracing::info!` instead of    │
       │ println                                        │
       │                                                │
       │ [Edit] [Delete]                                │
       └────────────────────────────────────────────────┘
```
- Draft: muted yellow left border (`theme::warning()` 30% opacity).
- Sent: teal left border (`theme::accent()`).
- Agent reply: different avatar, on the right side of the comment
  column in side-by-side mode.

### Pending-comments drawer (bottom)

Collapsed by default. Pill in the bottom-right of the viewport:
`[ 2 pending ▲ ]`. Expands to:

```
┌────────────────────────────────────────────────────────────────┐
│ Pending review — 2 comments                          [▼ Hide]  │
├────────────────────────────────────────────────────────────────┤
│  [x] foo.rs:13      "this should use tracing::info! instead…"  │
│  [x] bar.rs:42-48   "this block can be a single .map() …"      │
├────────────────────────────────────────────────────────────────┤
│  Target: [▼ session abc — my-agent-task]                       │
│  [Clear]                                [Send to agent]         │
└────────────────────────────────────────────────────────────────┘
```

- Checkbox per row lets user uncheck comments they don't want to send
  in this batch (comment stays as draft locally).
- "Target" dropdown — required. Lists all sessions that belong to the
  same project, with the most-recently-active session preselected. If
  only one session exists, it's locked in with a tooltip.
- Clicking a row scrolls the diff to that comment and flashes a halo.
- "Send to agent" is disabled when no row is checked or no session is
  selected.

### Recommended

- **Drafts local to GUI, explicit "Send to agent" per batch.** Don't
  auto-send — reviewers iterate on wording before shipping. Matches
  the GitHub "pending review" model users already know.
- **Bottom drawer for the pending list.** Right panel would crowd the
  diff when side-by-side is on; bottom respects the horizontal-first
  nature of diff review.
- **One target session per batch.** If the user wants to fan-out to
  multiple sessions, they re-open the drawer and send again.

### Payload shape (UX-visible contract)

Structured JSON sent to the agent session as a message. Exact transport
is the architecture team's call; from UX we commit to:

```json
{
  "type": "review_comments",
  "source": { "kind": "working_tree" },     // or commit/range/etc
  "project_path": "/abs/path/to/project",
  "comments": [
    {
      "path": "crates/foo/src/lib.rs",
      "side": "new",                         // "old" | "new"
      "line_start": 13,
      "line_end": 13,
      "body": "this should use tracing::info! instead of println"
    }
  ],
  "commit_sha": "optional — pinned commit for reproducibility"
}
```
- Always include both `path` and `commit_sha` (HEAD sha at the moment
  of sending for working-tree sources) so the agent can correlate even
  if the tree shifts underfoot.
- Always include `side`. Otherwise a comment on an "old" (deleted) line
  is ambiguous.

### Post-send behaviour

After "Send to agent" succeeds:
1. Drafts transition to **sent** state (teal border). They remain
   visible inline in the diff.
2. A toast confirms: *"Sent 2 comments to session abc"*.
3. The drawer clears (or shows only remaining unsent drafts).
4. Comments are persisted in the agent DB per project + per session
   so they survive GUI restart. Architecture RFC picks the
   table; from UX we require: same comment content + position must
   round-trip after app restart.

On send failure: inline error banner in the drawer; drafts stay as
drafts; retry button.

### Threading (agent replies)

V1 scope: **one-shot, no threads.** Agent receives comments as
context for its next turn; any "reply" is the agent writing code
changes, not commenting back.

V1.5 (follow-up, mention briefly in RFC but not blocking): agent
*can* reply via a protocol message and we render it as a child of the
original comment with the same visual treatment GitHub uses.

### Recommended

- **No threading in v1.** Send-to-agent is the payload; the agent's
  response is the next commit / terminal output, which the reviewer
  sees in the usual panels. Adding a full thread model in v1 muddles
  the contract.

---

## 4. Integration with the rest of the app

### Per-session badge (already touched in §1)

- Source of the "review ready" signal: agent-side hook. When a Claude
  Code turn completes with code changes, the agent emits a
  `ReviewReady { project_id, session_id, commit_sha_before,
  commit_sha_after, file_count }` event on the existing event WS
  (protocol team owns the exact event name).
- GUI listens in `MainView::start_event_polling` and forwards to the
  sidebar via a subscription, which increments an in-memory badge
  counter per session.
- Badge clears when user opens the diff view for that session *and*
  has scrolled past the last file.

### Notifications

- Use the existing toast + native notification stack
  (`NativeUrgency`, `ToastContainer`).
- On `ReviewReady`, show a toast:
  *"session abc has 3 files ready for review"* with a
  `[ Open diff ]` action. Action opens the diff view for that session.
- If the OS window is unfocused, elevate to a native notification at
  `NativeUrgency::Normal` (not `Critical` — this is informational,
  not an input-required block).
- Suppress if the user is already in the diff view for that session.

### Determining the "active project"

Already covered in §1 entry points. Formal rule:
1. If a terminal is open and has a known session, use that session's
   project.
2. Else, the most recently opened project (persisted).
3. Else, prompt the user to pick from the sidebar.

### Agent disconnection / Server mode

- If the agent that owns the project is **disconnected**, the diff view
  shows a muted error card in the diff pane:
  *"Agent is disconnected — diff is cached from [timestamp]. Reconnect
  to refresh."* Existing drafts stay interactable; "Send to agent" is
  disabled with a tooltip.
- Last successfully loaded diff is held in memory (not persisted across
  GUI restart in v1 — cache across restarts is a follow-up). This
  means reopening the view after a reconnect triggers a fresh load.

### Recommended

- **Badge + toast + native notification, gated by window focus.** Three
  channels, each targets a different attention state (glancing at
  sidebar, working in-app, away from app).

---

## 5. States and edge cases

| Case | UX response |
|---|---|
| **Repo with no commits yet** | Source picker disables commit/range/branch options; working-tree is the only selectable source. Empty state if working tree itself is clean. |
| **Detached HEAD** | Breadcrumb shows `(detached @ abc1234)` next to the project name. "Branch vs" selector is disabled with tooltip: *"Current HEAD isn't on a branch."* Ranges and commits still work. |
| **Merge conflict zones** | Render as a normal diff but the file header shows a red `[CONFLICT]` badge. Conflict markers (`<<<<<<<`) are highlighted in `theme::error()` with a dedicated left-border. Comments allowed but a subtle hint: *"this file has conflict markers — resolve in your editor first."* |
| **Binary files** | Show a file row in the list with `[B]` badge. Diff pane shows a card: *"Binary file — cannot diff."* with file size info, no hunks, no comment affordance. |
| **Submodule changes** | Surface as a single-line pseudo-hunk: *"submodule foo: abc → def"*. Clickable if the submodule is itself a known project; opens that project's diff view. |
| **File > threshold** | Collapse per §2. If file is also binary-like (LF-less, huge line), force binary-style card. |
| **Disconnected agent, Server mode** | See §4. |
| **Stale diff vs on-disk** | Polling: every 10s while the diff view is visible, the agent checks HEAD sha + working-tree hash. If changed, a yellow banner appears at the top of the diff pane: *"Working tree changed — [Refresh]"*. The banner does not auto-refresh (user might be mid-comment); clicking refresh rebuilds the diff, preserving *comments whose anchor lines still exist on the same side*. Comments on lines that no longer exist are parked in a "stale" section of the drawer with a warning icon and the original excerpt for context. |
| **Comments on deleted lines after refresh** | They move to the "stale" group. User can either (a) delete them, (b) re-anchor manually by clicking a new line ("move here"), or (c) send them anyway — the payload will include the original path + line range + the user's original comment body, clearly flagged `"stale": true`. |
| **Working tree with 0 staged + 0 unstaged** | Empty state as per §2. |
| **Switching projects mid-review** | Drafts per project persist until sent or explicitly cleared. Switching projects hides drafts for the other project but doesn't drop them. |
| **Agent renames the project** | Project ID is stable, display name updates. No UX impact. |
| **User closes app with unsent drafts** | Drafts persist in local GUI state (the same layer as window size / settings). On next launch they reappear in the drawer. This is important because "send to agent" is a deliberate action — drafts must survive sessions. |
| **Very long single comment** | Render with a `max-height: 200px` and a "show more" toggle. Editor itself is unbounded. |
| **Emoji / non-ASCII in comments** | Fully supported (GPUI text layer handles it). No UX work needed. |

### Recommended

- **Refresh-on-change is user-initiated, not automatic.** Auto-refresh
  would discard in-progress comment drafts. Banner-and-button is the
  same pattern VS Code uses for external file changes, which users
  already understand.
- **Stale comments are kept, not deleted.** Losing a thoughtful comment
  because a file shifted is worse than showing a stale warning.

---

## Appendix A — Keyboard shortcuts

| Shortcut | Action |
|---|---|
| `Cmd+D` (global) | Open diff for active project |
| `Cmd+Shift+D` (global) | Open diff + focus source picker |
| `C` (diff pane focused, line selected) | Open comment composer |
| `Esc` (composer) | Cancel / close |
| `Cmd+Enter` (composer) | Save as draft |
| `Cmd+K` → "Open diff …" | Palette entry |
| `Cmd+R` (diff view focused) | Refresh diff |
| `Cmd+Shift+Enter` (drawer focused) | Send to agent |
| `J / K` (diff pane focused) | Next / previous hunk |
| `N / P` (diff pane focused) | Next / previous file |

Global shortcuts are registered via the existing
`KeyAction` + `dispatch_global_key` mechanism (see
`main_view.rs`).

## Appendix B — Theme tokens used

All colours flow through `theme::*()` — no new tokens proposed, the
current palette covers every state above. Icons used: `GitBranch`,
`GitCommit`, `FileDiff` (add Lucide SVG under `assets/icons/`),
`MessageSquare` (for comments), `Send` (for send-to-agent), existing
`X`, `ChevronDown`, `Filter`.

## Appendix C — Out-of-scope / v2 candidates

- Nested file tree.
- Virtualised rendering for huge diffs (streaming).
- Agent replies as threads.
- Comment @-mentions across sessions / users.
- Word-level blame sidebar.
- Side-by-side image/binary diff.
- Persistent comment history across sends (comments as a per-project
  log, not just "pending / sent").
- Cross-project review queue ("all my pending reviews").

# RFC: Git Diff UI with Agent-Routed Review Comments

**Status:** Draft — awaiting user approval
**Author:** team-lead (rfc-git-diff-ui team)
**Date:** 2026-04-20
**Sources:**
- `docs/rfc/research-git-diff-ui.md` (researcher — Okena/Arbor patterns, Rust libs)
- `docs/rfc/ux-git-diff-ui.md` (ux-designer — layout, review flow, edge cases)
- `docs/rfc/arch-git-diff-ui.md` (architect — protocol, endpoints, GPUI structure)

---

## 1. Summary

Add a read-only git diff viewer to the GPUI desktop client, with the ability to
leave inline review comments and ship them to a ZRemote agent session as a
structured prompt. Primary use case: reviewing AI-generated code from a Claude
Code session, pushing feedback back into the same session (or a new one).

Works across all three modes (Standalone, Local, Server). In Server mode the
diff is computed on the remote agent and streamed through the central server
to the GUI.

## 2. Goals / Non-Goals

**Goals**
- Read-only diff viewer: working tree, staged, HEAD vs ref, range (`a..b` /
  `a...b`), single commit.
- Side-by-side and unified rendering with per-project persisted preference.
- Syntax highlighting via `syntect` (consistent with Okena and Arbor; no per-
  language crate management).
- Inline review comments (single line + range) with a local draft model. User
  explicitly batches and sends; one session target per batch.
- Session badge when the agent signals "review ready".
- Works in all three ZRemote modes; data model identical, only transport differs.

**Non-goals for v1**
- Mutations (stage, unstage, discard, commit, amend, push, pull).
- Merge conflict resolution.
- Image / binary diff (shown as badge only).
- Threaded agent replies (noted as v1.5).
- Word-level intra-line diff (deferred to P2 — use `imara-diff` client-side).
- Nested directory tree for file list (flat list in v1).

## 3. Architecture overview

```
Standalone / Local:
  GPUI (DiffView)
    ─> zremote-client::stream_diff
       ─> POST /api/projects/:id/diff   (NDJSON stream)
          ─> agent::project::diff::run_diff_streaming
             ─> shell-out `git diff ...`, `git show`, `git status --porcelain=v2`
             ─> parse unified diff → DiffEvent per file
             ─> sink(event) → tokio mpsc → HTTP body

Server:
  GPUI ─> client ─> POST server:/api/projects/:id/diff
                    ─> server::diff_dispatch registers (request_id → mpsc)
                    ─> sends ServerMessage::ProjectDiff over WS
                       ─> agent emits AgentMessage::DiffStarted /
                          DiffFileChunk / DiffFinished
                          ─> server forwards to mpsc ─> NDJSON body to GUI
```

Data model is shared; only transport framing differs (NDJSON lines vs. tagged
`AgentMessage` variants).

## 4. Key decisions (resolving research conflicts)

These are the non-obvious calls where the three reports diverged. Decided here
so the implementation phase doesn't re-litigate.

### 4.0 Follow existing standards — do not invent wire formats

The three reports drafted a bespoke schema. That is wrong: every Git review
tool (GitHub, GitLab, Forgejo / Gitea, Gerrit) already solves these two
problems and their schemas have converged. We align to them.

- **Diff content:** unified diff as produced by `git diff` is already the
  standard. We parse it into structured chunks (§4.2) using field names and
  semantics matching the git plumbing vocabulary (hunk `old_start`,
  `old_lines`, `new_start`, `new_lines`; line `kind` in
  `context|added|removed|no_newline`). No renaming to our own vocabulary.

- **Review comment schema:** modelled on the **GitHub PR review comment
  API** (which Gitea, Forgejo, and GitLab all mimic for compatibility):
  - `path` (relative, forward slashes) — not `file_path`.
  - `side`: `"left"` | `"right"` — LEFT = pre-image (removed / old), RIGHT =
    post-image (added / context / new). Matches GitHub exactly.
  - `line`: 1-based line number on the given `side`. For a single-line
    comment, only `line` is set.
  - `start_line` + `start_side`: optional, set for multi-line comments. End
    of the range is `line` + `side`. This matches GitHub's multi-line
    review comments 1:1.
  - `commit_id`: SHA the comment is anchored to. Required — makes a comment
    replayable against a reload of the same commit and lets future PR-import
    features round-trip without a custom mapping.
  - `body`: markdown.

- **Import / export path:** because our schema is a strict subset of the
  GitHub one, a future "import PR comments" or "export review to
  GitHub/Gitea" feature is a field rename only, not a redesign.

This deprecates the `LineRange { start, end }` + `ReviewSide::{Old, New}`
shapes from the architect draft. The updated shape is shown in §5.

### 4.1 Git backend: **shell-out, not `git2`**

Researcher's recommendation wins. Rationale:

- `crates/zremote-agent/src/project/git.rs` already has a hardened `run_git`
  with 5s timeout, `GIT_CEILING_DIRECTORIES`, and disabled credential prompts.
  Extending it costs zero new dependencies.
- Adding `git2` pulls in `libgit2-sys` (C), hurts cross-platform builds
  (macOS universal, Linux musl for Server), and adds ~2 MB to every binary.
- Unified diff output is a stable, textual spec. Parsing it deterministically
  (Okena-style state machine, ~250 lines) is well-understood; there is a path
  to swap in `gix-diff` later if we need structured output.
- Remote case: `git` is practically always present on agent hosts; agent is
  already shelling out to `git` for all other metadata.

Trade-off: we own a unified-diff parser. Mitigated by cloning Okena's parser
layout under `zremote-agent/src/project/diff_parser.rs` with the same unit
tests (insertions, deletions, renames, binary markers, "no newline at EOF").
Integration tests run against `tempfile` repos built with realistic commits
(matching the shape of `project/git.rs` tests).

**Escape hatch:** if an edge case (submodule diff, sparse checkout) reveals a
parser gap, feature-flag a `gix-diff` backend behind
`ZREMOTE_DIFF_BACKEND=gix`. Protocol types stay unchanged.

### 4.2 Diff data wire: **unified diff parsed on agent → structured chunks**

Not raw unified diff text across the wire, not full blobs. Agent shells out
for unified diff, parses it into `DiffFile { summary, hunks: [DiffHunk {
lines: [DiffLine] }] }`, and sends the structured shape. Reasons:

- GUI must reason about line numbers, hunk boundaries, and comment anchors —
  giving it raw text means every consumer re-parses.
- Blobs-only (variant B in researcher's report) would balloon payloads for
  large files and push algorithm choice to the GUI; rejected for v1.
- Structured shape is the same shape Okena's `FileDiff` uses; we re-use the
  pattern where it already works.

For word-level diff inside a modified line (P2), the agent will additionally
send the two single-line strings as they appear in the hunk — the GUI can run
`imara-diff` on the word level client-side without the full blob.

### 4.3 Syntax highlighting: **`syntect`** with caps

- Both Okena and Arbor ship `syntect`. It is the known-working option for
  diff rendering in a GPUI app.
- Pre-highlight the **full file** (old + new content) for any file the user
  opens, to keep syntect's state machine correct across multi-line constructs
  (JSX, template literals, block comments).
- Cache highlight output keyed by `(blob_sha, syntax_name)` so scroll and
  view-mode toggle do not re-highlight.
- Skip highlighting for files > 1 MB or > 10 000 lines — render diff-only
  colouring with plain text.
- Themed via mapping syntect token → `theme::*()` entries. No bundled syntect
  themes in product (stylistic mismatch with ZRemote).

### 4.4 Placement: **dedicated per-project view** (UX Option A)

Full content-area view, not a tab next to the terminal. Replaces the current
single-panel `MainView::terminal` slot with an enum:

```rust
pub enum MainContent {
    Terminal(Entity<TerminalPanel>),
    Diff(Entity<DiffView>),
}
```

Entry points: sidebar "Diff" row under each project, `Cmd+Shift+D`, command
palette, session context menu ("Review changes"). Session row badge when the
agent signals review-ready.

Split-pane (terminal beside diff) is explicitly deferred to P6.

### 4.5 Streaming shape: **chunked per-file, no file-level pagination**

Local: NDJSON over HTTP. Server: `AgentMessage::DiffStarted` →
`DiffFileChunk` * N → `DiffFinished`, shuttled through a server-side
`DiffDispatch` to an NDJSON body.

Agent sends a `DiffStarted` summary list immediately (cheap — `git diff
--stat` or the parsed equivalent). Per-file hunks follow as separate chunks
so a slow file does not block earlier files from rendering. The GUI renders
incrementally: file tree populates on `DiffStarted`, each file becomes
clickable when its chunk arrives. Files too large trip a `too_large=true`
flag and the chunk ships an empty `hunks` vector.

Cancellation: client drops the response → local mode closes HTTP, server mode
sends `ServerMessage::DiffCancel { request_id }`, agent checks a token
between files and breaks out of the worker loop.

### 4.6 Review drafts: **persisted in GUI local state, not agent DB**

- Drafts are single-user, single-device, short-to-medium-lived (minutes to a
  day). Persisting to agent DB requires a round-trip in Server mode on every
  keystroke, which is excessive.
- Persist drafts via the existing `crates/zremote-gui/src/persistence.rs`
  layer (same layer window size / theme live in). Key by `(host_id,
  project_id)`.
- UX contract requires drafts to survive GUI restart (see UX §5). Pure
  in-memory would violate that.
- On "Send to agent": comments are rendered into a prompt and written via
  `ReviewDelivery::InjectSession` (or `StartClaudeTask` if no session
  selected). The drafts are then marked `sent` and kept for visual reference
  inline until the user clears them.

### 4.7 Review delivery: **PTY inject for MVP**, start-new-session as P2

`ReviewDelivery::InjectSession` writes rendered markdown into an existing
Claude Code session's PTY stdin via the existing `session_manager.write_to`
path (analogous to `ContextPush` in `zremote-protocol/src/terminal.rs`). This
piggybacks on the already-audited channel.

`ReviewDelivery::StartClaudeTask` will spawn a fresh session with the rendered
prompt as initial input (P2). `ReviewDelivery::McpTool` variant reserved in
the protocol so wire compat survives when we add it.

**CSI injection guard:** before writing to PTY, strip `\x1b[` sequences and
other control characters (keep `\n`, `\t`) from each comment body. Test in
agent-side integration test.

## 5. Protocol additions (`zremote-protocol`)

New modules:

- `crates/zremote-protocol/src/project/diff.rs` — `DiffSource`, `DiffRequest`,
  `DiffFile`, `DiffFileSummary`, `DiffHunk`, `DiffLine`, `DiffSourceOptions`,
  `RecentCommit`, `DiffError`, `DiffErrorCode`.
- `crates/zremote-protocol/src/project/review.rs` — `ReviewComment`,
  `ReviewSide`, `ReviewDelivery`, `SendReviewRequest`,
  `SendReviewResponse`. Schema mirrors GitHub PR review comment API (§4.0).

Representative shapes (full definitions in `docs/rfc/arch-git-diff-ui.md`
§1.2–1.6):

```rust
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffSource {
    WorkingTree,
    Staged,
    WorkingTreeVsHead,
    HeadVs { #[serde(rename = "ref")] reference: String },
    Range { from: String, to: String, #[serde(default)] symmetric: bool },
    Commit { sha: String },
}

pub struct DiffRequest {
    pub project_id: String,
    pub source: DiffSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_paths: Option<Vec<String>>,
    #[serde(default = "default_context_lines")]
    pub context_lines: u32,
}

pub struct DiffFileSummary {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: DiffFileStatus, // Added|Modified|Deleted|Renamed|Copied|TypeChanged
    #[serde(default)] pub binary: bool,
    #[serde(default)] pub submodule: bool,
    #[serde(default)] pub too_large: bool,
    pub additions: u32,
    pub deletions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_sha: Option<String>,
}

// Review comment — GitHub PR comment schema (§4.0).
// Compatible with Gitea / Forgejo / GitLab variants (field rename only).

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReviewSide {
    /// Pre-image (deleted / old line).
    Left,
    /// Post-image (added / context / new line).
    Right,
}

pub struct ReviewComment {
    pub id: Uuid,
    pub path: String,                    // relative, forward slashes
    pub commit_id: String,               // SHA the comment is anchored to
    pub side: ReviewSide,                // "left" | "right"
    pub line: u32,                       // 1-based end of range (or single line)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_side: Option<ReviewSide>,  // multi-line: start side
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,         // multi-line: start line (1-based)
    pub body: String,                    // markdown
    pub created_at: DateTime<Utc>,
}
```

Mapping to GitHub's API: `id` is local UUID (GitHub returns i64); everything
else is a field-for-field match including snake_case and the `left` / `right`
side encoding. `commit_id` is always populated so a review survives refresh
of the same commit and a future PR-export feature has no custom mapping to
invent.

WS tunnel additions in `crates/zremote-protocol/src/terminal.rs`
(`#[serde(tag = "type")]` tagged): `ServerMessage::ProjectDiff`,
`ProjectDiffSources`, `ProjectSendReview`, `DiffCancel`; `AgentMessage::
DiffStarted`, `DiffFileChunk`, `DiffFinished`, `DiffSourcesResult`,
`SendReviewResult`.

**Compat rules:** every new field is `Option<T>` + `#[serde(default)]`.
Backward-compat test in P0: deserialise old payloads without the new fields.

**Capability advertisement:** agent's `Register` message grows a
`supports_diff: bool` (opt-in default false). Server returns 501 with a
friendly message if an older agent is targeted for a diff request — never
silently hangs.

## 6. Agent — git layer + endpoints

### 6.1 Git layer

New module `crates/zremote-agent/src/project/diff.rs`:

```rust
pub enum DiffEvent {
    Started { files: Vec<DiffFileSummary> },
    File { file_index: u32, file: DiffFile },
    Finished { error: Option<DiffError> },
}

pub fn run_diff_streaming<F>(
    project_path: &Path,
    req: &DiffRequest,
    sink: F,
) -> Result<(), DiffError>
where F: FnMut(&DiffEvent) -> std::io::Result<()>;

pub fn list_diff_sources(project_path: &Path, max_commits: usize)
    -> Result<DiffSourceOptions, DiffError>;
```

Implementation shells out (§4.1). Mapping `DiffSource` → git arguments:

| `DiffSource` | shell |
|---|---|
| `WorkingTree` | `git diff --no-color --no-ext-diff -U<N> -- <paths>` |
| `Staged` | `git diff --cached --no-color --no-ext-diff -U<N> -- <paths>` |
| `WorkingTreeVsHead` | `git diff HEAD --no-color --no-ext-diff -U<N> -- <paths>` |
| `HeadVs { reference }` | `git diff --no-color --no-ext-diff -U<N> <reference>..HEAD -- <paths>` |
| `Range { from, to, false }` | `git diff --no-color --no-ext-diff -U<N> <from>..<to> -- <paths>` |
| `Range { from, to, true }` | `git diff --no-color --no-ext-diff -U<N> <from>...<to> -- <paths>` |
| `Commit { sha }` | `git diff --no-color --no-ext-diff -U<N> <sha>^ <sha> -- <paths>` (fall back to `git show --format= ...` for root commits) |

Plus `git ls-files --others --exclude-standard` for untracked files in
`WorkingTree` / `WorkingTreeVsHead` sources (Okena pattern) — each untracked
file becomes a synthetic `DiffFile` with all lines `Added`.

**Security validation** (before invoking git):

- `validate_git_ref(&str)` — reject refs starting with `-` (flag injection),
  refs containing `..`, `\n`, NUL. Borrow Okena's implementation.
- `file_paths` capped at 1000 entries per request; each path validated
  against path traversal (reuse existing `validate_path_no_traversal` from
  the agent).
- `project_id → path` already goes through `get_project_host_and_path`.

Limits (hard-coded, consts in `diff.rs`):

- `MAX_FILE_BYTES = 512 * 1024` — bigger files ship `too_large=true` with
  empty hunks.
- `MAX_TOTAL_FILES = 2000` — requests with more files return `DiffError`.
- `MAX_CONTEXT_LINES = 20`.
- `DIFF_TIMEOUT = 30s` wall-clock.

### 6.2 Local REST

In `crates/zremote-agent/src/local/routes/projects/`:

```
POST /api/projects/:id/diff            -> NDJSON stream of DiffEvent
GET  /api/projects/:id/diff/sources    -> DiffSourceOptions (JSON)
POST /api/projects/:id/review/send     -> SendReviewResponse (JSON)
```

NDJSON body: `axum::body::Body::from_stream` fed by a `tokio::sync::mpsc`
channel written from the shell-out worker (spawned via `spawn_blocking`).
`try_send` failure (client gone) aborts the worker.

### 6.3 `render_review_prompt`

Module `crates/zremote-agent/src/project/review.rs`. Input:
`SendReviewRequest`. Output: plain markdown string grouped by file:

```
<preamble, if any>

## Code review comments

Diff source: working tree

### `crates/foo/src/lib.rs`

- L13 (new): this should use `tracing::info!` instead of println
- L42-48 (new): this block can be a single `.map()`
```

CSI-stripped per §4.7. For PTY `InjectSession` the output is terminated with
`\n` to submit.

## 7. Server — dispatch

New file `crates/zremote-server/src/routes/projects/diff.rs` mirrors local
endpoints, plus a new dispatch helper:

```rust
pub struct DiffDispatch {
    inner: Arc<RwLock<HashMap<Uuid, mpsc::Sender<DiffEvent>>>>,
}

impl DiffDispatch {
    pub fn register(&self, request_id: Uuid, tx: mpsc::Sender<DiffEvent>);
    pub async fn forward(&self, request_id: Uuid, event: DiffEvent);
    pub async fn finish(&self, request_id: Uuid, error: Option<DiffError>);
    pub fn unregister(&self, request_id: Uuid);
}
```

`AppState` grows `pub diff_dispatch: Arc<DiffDispatch>`. Match arms added in
`crates/zremote-server/src/routes/agents/dispatch.rs` for each new
`AgentMessage::Diff*` variant. REST handler creates `request_id`, registers
the `tx`, sends `ServerMessage::ProjectDiff` to the agent, and returns a body
built from `ReceiverStream` wrapped as NDJSON. On body drop, the handler
fires `ServerMessage::DiffCancel` to the agent.

## 8. GUI — view structure

### 8.1 Module layout

```
crates/zremote-gui/src/views/diff/
    mod.rs           // DiffView + MainContent hookup
    state.rs         // DiffState reducer
    source_picker.rs // working | staged | HEAD vs. | commit | range | N prev
    file_tree.rs     // flat file list (left pane)
    diff_pane.rs     // unified + side-by-side (center)
    review_panel.rs  // bottom drawer + send button
    review_comment.rs// inline comment marker on a line
    highlight.rs     // syntect bridge with cache
    large_file.rs    // collapsed-hunk rendering
```

### 8.2 `DiffView` type

```rust
pub struct DiffView {
    app_state: Arc<AppState>,
    project_id: String,
    focus_handle: FocusHandle,
    source_picker: Entity<SourcePicker>,
    file_tree: Entity<FileTree>,
    diff_pane: Entity<DiffPane>,
    review_panel: Entity<ReviewPanel>,
    // Owned async tasks — dropped-on-drop (per CLAUDE.md convention).
    _loader: Option<Task<()>>,
    _highlighter: Option<Task<()>>,
    _review_sender: Option<Task<()>>,
    state: DiffState,
}
```

`state` holds `DiffSourceOptions`, files list, per-file loaded diffs,
selected file, view mode, drafts. Reducer functions handle each
`DiffEvent`.

**Render decomposition** (CLAUDE.md rule): `render()` composes
`render_header`, `render_body` (horizontal stack of file_tree + diff_pane),
`render_review_drawer`, with `render_loading` / `render_empty_state` /
`render_error` as conditional branches.

### 8.3 MainView integration

`main_view.rs` replaces `terminal: Option<Entity<TerminalPanel>>` with:

```rust
pub enum MainContent {
    Terminal(Entity<TerminalPanel>),
    Diff(Entity<DiffView>),
}
```

Transition actions:
- `open_diff(project_id)` — switches content to `Diff`, preserves terminal
  entity in an in-app cache so coming back retains state.
- `open_terminal(session_id)` — reverse.

Sidebar adds a diff row per project; session rows carry a review badge.

### 8.4 Syntax highlight bridge

```rust
// highlight.rs
pub struct HighlightEngine { ... }  // wraps syntect SyntaxSet + Theme (lazy)
impl HighlightEngine {
    pub fn global() -> &'static Self;
    pub fn detect_syntax(&self, path: &str) -> &SyntaxReference;
    pub fn highlight_file(&self, text: &str, syntax: &SyntaxReference)
        -> Vec<Vec<(HighlightStyle, Range<usize>)>>; // per-line spans
}
```

Highlight runs in `cx.background_spawn` owned by `_highlighter: Option<Task<()>>`
on the `DiffView`. Results stored in a `HashMap<(blob_sha, String), ...>` cache
keyed so scroll is free and view-mode toggle is free. Cache cleared on
project switch.

Syntect token → GPUI `HighlightStyle` mapping lives in `highlight.rs` using
`theme::*()` colours. Theme token bundled in product repo, not loaded from
syntect defaults.

### 8.5 Large file strategy

- Files > 2000 rendered lines: hunks render collapsed ("142 lines"); click
  expands a single hunk. Expander rows (Okena pattern) for context.
- Files > 50 000 lines: warning banner, all hunks collapsed, `[Show anyway]`.
- Binary / submodule: card in the diff pane, no hunks, no comment gutter.
- `uniform_list` virtualises the diff pane so rendered-in-viewport cost is
  bounded regardless of file size.

## 9. Review flow — UX contract → impl mapping

### 9.1 Comment placement

- Hover gutter → `+` icon. Click opens inline `CommentComposer` below the
  line. Shortcut `C` on focused line.
- Click-drag on gutter selects a range; `ReviewComment.start_line` +
  `ReviewComment.line` capture the range, `start_side` + `side` capture the
  endpoints. Side is inferred from which gutter was clicked (side-by-side)
  or from the line's `DiffLineKind` (unified: removed → `left`, added /
  context → `right`).

### 9.2 Draft persistence

- All drafts serialised into GUI persistence under key
  `diff_drafts:<host_id>:<project_id>`. Structure:
  `Vec<ReviewComment>` (protocol type re-used).
- Persist on add/edit/delete with a 500 ms debounce.
- Load on `DiffView` mount; merge with live drafts by `ReviewComment.id`.

### 9.3 Send to agent

Drawer pill `[N pending ▲]`. Expanded drawer:

```
┌───────────────────────────────────────────────────────────┐
│ Pending review — 2 comments                        [▼]    │
├───────────────────────────────────────────────────────────┤
│ [x] foo.rs:13       "use tracing::info! instead of …"     │
│ [x] bar.rs:42-48    "can be a single .map() …"            │
├───────────────────────────────────────────────────────────┤
│ Target: [▼ session abc — my-task]                         │
│ [Clear]                              [Send to agent] ─┐   │
└───────────────────────────────────────────────────────┘   │
```

- Target dropdown: sessions whose `working_dir` matches the project. Default:
  most recently active.
- `[Send to agent]` builds `SendReviewRequest { delivery: InjectSession,
  session_id: Some(sid) }`, posts to `/api/projects/:id/review/send`.
- On success: drafts transition to `sent` state (render visual), toast
  confirms, drawer collapses.
- On failure: inline error banner in drawer; drafts remain drafts; Retry.

### 9.4 Review-ready signal

Agent emits `ServerEvent::ReviewReady { project_id, session_id,
commit_sha_before, commit_sha_after, file_count }` on the existing event
channel. GUI subscription increments a per-session badge; toast with `[Open
diff]` action when window is focused; native notification at
`NativeUrgency::Normal` otherwise. Badge clears when the user opens the diff
and scrolls past the last file.

### 9.5 Stale diff handling

- Poll HEAD + worktree hash every 10 s while `DiffView` is visible.
- On change: yellow banner `Working tree changed — [Refresh]`. No auto-refresh
  (preserves in-flight drafts).
- Refresh: re-request, re-anchor comments by `(path, side, line)` on the new
  lines. Comments whose anchor line no longer exists move to a "stale" group
  in the drawer with the original excerpt preserved.

## 10. Phases

Each phase is self-contained and mergeable. Tests listed are minimum bar.

### Phase 0 — Protocol + capability

**CREATE**
- `crates/zremote-protocol/src/project/diff.rs`
- `crates/zremote-protocol/src/project/review.rs`

**MODIFY**
- `crates/zremote-protocol/src/project/mod.rs` — re-exports
- `crates/zremote-protocol/src/terminal.rs` — `ServerMessage::Project{Diff,
  DiffSources,SendReview,DiffCancel}`, `AgentMessage::Diff*`,
  `SendReviewResult`, `DiffSourcesResult`; `Register.supports_diff: bool`

**Tests**
- Serde roundtrip per new type, per `DiffSource` variant.
- Back-compat: decode synthetic legacy payload without new fields.
- Default-value tests (`context_lines`, `symmetric`, `supports_diff`).

**Exit:** `cargo test -p zremote-protocol` clean. Workspace still builds.

### Phase 1 — Agent diff layer + local REST

**CREATE**
- `crates/zremote-agent/src/project/diff.rs` (shell-out runner, limits)
- `crates/zremote-agent/src/project/diff_parser.rs` (Okena-style state machine)
- `crates/zremote-agent/src/project/review.rs` (`render_review_prompt`, CSI strip)
- `crates/zremote-agent/src/local/routes/projects/diff.rs` (NDJSON endpoint,
  sources endpoint, send-review endpoint)

**MODIFY**
- `crates/zremote-agent/src/project/mod.rs` — `pub mod diff; pub mod review; pub mod diff_parser;`
- `crates/zremote-agent/src/local/routes/projects/mod.rs`
- `crates/zremote-agent/src/local/router.rs` — register 3 routes
- `crates/zremote-agent/src/project/git.rs` — add `validate_git_ref` +
  `list_recent_commits(path, n)` + `list_branches` refactor to return
  existing `BranchList`

**Tests**
- Per-variant `run_diff_streaming` tests (tempfile repos with a helper
  mirroring the existing `init_git_repo`). Cover: modified, added, deleted,
  renamed, untracked, binary, deleted+untracked, empty repo, detached HEAD,
  root-commit diff, CRLF line endings.
- Parser unit tests: hunks, renames, binary markers, "no newline at EOF",
  malformed tail.
- `render_review_prompt`: multi-file grouping, range formatting, CSI strip
  (assert `\x1b[31m` does not survive).
- Integration: axum `TestClient` `POST /api/projects/:id/diff` → assert
  NDJSON lines + DiffFinished.
- Abort test: `sink` returns `BrokenPipe` → worker exits between files.

**Exit:** `curl localhost:3000/api/projects/:id/diff` in local mode returns a
valid NDJSON stream end-to-end.

### Phase 2 — Server dispatch + client SDK

**CREATE**
- `crates/zremote-server/src/diff_dispatch.rs`
- `crates/zremote-server/src/routes/projects/diff.rs`
- `crates/zremote-client/src/diff.rs`

**MODIFY**
- `crates/zremote-server/src/state.rs` — `pub diff_dispatch: Arc<DiffDispatch>`
- `crates/zremote-server/src/routes/agents/dispatch.rs` — 5 new match arms
- `crates/zremote-server/src/routes/projects/mod.rs` — route registration
- `crates/zremote-agent/src/connection/dispatch.rs` — handle
  `ServerMessage::ProjectDiff / ProjectDiffSources / ProjectSendReview /
  DiffCancel`
- `crates/zremote-client/src/lib.rs` — re-exports

**Tests**
- `DiffDispatch` unit: register → forward N → finish → unregister; dropped
  receiver causes subsequent forwards to be discarded (not errored).
- Agent dispatch: pattern-matched test like
  `worktree_create_threads_base_ref_through_dispatch` — dispatch a
  `ProjectDiff`, collect emitted `AgentMessage`s, assert order and payload.
- Cancel test: server REST drops → `DiffCancel` emitted.
- Capability test: agent with `supports_diff=false` → REST 501.

**Exit:** end-to-end integration test spinning server + agent processes: GUI
client receives full DiffStarted → chunks → Finished sequence.

### Phase 3 — GUI MVP (unified, no highlight, no review)

**CREATE**
- `crates/zremote-gui/src/views/diff/{mod.rs, state.rs, source_picker.rs,
  file_tree.rs, diff_pane.rs, large_file.rs}`

**MODIFY**
- `crates/zremote-gui/src/views/mod.rs` — `pub mod diff;`
- `crates/zremote-gui/src/views/main_view.rs` — `MainContent` enum, open/close
  transitions, badge forwarding
- `crates/zremote-gui/src/views/sidebar.rs` — diff row under each project,
  session-level review badge
- `crates/zremote-gui/src/views/key_bindings.rs` — `Cmd+Shift+D`
- `crates/zremote-gui/src/views/command_palette/*` — "Open diff" entries

**Tests**
- Reducer tests: `DiffStarted` sets files; `DiffFileChunk` fills
  `loaded_files`; `DiffFinished { error }` sets error.
- `file_tree::render_item` unit per `DiffFileStatus`.
- Visual: `/visual-test` skill — load a synthetic project, open diff,
  screenshot.

**Exit:** user can click "Diff" on a project in a running local agent and
see a unified diff of the working tree with file list.

### Phase 4 — Syntax highlight + side-by-side

**CREATE**
- `crates/zremote-gui/src/views/diff/highlight.rs`

**MODIFY**
- `crates/zremote-gui/Cargo.toml` — `syntect = { workspace = true }`
- `Cargo.toml` (workspace) — `syntect = "5.3"`
- `crates/zremote-gui/src/views/diff/diff_pane.rs` — view-mode toggle,
  per-side line rendering, highlight cache integration

**Tests**
- `detect_syntax`: `.rs`, `.ts`, `.go`, `.py`, `.md`, no extension, unknown.
- Stability test: same input → same spans.
- File > threshold → highlight skipped, diff-only colours applied.

**Exit:** side-by-side and unified both render correctly; syntax highlight
visible on rust / ts / py.

### Phase 5 — Review comments

**CREATE**
- `crates/zremote-gui/src/views/diff/{review_panel.rs, review_comment.rs}`

**MODIFY**
- `crates/zremote-gui/src/views/diff/state.rs` — `draft_comments` vec +
  reducers (add/edit/delete/send/clear)
- `crates/zremote-gui/src/views/diff/diff_pane.rs` — gutter `+` icon,
  inline composer, inline rendered comments
- `crates/zremote-gui/src/persistence.rs` — add `diff_drafts` key handling
- `crates/zremote-gui/src/views/command_palette/*` — "Send review" entry

**Tests**
- Reducer tests: add single-line, add range, edit, delete, clear all.
- Persistence roundtrip: save drafts → restart GUI mock → drafts load.
- Integration: spawn agent, send 2 comments via `InjectSession`, read PTY
  output, assert the rendered prompt contains both comments grouped by file.
- CSI attempt: body `"foo \x1b[31m BAD"` must be sanitised in the PTY
  output.

**Exit:** full MVP. User reviews a diff, comments on lines, sends to a
session, sees the rendered markdown appear in the target terminal.

### Phase 6 — Polish (deferred, not MVP)

- Agent-emitted `ReviewReady` event + session badge clearing.
- Split-pane layout (diff beside terminal).
- Side-by-side scroll sync.
- Word-level intra-line diff (`imara-diff` client-side).
- Keyboard-only review flow (j/k/n).
- `StartClaudeTask` delivery path.
- Large-file lazy "load anyway" toggle.
- Linear / GitHub PR comment import.

## 11. Risks

| Risk | Mitigation |
|---|---|
| Unified-diff parser edge cases (renames, binary, "no newline at EOF", submodules) | Port Okena's tested parser; integration tests against `tempfile` repos; `ZREMOTE_DIFF_BACKEND=gix` escape hatch. |
| Large files cause OOM in streaming chunk | Hard caps: per-file 512 KB, total 2000 files; `too_large=true` signals skip. |
| Protocol compat (old agents) | `Register.supports_diff` capability flag; server returns 501 with hint when missing. |
| WS frame size under large diffs | Per-file chunk capped at 512 KB; agent enforces; WS server limit 64 MB default is ample. |
| Syntect init latency (~50 ms) | Warm up `HighlightEngine::global()` in background task at GUI start. |
| Syntect perf on huge files | Skip highlight > 1 MB / > 10 000 lines; diff-only colours. |
| Slow NDJSON consumer blocks agent | bounded mpsc (32) backpressure; agent aborts with `BrokenPipe`; UI surfaces "stream slow". |
| CSI injection via comment body | Strip control chars before PTY write; integration test. |
| Draft loss on GUI crash | Persist via existing persistence layer, 500 ms debounce; warn user on close if drafts exist. |
| Refresh clobbering in-flight drafts | No auto-refresh; banner + explicit Refresh; re-anchor on best-effort; stale group in drawer. |
| Branch / ref names with special chars | `validate_git_ref` rejects leading `-`, `..`, `\n`, NUL; pathspec uses `--` separator. |
| Stream cancel vs Server mode latency | `DiffCancel` + token checked between files keeps worst-case waste at one file's work. |

## 12. Open questions

1. **Agent feature flag name** for `supports_diff` — do we piggy-back an
   existing capability struct or add a standalone field? (Decide in P0
   review of `terminal.rs::Register` layout.)
2. **Review draft persistence granularity** — per `(host, project)` only, or
   also keyed on `DiffSource`? (Currently per project; cross-source drafts
   all live together. User can inspect but not filter by source in v1.)
3. **Session-project mapping** — drafts target a session; we rely on
   `session.working_dir ⊆ project.path`. Is that reliable across worktrees?
   Verify in P5 with a test for the worktree case. Worst case: fall back to
   "all sessions on the same host".
4. **Syntect theme** — ship one light + one dark theme matched to
   `theme::*()` in P4, or punt until P6? Proposal: ship one baseline in P4
   mapped from `theme::*()` so we aren't at the mercy of syntect bundled
   themes.
5. **MCP delivery** — placeholder variant `ReviewDelivery::McpTool` is in
   the protocol for forward compat. Do we also want to expose review as an
   MCP resource for read-only consumption by external Claude sessions? Out
   of scope for this RFC.

## 12a. Deviations

- **ViewMode persistence deferred to Phase 6 (project UX polish).** RFC §2
  goal "per-project persisted preference" for unified vs side-by-side mode
  is not delivered in P4 scope by design — P4 surface is limited to the
  syntect highlight engine and the side-by-side renderer. Current behaviour:
  ViewMode defaults to `Unified` on every diff open; user toggle via Alt+S
  persists only for the open view instance.

## 13. References

- Research report — `docs/rfc/research-git-diff-ui.md`
- UX report — `docs/rfc/ux-git-diff-ui.md`
- Architecture report — `docs/rfc/arch-git-diff-ui.md`
- Okena (reference impl) — `https://github.com/contember/okena`
- Arbor (reference impl) — `https://github.com/penso/arbor`
- Existing git layer — `crates/zremote-agent/src/project/git.rs`
- Protocol home — `crates/zremote-protocol/src/terminal.rs`

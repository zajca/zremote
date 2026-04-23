# Architecture — Git Diff Viewer + Review Comments

Technická architektura pro diff viewer v GPUI desktop klientovi. Musí fungovat
ve všech třech módech ZRemote:

- **Standalone**: `zremote gui --local` — binary si sám spawnuje agent child process.
- **Local**: agent běží samostatně (`zremote agent local`), GUI se přes HTTP/WS
  připojuje na `localhost`.
- **Server**: GUI ↔ Axum server ↔ agent (na remote hostu, obousměrný WS tunel).
  Diff v tomto módu jde přes dvě hopy; stream přes WS je limitující faktor.

Status: _draft_. Review task #3 (architect). Závisí na tom, co researcher (task #1)
vrátí pro git2 vs. shell-out a pro syntax highlight — předpokládám volbu
odůvodněnou níže, ale finální volba proběhne po přečtení research reportu před
Phase 0 exit.

---

## 0. Terminology a předpoklady

- **DiffSource** — logický popis _čeho_ se diff týká (working tree vs. staged,
  HEAD vs. branch, range, samostatný commit).
- **DiffFile** — jeden soubor v diffu, obsahuje hlavičku + všechny hunks.
- **DiffHunk** — kontinuální blok změn oddělený `@@ ... @@` hlavičkou.
- **DiffLine** — řádek hunku (context / added / removed) se source line numbery.
- **ReviewComment** — uživatelská poznámka na řádku / bloku, kterou lze poslat
  agentovi jako instrukci.
- **Review draft** — lokální pracovní sada komentářů, existuje než ji uživatel
  odešle agentovi (nebo zahodí).

Protokolové úmluvy dle `CLAUDE.md`:

- `#[serde(tag = "type")]` pro tagged enum JSON.
- `#[serde(default)]` + `Option<T>` pro nové fieldy (backward compat).
- UUID jako string, timestampy ISO 8601 (`chrono::DateTime<Utc>`).
- `snake_case` v JSON.

---

## 1. Protocol types (`zremote-protocol`)

### 1.1 Umístění

Nové soubory:

- `crates/zremote-protocol/src/project/diff.rs` — hlavní typy (DiffSource,
  DiffRequest, DiffFile, DiffHunk, DiffLine, error types, DiffSourceList).
- `crates/zremote-protocol/src/project/review.rs` — ReviewComment, ReviewThread,
  SendReviewRequest/Response.

Zaregistrovat v `crates/zremote-protocol/src/project/mod.rs`:

```rust
mod diff;
mod review;
// ...
pub use diff::*;
pub use review::*;
```

### 1.2 DiffSource

Diskriminant _co_ se má porovnat. Všechny varianty pokrývají GitHub/PR flow i
lokální inspekci:

```rust
// crates/zremote-protocol/src/project/diff.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffSource {
    /// Unstaged changes: working tree vs. index.
    WorkingTree,
    /// Staged changes: index vs. HEAD.
    Staged,
    /// All local changes: working tree (including index) vs. HEAD.
    /// Kombinace Staged + WorkingTree, odpovídá `git diff HEAD`.
    WorkingTreeVsHead,
    /// HEAD vs. a ref (branch name, tag, SHA). `ref` is the *base* — diff
    /// ukazuje changes _from base to HEAD_. To odpovídá GitHub PR semantice
    /// (ukaž mi, co by PR přinesl proti `main`).
    HeadVs {
        #[serde(rename = "ref")]
        reference: String,
    },
    /// Two refs: `from`..`to`. `symmetric=true` = `from...to` (three-dot,
    /// merge-base based), `symmetric=false` = `from..to` (raw range).
    Range {
        from: String,
        to: String,
        #[serde(default)]
        symmetric: bool,
    },
    /// Diff introduced by a single commit (against its first parent).
    Commit {
        sha: String,
    },
}
```

**Proč tyhle varianty:** WorkingTree + Staged + WorkingTreeVsHead kryje běžný
inspekční flow ("co jsem právě změnil"). HeadVs kryje PR review ("co tenhle
branch přidá"). Range kryje "posledních N commitů" / "co se změnilo od
minulého týdne". Commit je klasický commit viewer.

### 1.3 DiffRequest / DiffFileSummary / DiffFile

```rust
/// Client → Agent: žádost o diff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffRequest {
    pub project_id: String,
    pub source: DiffSource,
    /// Whitelist of file paths (relative to project root). None = all files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_paths: Option<Vec<String>>,
    /// Lines of context per hunk. Default 3 (matches `git diff -U3`).
    #[serde(default = "default_context_lines")]
    pub context_lines: u32,
    /// Include syntax highlighting tokens in the response. Client may set
    /// false for initial listing (summary only) and true when rendering a
    /// specific file.
    #[serde(default)]
    pub include_highlight: bool,
}

fn default_context_lines() -> u32 { 3 }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    /// Type change (e.g., file → symlink). Rare but git reports it.
    TypeChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffFileSummary {
    /// New path (current name). For deletes this is the old path.
    pub path: String,
    /// Old path. Set only for Renamed/Copied; None otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: DiffFileStatus,
    /// Binary content — no hunks will be sent.
    #[serde(default)]
    pub binary: bool,
    /// Submodule change — no hunks.
    #[serde(default)]
    pub submodule: bool,
    /// Large file — hunks omitted (set when exceeds agent-side threshold).
    #[serde(default)]
    pub too_large: bool,
    pub additions: u32,
    pub deletions: u32,
    /// Old blob SHA (pre-image). Used for later anchor-based resyncing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_sha: Option<String>,
    /// New blob SHA (post-image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_sha: Option<String>,
    /// Old file mode (git mode bits, e.g. "100644"). Present only when it
    /// changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffFile {
    pub summary: DiffFileSummary,
    #[serde(default)]
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// Raw hunk header text (`@@ -10,7 +10,8 @@ fn foo`).
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
    /// Rare but possible: "No newline at end of file" marker line.
    NoNewlineMarker,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// 1-based line number on the old side. None for Added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_lineno: Option<u32>,
    /// 1-based line number on the new side. None for Removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_lineno: Option<u32>,
    pub content: String,
}
```

### 1.4 Source picker — list diff sources

Pro UI picker (branch dropdown + recent commits) potřebujeme lehký endpoint —
jinak by klient musel volat `list_branches` + nový recent-commits endpoint
zvlášť.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentCommit {
    pub sha: String,
    pub short_sha: String,
    pub author: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffSourceOptions {
    /// Whether the working tree has unstaged changes.
    pub has_working_tree_changes: bool,
    /// Whether the index has staged changes.
    pub has_staged_changes: bool,
    /// Local + remote branches reusing existing `BranchList` from
    /// `project::git`. Frontend will build HeadVs picker from this.
    pub branches: BranchList,
    /// HEAD + N most recent commits (capped, default 50).
    pub recent_commits: Vec<RecentCommit>,
    /// Current HEAD short SHA so the picker can highlight it.
    pub head_sha: Option<String>,
    pub head_short_sha: Option<String>,
}
```

### 1.5 Review comments

Lokální draft žije jen na GUI straně (in-memory, neukládáme do DB — je to
short-lived před odesláním, viz sekce 2.4). Wire typy stačí pro `POST
/review/send`:

```rust
// crates/zremote-protocol/src/project/review.rs
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSide {
    /// Comment on removed / pre-image line.
    Old,
    /// Comment on added / context / post-image line.
    New,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineRange {
    /// 1-based inclusive.
    pub start: u32,
    /// 1-based inclusive. `start == end` for single-line comments.
    pub end: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewComment {
    pub id: Uuid,
    /// Relative path (matches DiffFileSummary.path).
    pub file_path: String,
    pub line_range: LineRange,
    pub side: ReviewSide,
    pub body: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Server/agent-side anchor metadata — snapshot of the line content at
    /// draft time so a stale comment can be flagged if the file moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_content: Option<String>,
    /// Blob SHA the comment was drafted against (copied from
    /// DiffFileSummary.new_sha for added lines, old_sha for removed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_blob_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDelivery {
    /// Inject body into an existing agent session's PTY stdin.
    InjectSession,
    /// Start a new Claude task with the review as initial prompt.
    StartClaudeTask,
    /// (Future) send via MCP. Keep the variant so we don't break wire compat.
    McpTool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendReviewRequest {
    pub project_id: String,
    /// Diff source the review was drafted against. Echoed back in response so
    /// the injected prompt can cite it ("review of diff against main").
    pub source: DiffSource,
    pub comments: Vec<ReviewComment>,
    pub delivery: ReviewDelivery,
    /// Required when delivery == InjectSession.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<uuid::Uuid>,
    /// Optional freeform preamble (e.g. "Please address these comments:").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendReviewResponse {
    /// Session the review was routed to (new or existing).
    pub session_id: uuid::Uuid,
    /// Number of comments actually delivered.
    pub delivered: u32,
    /// Rendered prompt text that was written to the PTY (for audit trail).
    pub prompt: String,
}
```

### 1.6 Error types

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffErrorCode {
    /// Not a git repo.
    NotGitRepo,
    /// Ref / SHA not found.
    RefNotFound,
    /// Working tree missing (path gone).
    PathMissing,
    /// File listed in `file_paths` doesn't exist in diff.
    FileNotInDiff,
    /// Git subprocess timed out.
    Timeout,
    /// Any other error — message carries detail.
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffError {
    pub code: DiffErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}
```

### 1.7 WS-tunnel variants (ServerMessage / AgentMessage)

Pro server mode musíme dispatchnout diff requesty přes WS. Vzorem je
`ProjectGitStatus` / `GitStatusUpdate` (request/response pair). Velké diffy
rozdělíme do **chunks** — jedna `DiffStarted` zpráva + N `DiffFile` zpráv +
`DiffFinished`. Pokud bude file příliš velký, pošleme jen summary +
`too_large=true`.

Pseudodefinice rozšíření v `crates/zremote-protocol/src/terminal.rs`:

```rust
// v ServerMessage
ProjectDiff {
    request_id: uuid::Uuid,
    request: DiffRequest,
},
ProjectDiffSources {
    request_id: uuid::Uuid,
    project_path: String,
},
ProjectSendReview {
    request_id: uuid::Uuid,
    request: SendReviewRequest,
},

// v AgentMessage
DiffStarted {
    request_id: uuid::Uuid,
    files: Vec<DiffFileSummary>,
},
DiffFileChunk {
    request_id: uuid::Uuid,
    /// Index into the `files` list from DiffStarted (stable under streaming).
    file_index: u32,
    file: DiffFile,
},
DiffFinished {
    request_id: uuid::Uuid,
    /// If Some, the whole op failed after DiffStarted — client should abort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<DiffError>,
},
DiffError {
    request_id: uuid::Uuid,
    error: DiffError,
},
DiffSourcesResult {
    request_id: uuid::Uuid,
    options: Option<Box<DiffSourceOptions>>,
    error: Option<DiffError>,
},
SendReviewResult {
    request_id: uuid::Uuid,
    response: Option<Box<SendReviewResponse>>,
    error: Option<DiffError>,
},
```

**Chunking design:**

1. Agent vygeneruje summary všech souborů (`Vec<DiffFileSummary>`) a pošle
   `DiffStarted`.
2. Pro každý soubor (v pořadí) pošle `DiffFileChunk` — celý `DiffFile`.
   Chunk-per-file je kompromis: zachovává "one message = one logical unit"
   semantiku WS protokolu a server mode nepotřebuje parsovat streaming JSON
   uprostřed zprávy.
3. Pokud soubor překročí `MAX_FILE_HUNK_BYTES` (viz sekce 3.3), pošle se
   `DiffFileChunk` s prázdnými hunks a `summary.too_large=true`.
4. Na konci `DiffFinished`. Pokud něco selže po `DiffStarted`, pošle se
   `DiffFinished { error: Some(...) }` — klient má nečeknuté chunky zahodit.

Proč **ne** streamovaný JSON-lines response (server mode): server zná WS
message boundaries jen po ServerMessage/AgentMessage enum. Splittování do
menších jednotek dělá retry/error handling jednodušší. V local mode s HTTP můžeme
použít stejný wire formát jako sekvenci `Content-Type: application/x-ndjson`
(viz 2.1) — tělo se skládá ze stejných `DiffStarted` / `DiffFileChunk` /
`DiffFinished` JSON objektů, jeden na řádek.

---

## 2. Agent endpointy

### 2.1 Local REST (`zremote-agent/src/local/routes/projects/`)

Nový modul `diff.rs`. Router registrace v
`crates/zremote-agent/src/local/router.rs` (k existujícím routám
`/api/projects/...`):

```rust
.route(
    "/api/projects/{project_id}/diff",
    post(routes::projects::diff::post_diff),
)
.route(
    "/api/projects/{project_id}/diff/sources",
    get(routes::projects::diff::get_diff_sources),
)
.route(
    "/api/projects/{project_id}/review/send",
    post(routes::projects::diff::post_send_review),
)
```

**Transport pro `POST /api/projects/:id/diff`:** NDJSON streaming
(`Content-Type: application/x-ndjson`). Každý řádek je JSON objekt odpovídající
variantě enum `DiffEvent { Started(..), File(..), Finished(..) }`. Axum to
podporuje přes `axum::body::Body::from_stream`:

```rust
use axum::body::Body;
use axum::response::Response;
use futures::stream::Stream;

pub async fn post_diff(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
    Json(req): Json<DiffRequest>,
) -> Result<Response, AppError> {
    let project_id = parse_project_id(&project_id)?.to_string();
    let (_, path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(...))?;

    // mpsc channel fed by a spawn_blocking git worker; receiver is the
    // streaming body.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(16);
    let req_clone = req.clone();
    tokio::task::spawn_blocking(move || {
        crate::project::diff::run_diff_streaming(
            std::path::Path::new(&path),
            &req_clone,
            |event| {
                // Serialize to JSON + \n and send through channel.
                let mut line = serde_json::to_vec(&event).unwrap_or_default();
                line.push(b'\n');
                // Try_send so the worker can detect a disconnected client and
                // abort early; map to std::io::Error::Other on closed channel.
                tx.blocking_send(Ok(bytes::Bytes::from(line)))
                    .map_err(|_| std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "client disconnected",
                    ))
            },
        )
    });

    Ok(Response::builder()
        .header("content-type", "application/x-ndjson")
        .body(Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)))?)
}
```

Proč NDJSON a ne paginace: diff by měl být "one request, stream events" —
paginace by vyžadovala stable file ordering na serveru a komplikovala by
unifikaci se server mode. NDJSON line boundaries jsou robustní i pro klient s
vlastním parserem (zremote-client).

Pro **server mode** je wire formát totéž (ServerMessage::ProjectDiff → agent
generuje DiffStarted/DiffFileChunk/DiffFinished AgentMessages). Jinak přes oba
módy je datový model identický, jen transport vrstva se mění (HTTP NDJSON vs.
WS messages).

**`GET /api/projects/:id/diff/sources`:** klasický JSON response, nestreamuje
se. Vrací `DiffSourceOptions`. Cap na `recent_commits` (default 50, konfigurovatelně
query param `?commits=N`, max 200).

### 2.2 Request IDs a zrušení

Client generuje `request_id: Uuid` pro každý diff request. V local mode je to
uchované jen v rámci HTTP connection (close connection = abort). V server mode
musíme:

1. Mít mapu `request_id → CancellationToken` na agentovi.
2. Nový `ServerMessage::DiffCancel { request_id }` který nastaví token.
3. Worker pravidelně kontroluje token (mezi files) a pošle
   `DiffFinished { error: Some(Timeout) }` při zrušení.

Podobně tear-down: když server zavře WS k agentovi, všechny in-flight diff
requests dostanou synteticky `DiffFinished { error: Other("connection closed") }`
k GUI klientovi, aby se GUI nezaseklo na "loading".

### 2.3 Server dispatch (`zremote-server/src/routes/agents/dispatch.rs`)

Přidat match arm v `handle_agent_message` pro každý nový AgentMessage variant
(DiffStarted / DiffFileChunk / DiffFinished / DiffError / DiffSourcesResult /
SendReviewResult). Server nemá DB tabulku pro diff — je to stateless passthrough.

Implementace: server má mapu `Map<request_id, mpsc::Sender<DiffEvent>>`. REST
endpoint na serveru (`POST /api/projects/:id/diff`) vytvoří request_id, zaregistruje
sender, pošle `ServerMessage::ProjectDiff` agentovi, převezme `mpsc::Receiver`
a vrátí ho jako NDJSON body:

```rust
// zremote-server/src/routes/projects/diff.rs  (nový)
pub async fn post_diff(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    Json(req): Json<DiffRequest>,
) -> Result<Response, AppError> {
    let (host_id, path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(...))?;
    let request_id = Uuid::new_v4();
    let (tx, rx) = mpsc::channel::<DiffEvent>(32);
    state.diff_dispatch.register(request_id, tx).await;

    let sender = state.connections.get_sender(&host_id.parse()?).await
        .ok_or_else(|| AppError::Conflict("host offline".into()))?;
    sender.send(ServerMessage::ProjectDiff {
        request_id,
        request: DiffRequest { project_id: path, ..req },
    }).await?;

    // Stream rx → NDJSON body. On drop, unregister request_id and fire
    // ServerMessage::DiffCancel { request_id } to the agent.
    Ok(ndjson_response(rx, state.diff_dispatch.clone(), request_id))
}
```

`diff_dispatch` je nový field na `AppState`:

```rust
pub struct DiffDispatch {
    inner: Arc<RwLock<HashMap<Uuid, mpsc::Sender<DiffEvent>>>>,
}
```

A v `handle_agent_message`:

```rust
AgentMessage::DiffStarted { request_id, files } => {
    state.diff_dispatch.forward(request_id, DiffEvent::Started { files }).await;
}
AgentMessage::DiffFileChunk { request_id, file_index, file } => {
    state.diff_dispatch.forward(request_id, DiffEvent::File { file_index, file }).await;
}
AgentMessage::DiffFinished { request_id, error } => {
    state.diff_dispatch.finish(request_id, error).await;
}
```

### 2.4 Review comments storage

**Rozhodnutí:** GUI drží draft in-memory (Entity field). Teprve `POST /review/send`
obsahuje plný payload. Důvody:

- Review draft má short lifespan (vteřiny až minuty) — nemá smysl pro něj
  platit DB roundtrip.
- Nepotřebujeme cross-device sync (draft je vázaný na jednu GUI instanci).
- Žádné offline delivery: v okamžiku "send" musí být agent online, jinak to
  vrátí 409.
- Zjednodušuje protokol — komentář žije čistě v UI.

Trade-off: uživatel zavře GUI → draft zmizí. Přijatelné pro první iteraci;
P2 může přidat `POST /review/draft` endpoint s lokální SQLite persistencí
(sloupec na `projects` tabulce, serializovaný JSON).

### 2.5 Send review flow

Dvě cesty přes `ReviewDelivery`:

**`InjectSession`:** existující flow analogický `ContextPush` (viz
`crates/zremote-protocol/src/terminal.rs:244`). Agent:

1. Validuje `session_id` — musí existovat v `SessionManager`.
2. Renderuje prompt (sekce 2.6).
3. Píše bytes do PTY přes `session_manager.write_to(session_id, ...)`.
4. Odpoví `SendReviewResponse` s rendered promptem.

**`StartClaudeTask`:** použije existující `ClaudeServerMessage::StartSession`
path s `initial_prompt = rendered_review`. Vrací nové `session_id`, které GUI
otevře v terminal panelu automaticky.

### 2.6 Rendering review promptu

Funkce `render_review_prompt` v `zremote-agent/src/project/review.rs`:

```rust
pub fn render_review_prompt(req: &SendReviewRequest) -> String {
    let mut out = String::new();
    if let Some(pre) = &req.preamble {
        out.push_str(pre);
        out.push_str("\n\n");
    }
    out.push_str("## Code review comments\n\n");
    out.push_str(&format!("Diff source: {}\n\n", format_diff_source(&req.source)));

    // Group by file for readability.
    let mut by_file: BTreeMap<&str, Vec<&ReviewComment>> = BTreeMap::new();
    for c in &req.comments {
        by_file.entry(c.file_path.as_str()).or_default().push(c);
    }
    for (file, comments) in by_file {
        out.push_str(&format!("### `{file}`\n\n"));
        for c in comments {
            let range = if c.line_range.start == c.line_range.end {
                format!("L{}", c.line_range.start)
            } else {
                format!("L{}-{}", c.line_range.start, c.line_range.end)
            };
            let side = match c.side {
                ReviewSide::Old => "old",
                ReviewSide::New => "new",
            };
            out.push_str(&format!("- {range} ({side}): {}\n", c.body.trim()));
        }
        out.push('\n');
    }
    out
}
```

Pro PTY injection ukončit `\n` (trigger prompt submit); pro `StartClaudeTask`
použít plain markdown (prompt je už submitovaný startem).

**Bezpečnost:** escape backtick / řídicí sekvencí v `body` není potřeba pro
PTY (nic se neeval-uje), ale budeme se bránit CSI injection: před zápisem do PTY
projít komentáře filtrem který odstraní `\x1b[` sekvence. Implementace:
`body.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t')`.

---

## 3. Git layer (agent side)

### 3.1 Rozhodnutí: `git2` vs. shell-out

Existující `project/git.rs` používá shell-out (timeout 5s, credential prompts
disabled, path traversal guards). Diff layer doporučuje **`git2` (libgit2)**
z následujících důvodů:

1. **Strukturovaný výstup** — `Diff` API vrací hunks/lines přímo jako objekty,
   odpadá parser textového unified diff formátu (který je past — filenames se
   speciálními znaky, rename detection heuristiky, binary markers).
2. **Rename detection** — `DiffOptions::skip_binary_check(false)` +
   `find_similar` vrátí Added/Deleted páry jako Rename, což `git diff`
   default dělá jen pro commit-vs-commit (ne pro index).
3. **Výkon** — žádný fork, žádný subprocess overhead (relevantní pro streaming
   mnoha souborů).
4. **Blob SHA přístupné přímo** — nepotřebujeme extra `git hash-object` calls
   pro DiffFileSummary.{old,new}_sha.

Rizika:
- `git2` verze v workspace musí být pinned (`git2 = "0.19"`, v `Cargo.toml`).
  Přidáváme jednu crate, ale libgit2 už je transitive dependency přes vícero
  ekosystému (např. cargo).
- libgit2 nedělá worktree discovery pro linked worktrees **identicky** jako
  git CLI v některých edge cases. Ověřit testem (`list_branches` reference).
- `unsafe_code = "deny"` — git2 crate má interně unsafe, ale wrapper je safe
  rust, takže workspace lint je OK.

**Fallback plán:** pokud researcher v task #1 doporučí shell-out (např.
kompatibilita s submodules nebo sparse checkout), použít shell-out s parserem
na `unified-diff-parser` crate, ale stále generovat stejné protocol typy.
Protokol tím nedotčen.

**Umístění:** nový modul `crates/zremote-agent/src/project/diff.rs`. Jednoduché
API:

```rust
use std::path::Path;
use zremote_protocol::project::{DiffError, DiffRequest, DiffFile, DiffFileSummary, DiffSource, DiffSourceOptions};

/// Event emitted by the streaming diff runner.
pub enum DiffEvent {
    Started { files: Vec<DiffFileSummary> },
    File { file_index: u32, file: DiffFile },
    Finished { error: Option<DiffError> },
}

/// Synchronous streaming runner. Invokes `sink` for each event; aborts on sink
/// error (client disconnected). Runs on a spawn_blocking thread.
pub fn run_diff_streaming<F>(
    project_path: &Path,
    req: &DiffRequest,
    sink: F,
) -> Result<(), DiffError>
where
    F: FnMut(&DiffEvent) -> std::io::Result<()>,
{
    // ...
}

/// Non-streaming variants for unit tests + simple callers. Internally uses
/// run_diff_streaming but buffers into a Vec.
pub fn compute_diff(project_path: &Path, req: &DiffRequest) -> Result<Vec<DiffFile>, DiffError>;

/// Enumerate sources for the picker.
pub fn list_diff_sources(project_path: &Path, max_commits: usize)
    -> Result<DiffSourceOptions, DiffError>;
```

### 3.2 Mapping DiffSource → git2 operace

```rust
match &req.source {
    DiffSource::WorkingTree => repo.diff_index_to_workdir(None, Some(&opts))?,
    DiffSource::Staged => {
        let head_tree = repo.head()?.peel_to_tree()?;
        repo.diff_tree_to_index(Some(&head_tree), None, Some(&opts))?
    }
    DiffSource::WorkingTreeVsHead => {
        let head_tree = repo.head()?.peel_to_tree()?;
        repo.diff_tree_to_workdir_with_index(Some(&head_tree), Some(&opts))?
    }
    DiffSource::HeadVs { reference } => {
        let base = repo.revparse_single(reference)?.peel_to_tree()?;
        let head = repo.head()?.peel_to_tree()?;
        repo.diff_tree_to_tree(Some(&base), Some(&head), Some(&opts))?
    }
    DiffSource::Range { from, to, symmetric } => {
        let base_id = repo.revparse_single(from)?.id();
        let to_id = repo.revparse_single(to)?.id();
        let base_id = if *symmetric {
            repo.merge_base(base_id, to_id)?
        } else {
            base_id
        };
        let base = repo.find_object(base_id, None)?.peel_to_tree()?;
        let to = repo.find_object(to_id, None)?.peel_to_tree()?;
        repo.diff_tree_to_tree(Some(&base), Some(&to), Some(&opts))?
    }
    DiffSource::Commit { sha } => {
        let commit = repo.revparse_single(sha)?.peel_to_commit()?;
        let parent = commit.parent(0).ok();
        let parent_tree = parent.as_ref().map(|c| c.tree()).transpose()?;
        let commit_tree = commit.tree()?;
        repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), Some(&opts))?
    }
}
```

`opts` je `DiffOptions` s `context_lines(req.context_lines)`, path filter
(pokud `req.file_paths` je Some), a následné `diff.find_similar(Some(&mut find_opts))`
pro rename detection.

### 3.3 Limity (rozhodnout na agent side)

- `MAX_FILE_HUNK_BYTES: 512 * 1024` — po překročení se soubor označí
  `too_large=true` a neposílá se obsah. UI musí nabídnout "load anyway" tlačítko
  (nový request s `file_paths=[one file]` + override flag `force_large=true`,
  dodáme v P2 pokud potřeba).
- `MAX_DIFF_TOTAL_FILES: 2000` — pokud diff obsahuje víc (např. uživatel klikne
  na "branch vs. root commit" po létech), vrátit `DiffError { code: Other }`
  s hintem.
- `DIFF_COMPUTE_TIMEOUT: 30s` — celková wall-clock.
- `context_lines` cap: 20 (víc nedává smysl a zhoršuje výkon).

### 3.4 Binary / submodule / text encoding

- Binary: `delta.flags().is_binary()` → `summary.binary = true`, hunks prázdné.
- Submodule: detect přes `delta.old_file().mode() == FileMode::Commit`.
- Encoding: libgit2 vrací bytes. Konvertujeme `String::from_utf8_lossy` — pro
  non-UTF-8 soubory tím pádem budou `U+FFFD` replacements, ale protokol zůstane
  serializovatelný. Alternativa (unicode_normalize) je overkill pro první
  iteraci.
- CRLF: posíláme tak jak je v blobu. GUI může volitelně zobrazit neviditelné
  znaky (P2 feature).

### 3.5 `list_diff_sources` implementace

```rust
pub fn list_diff_sources(project_path: &Path, max_commits: usize)
    -> Result<DiffSourceOptions, DiffError>
{
    let repo = git2::Repository::open(project_path)?;

    // has_working_tree_changes / has_staged_changes
    let head_tree = repo.head().ok().and_then(|r| r.peel_to_tree().ok());
    let has_wt = repo.diff_index_to_workdir(None, None)?.deltas().len() > 0;
    let has_staged = match &head_tree {
        Some(t) => repo.diff_tree_to_index(Some(t), None, None)?.deltas().len() > 0,
        None => false, // empty repo
    };

    // Recent commits (revwalk from HEAD, max `max_commits`)
    let mut walk = repo.revwalk()?;
    walk.push_head()?;
    let mut recent = Vec::new();
    for oid_result in walk.take(max_commits) {
        let oid = oid_result?;
        let commit = repo.find_commit(oid)?;
        recent.push(RecentCommit {
            sha: oid.to_string(),
            short_sha: oid.to_string()[..7].to_string(),
            author: commit.author().name().unwrap_or("").to_string(),
            timestamp: chrono::DateTime::from_timestamp(commit.time().seconds(), 0)
                .unwrap_or_else(chrono::Utc::now),
            subject: commit.summary().unwrap_or("").to_string(),
        });
    }

    // Reuse existing shell-based GitInspector for branches (it already caps +
    // times out). We only bail on diff-specific functionality via git2.
    let branches = GitInspector::list_branches(project_path)
        .map_err(|e| DiffError { code: DiffErrorCode::Other, message: e, hint: None })?;

    let head_ref = repo.head().ok();
    let head_sha = head_ref.as_ref().and_then(|r| r.target())
        .map(|oid| oid.to_string());
    let head_short = head_sha.as_ref().map(|s| s[..7].to_string());

    Ok(DiffSourceOptions {
        has_working_tree_changes: has_wt,
        has_staged_changes: has_staged,
        branches,
        recent_commits: recent,
        head_sha,
        head_short_sha: head_short,
    })
}
```

---

## 4. GPUI view (`zremote-gui`)

### 4.1 Struktura modulů

```
crates/zremote-gui/src/views/diff/
    mod.rs              // public entry point (DiffView + events)
    source_picker.rs    // Dropdown: Working | Staged | HEAD vs. ... | Commit ...
    file_tree.rs        // Left pane: file list with status badges
    diff_pane.rs        // Center: unified or side-by-side rendering
    review_panel.rs     // Right: draft comments + send button
    review_comment.rs   // Single comment widget (inline on line, + sidebar entry)
    highlight.rs        // Syntax highlight bridge (see 4.4)
    state.rs            // DiffState (loaded DiffSourceOptions, pending request,
                        // active DiffFile list, selected file index, review draft)
```

Registrace v `crates/zremote-gui/src/views/mod.rs`:

```rust
pub mod diff;
```

### 4.2 Entry points (jak se dostat k diff view)

1. **Sidebar project card** — nová ikona "Diff" vedle existujících project
   actions (viz `crates/zremote-gui/src/views/sidebar_items.rs`). Po kliknutí
   `MainView::open_diff(project_id)`.
2. **Session toolbar** — pokud session má `working_dir` odpovídající projektu,
   zobrazit "Review changes" button (analogicky existujícímu worktree actions).
3. **Command palette** — `Diff: show working tree`, `Diff: compare branches`.
4. **Keyboard** — `Cmd+Shift+D` (jako Zed). Registrovat v
   `crates/zremote-gui/src/views/key_bindings.rs`.

### 4.3 MainView integrace

`MainView` má dnes `terminal: Option<Entity<TerminalPanel>>`. Přidáme:

```rust
pub enum MainContent {
    Terminal(Entity<TerminalPanel>),
    Diff(Entity<DiffView>),
    /// Future: ActivityPanel, etc.
}

pub struct MainView {
    // ...
    content: Option<MainContent>,  // replaces `terminal`
}
```

Když user otevře session → `MainContent::Terminal`. Když klikne "Diff" →
`MainContent::Diff`. Přepínač je součástí action layer — session <-> diff je
tab switch, ne window split (pro první iteraci). P2 může udělat split-pane.

### 4.4 DiffView struktura

```rust
pub struct DiffView {
    app_state: Arc<AppState>,
    project_id: String,
    source_picker: Entity<SourcePicker>,
    file_tree: Entity<FileTree>,
    diff_pane: Entity<DiffPane>,
    review_panel: Entity<ReviewPanel>,
    focus_handle: FocusHandle,
    /// Owned: when DiffView is dropped, this cancels the NDJSON stream.
    _loader: Option<Task<()>>,
    /// Owned: syntax highlight background task for the currently-shown file.
    _highlighter: Option<Task<()>>,
    state: DiffState,
}

pub struct DiffState {
    source: DiffSource,
    sources_available: Option<DiffSourceOptions>,
    files: Vec<DiffFileSummary>,
    loaded_files: HashMap<String, DiffFile>,  // path → full file
    selected_file: Option<String>,
    view_mode: DiffViewMode,  // Unified or SideBySide
    /// Draft review comments (lifted out of ReviewPanel so DiffPane can
    /// render inline markers).
    draft_comments: Vec<ReviewComment>,
    loading: bool,
    error: Option<DiffError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffViewMode { Unified, SideBySide }
```

### 4.5 Task ownership

Per CLAUDE.md "Async Task Ownership Convention":

- `_loader: Option<Task<()>>` — stored on struct. Při přepnutí zdroje se starý
  task dropne (= cancel) a nový se nastaví.
- `_highlighter: Option<Task<()>>` — per-file, cancel při přepnutí file.
- NENÍ `.detach()` pro loading — to by způsobilo leaking tasks po dropu view.

### 4.6 Syntax highlighting

**Volba:** `syntect` (s pre-loadnutou default theme + syntax set). Alternativy:

| Knihovna | Pro | Proti |
|----------|-----|-------|
| `syntect` | Jednoduché API, dump themes do bin, pre-built syntax | Runtime cost (regex-based), ~5 MB syntax set |
| `tree-sitter` | Lepší parsing, grammar-per-language | Komplexní setup, per-language crates, kompilace WASM |
| `bat` as crate | Reusable pipeline | Ne crate-form API, CLI-oriented |

`syntect` — zvolené. Implementace:

```rust
// crates/zremote-gui/src/views/diff/highlight.rs
pub struct HighlightEngine {
    syntax_set: syntect::parsing::SyntaxSet,
    theme: syntect::highlighting::Theme,
}

impl HighlightEngine {
    pub fn global() -> &'static Self { /* lazy_static */ }

    /// Synchronous highlighting of a single line. Returns Vec<(Style, Range<usize>)>.
    /// Runs in background task — never on render thread.
    pub fn highlight_line(&self, line: &str, syntax: &SyntaxReference) -> Vec<(Style, String)>;

    pub fn detect_syntax(&self, path: &str) -> &SyntaxReference;
}
```

**Integrace s GPUI textem:** podobně jako v `terminal_element.rs` — konvertovat
`syntect::highlighting::Style` na GPUI `HighlightStyle` + `StyledText`.
Background task highlight-ne všechny line-arrays (Vec per hunk), výsledek se
zachytí v `DiffPane::highlighted_lines` cache. Při re-renderu je render funkce
jen iteruje.

**Výkon:** pro soubory 5000+ řádků highlight-ovat jen visible range (viewport).
Cache se invaliduje při scroll (levné) a při font-size change (drahé, ale rare).
Lazy highlight podobný pattern jako existuje v `terminal_element.rs` pro glyph
caching.

**Theming:** použít `base16-ocean.dark` ze syntect defaults, nebo (lepší)
mapovat `syntect::Style.foreground` na ZRemote theme přes lookup tabulku.
První iterace: syntect theme přímo; polish v P2.

### 4.7 Side-by-side vs. unified rendering

- **Unified** (default): hunks `DiffLine`-by-`DiffLine`, background color
  podle `kind` (Added = green, Removed = red, Context = none). Column line
  numbers: "old" sloupec + "new" sloupec, oba optional.
- **Side-by-side**: dvojice `(removed?, added?)` vyrovnané řádek po řádku.
  Mezery (gap lines) se generují aby delete/insert páry šly vedle sebe.
  Algoritmus: projít `hunk.lines` a udržovat dva bufery; když narazíme na
  `Context`, flushneme oba buffery s null-paddingem na chybějící strany,
  pak zapíšeme context do obou sloupců.

View-mode toggle je prostý CSS/layout switch — data model se nemění.

### 4.8 Client SDK rozšíření (`zremote-client`)

Nový modul `crates/zremote-client/src/diff.rs`:

```rust
/// Streaming diff client. Caller drives a loop reading Events.
pub async fn stream_diff(
    &self,
    project_id: &str,
    req: &DiffRequest,
) -> Result<impl Stream<Item = Result<DiffEvent, ApiError>>, ApiError> {
    let resp = self.client
        .post(format!("{}/api/projects/{}/diff", self.base_url, encode_path(project_id)))
        .json(req)
        .send()
        .await?;
    let resp = self.check_response(resp).await?;
    // reqwest bytes stream → LinesCodec → serde_json::from_str per line
    Ok(ndjson_stream(resp))
}

pub async fn get_diff_sources(&self, project_id: &str)
    -> Result<DiffSourceOptions, ApiError>;

pub async fn send_review(&self, project_id: &str, req: &SendReviewRequest)
    -> Result<SendReviewResponse, ApiError>;
```

`ndjson_stream` helper: `tokio_util::codec::FramedRead<_, LinesCodec>` nad
response body, `.and_then(|line| serde_json::from_str(&line))`.

### 4.9 Render decomposition

Per CLAUDE.md:

- `DiffView::render()` ≤ 80 řádků. Kompozice:
  - `render_header()` — toolbar (source picker, view-mode toggle, refresh)
  - `render_empty_state()` — "No changes" když 0 files
  - `render_loading()` — spinner + cancel button
  - `render_error()` — error card + retry
  - `render_body()` — h-stack of file_tree + diff_pane + review_panel
  - `render_file_tree_item()` — jeden řádek (status badge + path + add/del counts)

---

## 5. Sekvenční diagramy

### 5.1 Show diff (local mode)

```
User         GUI (DiffView)      zremote-client        Agent (HTTP /api/projects/:id/diff)
 |                |                    |                         |
 |-- click -----> |                    |                         |
 |                |-- stream_diff ---->|                         |
 |                |                    |-- POST + JSON body ---->|
 |                |                    |                         |-- spawn_blocking
 |                |                    |                         |    run_diff_streaming
 |                |                    |                         |
 |                |                    |<----- 200 OK + NDJSON --|
 |                |<-- DiffStarted ----|                         |
 |                |  (set files list)  |                         |
 |                |<-- DiffFileChunk --|                         |
 |                |<-- DiffFileChunk --|                         |
 |                |     ...            |                         |
 |                |<-- DiffFinished ---|                         |
 |<-- render -----|                    |                         |
```

### 5.2 Show diff (server mode)

```
GUI    zremote-client   Server (Axum)         Agent
 |           |             |                    |
 |- stream -|              |                    |
 |           |-- POST ---->|                    |
 |           |             | register request_id tx
 |           |             |-- WS ProjectDiff ->|
 |           |             |                    | spawn_blocking diff
 |           |             |<- DiffStarted ----|
 |           | <-NDJSON---|
 |           |             |<- DiffFileChunk ---|
 |           | <-NDJSON---|                     ...
 |           |             |<- DiffFinished -----|
 |           |             | close rx            |
 |           | <- EOF -----|
```

Cancel path (user closes DiffView during load):
```
GUI drops DiffView → drops _loader Task
  → reqwest client drops response
  → TCP close to server
  → server: rx dropped → dispatch table notices
  → WS ServerMessage::DiffCancel { request_id } to agent
  → agent checks token between files → abort spawn_blocking
```

### 5.3 Send review to agent

```
User          GUI                   Agent
 |             |                      |
 |- click "Send" -> |                 |
 |             |-- send_review ------>|
 |             |  (InjectSession)     |
 |             |                      |-- session_manager.write_to(sid, rendered_bytes)
 |             |                      |
 |             |<-- SendReviewResponse|
 |<-- toast ---|                      |
 |             |  "Sent 5 comments"   |
```

Pro `StartClaudeTask` místo write_to se spawne nová PTY session přes existující
`ClaudeServerMessage::StartSession` flow a navíc `MainView` otevře nový
terminal panel.

---

## 6. Fáze implementace

### Phase 0 — Protocol + skeleton

**CREATE:**
- `crates/zremote-protocol/src/project/diff.rs` (sekce 1.2–1.4, 1.6)
- `crates/zremote-protocol/src/project/review.rs` (sekce 1.5)

**MODIFY:**
- `crates/zremote-protocol/src/project/mod.rs` — register moduls
- `crates/zremote-protocol/src/terminal.rs` — přidat nové WS variants (1.7)

**Tests:**
- Serde roundtrip pro každý nový typ + každou DiffSource variant
- Backward compat test: starý server serializovaný bez `include_highlight` field
  musí se deserializovat s default false
- Default values pro `context_lines`, `symmetric`

**Exit criteria:** `cargo test -p zremote-protocol` zelené. Diff feature nic
nedělá, ale wire types zkompilují a desktop/agent/server zkompilují.

### Phase 1 — Agent git layer + local REST

**CREATE:**
- `crates/zremote-agent/src/project/diff.rs` (git2-based, sekce 3)
- `crates/zremote-agent/src/project/review.rs` (render_review_prompt, 2.6)
- `crates/zremote-agent/src/local/routes/projects/diff.rs` (NDJSON endpoint, 2.1)

**MODIFY:**
- `crates/zremote-agent/Cargo.toml` — `git2 = "0.19"`, `syntect` se netýká
  agenta
- `crates/zremote-agent/src/project/mod.rs` — `pub mod diff; pub mod review;`
- `crates/zremote-agent/src/local/routes/projects/mod.rs` — re-export
- `crates/zremote-agent/src/local/router.rs` — 3 nové routes
- `Cargo.toml` (workspace) — git2 dep přidána

**Tests:**
- `diff.rs`: unit test pro každou DiffSource variant (init_test_repo helper,
  podobně jako v `project/git.rs`)
- Integration test pro NDJSON endpoint — spawn axum TestClient, poll body
- Test `run_diff_streaming` s abort (sink vrací Err) → worker thread se ukončí
  během iterace, NE až na konci
- `render_review_prompt` test — grouping by file, line range formátování, CSI
  strip

**Exit criteria:** `curl -X POST /api/projects/:id/diff` v local módu vrací
NDJSON stream. Standalone i local mode fungují.

### Phase 2 — Server WS dispatch + client SDK

**CREATE:**
- `crates/zremote-server/src/routes/projects/diff.rs` — REST endpoint (2.3)
- `crates/zremote-server/src/diff_dispatch.rs` — request_id → sender map
- `crates/zremote-client/src/diff.rs` — stream_diff, get_diff_sources,
  send_review (4.8)

**MODIFY:**
- `crates/zremote-server/src/routes/agents/dispatch.rs` — match arms pro
  DiffStarted/DiffFileChunk/DiffFinished/DiffSourcesResult/SendReviewResult
- `crates/zremote-server/src/state.rs` — `pub diff_dispatch: Arc<DiffDispatch>`
- `crates/zremote-server/src/routes/projects/mod.rs` — register route
- `crates/zremote-agent/src/connection/dispatch.rs` — match arm pro
  `ServerMessage::ProjectDiff` / `ProjectDiffSources` / `ProjectSendReview` /
  `DiffCancel`
- `crates/zremote-client/src/lib.rs` — re-exports

**Tests:**
- Protocol roundtrip pro nové WS variants
- `diff_dispatch` unit test: register → forward N events → finish → unregister
- Agent-side dispatch test v existujícím test modu (viz `dispatch.rs:2177+`)
  pro `ServerMessage::ProjectDiff` (init test repo, dispatch, sbírat events
  z channel jako v `worktree_create_threads_base_ref_through_dispatch`)
- Cancel test: register + drop rx → DiffCancel odchází agentovi

**Exit criteria:** server mode end-to-end (GUI → server → agent → zpět) přes
integrační test (spin up server + agent process, curl).

### Phase 3 — GPUI view MVP (unified, no highlight, no review)

**CREATE:**
- `crates/zremote-gui/src/views/diff/mod.rs`, `source_picker.rs`, `file_tree.rs`,
  `diff_pane.rs`, `state.rs`

**MODIFY:**
- `crates/zremote-gui/src/views/mod.rs` — `pub mod diff;`
- `crates/zremote-gui/src/views/main_view.rs` — MainContent enum (4.3)
- `crates/zremote-gui/src/views/sidebar_items.rs` — diff icon + action
- `crates/zremote-gui/src/views/key_bindings.rs` — Cmd+Shift+D
- `crates/zremote-gui/Cargo.toml` — `syntect` (pro P4)

**Tests:**
- `file_tree::render_file_tree_item` unit test pro každý `DiffFileStatus`
- `state.rs` reducer tests: DiffStarted → files populate, DiffFileChunk →
  loaded_files populate, DiffFinished { error } → error set

**Visual tests:** použít `/visual-test` skill (per CLAUDE.md).

**Exit criteria:** klik → pick source → vidíme file list + unified diff pro
první soubor. Žádné syntax highlight, žádné comments.

### Phase 4 — Syntax highlight + side-by-side

**CREATE:**
- `crates/zremote-gui/src/views/diff/highlight.rs` (4.6)

**MODIFY:**
- `diff_pane.rs` — integrate highlight cache, view-mode toggle

**Tests:**
- `highlight::detect_syntax` — `.rs`, `.ts`, `.go`, no extension, unknown
- Highlight result stability test (same input → same token spans)

**Exit criteria:** side-by-side funguje pro >3 jazyků (rust, ts, py).

### Phase 5 — Review comments

**CREATE:**
- `crates/zremote-gui/src/views/diff/review_panel.rs`,
  `review_comment.rs`

**MODIFY:**
- `state.rs` — `draft_comments: Vec<ReviewComment>`
- `diff_pane.rs` — inline "+" button per line, comment marker rendering
- Command palette — "Send review" action
- `crates/zremote-client` — už hotové z P2

**Tests:**
- Unit tests pro draft comment reducer (add, edit, delete, range selection)
- Integration test: spawn agent, send review via InjectSession, read PTY
  output, assert rendered prompt

**Exit criteria:** user může přidat komentář na řádku, vybrat range, odeslat
existující session, vidět content zapsaný do PTY.

### Phase 6 — Polish (nepowerzujeme v MVP)

- Draft persistence (sekce 2.4 trade-off)
- Split-pane (diff + terminal vedle sebe)
- Scroll synchronization side-by-side
- Word-diff uvnitř modified řádků
- Keyboard-only review flow (j/k mezi hunks, n pro nový komentář, Enter pro send)
- Linear/GitHub PR integration (import comments)

---

## 7. Rizika

### 7.1 Protocol compat

- Nové `ServerMessage::ProjectDiff*` a `AgentMessage::Diff*` varianty: existující
  agenti je nerozumí a server je logne jako unknown. Tolerantní parsování je
  již zajištěno (serde ignoruje unknown tags pouze při `#[serde(other)]` —
  ZRemote ho nepoužívá). **Mitigation:** verze agenta se bumpne; server před
  odesláním `ProjectDiff` zkontroluje `supports_persistent_sessions` style flag
  (přidáme `supports_diff: bool` do `Register` message) a vrátí 501 pokud ne.

### 7.2 Stream handling

- Pomalý konzument: mpsc channel v agentu má bounded capacity (32). Když
  klient NDJSON čte pomalu, `try_send` selže a diff se abortuje. To je
  očekávané chování (backpressure), ale UI musí error prezentovat
  srozumitelně ("stream slow, try again").
- WS fragmenty: AgentMessage varianty jsou malé (jeden `DiffFile` max ~512 KB).
  WS frame limit serveru je 64 MB (default), takže fit.

### 7.3 Velké soubory / velké diffy

- `too_large` markers (sekce 3.3) brání OOM.
- `MAX_DIFF_TOTAL_FILES: 2000` — pokud má diff víc, vracíme error. Spíše
  preventivně proti omylu (compare `main` se starým tagem).
- UI musí mít virtualizaci file_tree i diff_pane (lazy rendering jen viditelné
  hunks). GPUI `uniform_list` to řeší — použijeme podobně jako existující
  session switcher.

### 7.4 Unicode / encoding

- `String::from_utf8_lossy` se dotýká všech DiffLine.content — lossy je OK,
  ale musíme bumpnout verbose tooltip ("lines contain invalid UTF-8").
- Wide chars (CJK): GPUI text rendering už řeší (alacritty grapheme logic),
  stačí to nerozbít.
- CRLF: posíláme syrově. Unified/side-by-side renderer trim-ne trailing `\r`
  pouze pro visual display (cache flag `trim_cr: bool`), neukládá se do
  comment anchors.

### 7.5 Permissions / security

- Path traversal: každý `project_id → path` lookup jde přes `get_project_host_and_path`
  (už existuje, validuje). `req.file_paths` MUSÍ projít `validate_path_no_traversal`
  před tím, než se použije v `DiffOptions::pathspec`.
- CSI injection přes comment body: viz 2.6.
- Token logování: nikdy nelogovat `req.file_paths` v plain stdout (může obsahovat
  sensitive paths). `tracing::debug!(file_count = ?)` místo plného obsahu.

### 7.6 Syntax highlight performance

- `syntect` SyntaxSet binary ≈ 2 MB, inicializace ~50 ms při prvním načtení.
  Init v `once_cell::Lazy` + warmup při startu GUI (background task).
- Per-line highlight pro 5000-line file ≈ 80 ms (naměřené z jiného projektu).
  Backgrounded — nikdy ne na render thread.

### 7.7 Rename detection spolehlivost

- libgit2 `find_similar` default threshold 50 %. Pro velké přesuny s
  refactor-em nemusí rozpoznat rename. Workaround: exponovat `rename_threshold`
  v `DiffRequest` (P2).

### 7.8 git2 edge cases vs. CLI

- Sparse checkouts, partial clones — libgit2 support může zaostávat.
  **Mitigation:** nastavit feature flag `ZREMOTE_DIFF_BACKEND=cli` který
  přepne na shell-out backend (implementace odložena do P2 pokud se ukáže
  potřeba).

### 7.9 DOS vektory

- Velký `req.file_paths` (100k entries): cap na 1000 v request validaci.
- Opakované diffy ve smyčce: limit per-host rate (sdílený s existujícím
  `project scan` debounce infrastructure).
- `request_id` collision: server musí ignorovat duplicitní ID (logovat jako
  warn, zahodit).

### 7.10 Inkompatibilita between agent + server

- Pokud je server novější než agent (agent neumí ProjectDiff), server se
  zachová gracefully: zkontroluje `supports_diff` (nová flag v Register, sekce
  7.1) a vrátí 501 s user-friendly message.

---

## 8. Open questions (eskalace před implementací)

1. **git2 vs. shell-out finální volba** — čekat na research task #1 před
   začátkem Phase 1.
2. **UX viewport rozložení** — task #2 definuje finální layout; může posunout
   `MainContent` enum design (např. Diff nejde jako full-screen ale
   side-panel).
3. **ReviewDelivery::McpTool** — zatím reserve variant, implementace v P6+.
4. **Draft persistence** — v MVP NE. Uživatel dostane warning při close
   window pokud má draft.
5. **Accessibility** — screen reader support pro diff lines. Necháme v P6.

---

## 9. Changelog

- 2026-04-20 — initial draft by architect (task #3)

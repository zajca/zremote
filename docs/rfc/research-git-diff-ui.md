# Research: Git Diff UI pro ZRemote GPUI klient

**Autor:** researcher
**Datum:** 2026-04-20
**Související RFC:** docs/rfc/rfc-git-diff-ui.md (bude následovat)

Cílem je přidat read-only git diff viewer + review komentáře do GPUI desktop klienta. Diff viewer musí fungovat jak v lokálním (Standalone / Local) tak v remote (Server) módu — tedy diff se streamuje ze vzdáleného agenta přes stávající WebSocket/HTTP protokol.

Tento dokument analyzuje dvě existující Rust/GPUI implementace (Okena, Arbor), porovnává Rust git knihovny, syntax highlighting řešení a strategie pro velké diffy. Závěrem je doporučení konkrétních závislostí.

---

## Okena analýza

**Repo:** https://github.com/contember/okena (Rust, GPUI, 50⭐)
**Popis:** Terminálový multiplexer v GPUI, součástí je plnohodnotný git diff viewer (není to jenom terminál) — Okena je spíš "IDE-lite" s integrovaným diff/git viewem.

### Architektura

Rozdělení do dvou crates:

- `okena-git` — čistá doménová vrstva (`diff.rs`, `repository.rs`, `branch_names.rs`, `lib.rs`). **Zajímavost:** používá pouze `parking_lot`, `serde`, `serde_json`, `uuid`, `log` — žádné git binding crates. Všechno je shell-out na `git` binary.
- `okena-views-git` — GPUI view vrstva. Obsahuje `diff_viewer/` s moduly:
  - `mod.rs` — hlavní view, orchestrace state
  - `provider.rs` — `GitProvider` trait + `LocalGitProvider` / `RemoteGitProvider` (!)
  - `types.rs` — `FileDiff`, `DiffHunk`, `DiffLine`, `SideBySideLine`, `ExpanderRow`, `DisplayItem`, `ChangedRange`, atd.
  - `render.rs` — unified view
  - `side_by_side.rs` — side-by-side pairing algoritmus (506 řádků)
  - `line_render.rs` — rendering jednoho řádku (spans, markery)
  - `syntax.rs` — syntect integrace (pre-highlight celého souboru)
  - `scrollbar.rs` — vlastní scrollbar s drag support
  - `context_menu.rs` — kontextové menu (copy, open in editor…)

### Jak získává git data

`crates/okena-git/src/diff.rs`:
- `get_diff_with_options(path, mode, ignore_whitespace)` zavolá `git -C <path> diff ...` s flagy dle `DiffMode`.
- Parsing unified diff je **vlastní implementace** (`parse_unified_diff` ~240 řádků) — regex-less, stavový stroj po řádcích. Rozpoznává `diff --git`, `---`/`+++`, `@@ ... @@`, `index ...`.
- Pro untracked soubory (working-tree mode) zvlášť volá `git ls-files --others --exclude-standard` a vytvoří "virtuální" FileDiff se všemi řádky jako `Added`.
- **Security:** `validate_git_ref` odmítá refs začínající `-` (prevence flag injection), `safe_repo_path` kanonikalizuje cestu a ověřuje že zůstává v repu (prevence path traversal).
- **Caching:** `is_git_repo` má 30s TTL cache s max 256 entries — neplatí subprocesses na každý render.

```rust
pub enum DiffMode {
    WorkingTree,                                       // git diff
    Staged,                                            // git diff --cached
    Commit(String),                                    // git diff <hash>^ <hash>
    BranchCompare { base: String, head: String },     // git diff base...head
}
```

### Jak renderuje diff

- **Side-by-side**: páruje context/modified řádky, `SideBySideLine { left: Option<SideContent>, right: Option<SideContent> }`. Pure additions jsou `left=None`, pure removals `right=None`.
- **Expander rows**: místo zobrazení všech context lines okolo hunků ukazuje klikatelný řádek ("Show 42 hidden lines"). `ExpanderRow { old_range, new_range }` — user klikne, view přegeneruje.
- **Word-level highlighting**: `ChangedRange { start, end }` v `SideContent` — umožňuje zvýraznit změněné znaky uvnitř řádku. Pairing algorithm (v `side_by_side.rs`) počítá character-level diff mezi párovanými Modified řádky.
- **Syntax highlighting**: `syntect`. Klíčový insight — pre-highlightuje **celý soubor** (old i new), ne jenom hunk lines. To je nutné pro správný state stroje syntektu v multi-line konstruktech (JSX, template literals, block comments). Výsledek: `HashMap<usize, Vec<HighlightedSpan>>` indexovaný 1-based line num.
- **Vlastní scrollbar** (`scrollbar.rs`) — GPUI nemá virtualizovaný scroll out-of-box (pre GPUI 0.2), `uniform_list` musí být wrapped.

### Provider abstrakce (toto je pro nás zásadní)

```rust
pub trait GitProvider: Send + Sync + 'static {
    fn is_git_repo(&self) -> bool;
    fn get_diff(&self, mode: DiffMode, ignore_whitespace: bool) -> Result<DiffResult, String>;
    fn get_file_contents(&self, file_path: &str, mode: DiffMode) -> (Option<String>, Option<String>);
    fn get_diff_file_summary(&self) -> Vec<FileDiffSummary>;
    fn get_commit_graph(&self, count: usize, branch: Option<&str>) -> Vec<GraphRow>;
    fn list_branches(&self) -> Vec<String>;
    // mutations: stage/unstage/discard/delete — pro nás irelevantní (read-only)
}
```

`RemoteGitProvider` posílá `ActionRequest::GitDiff { project_id, mode, ignore_whitespace }` přes HTTP a deserializuje `DiffResult` (který je `Serialize`/`Deserialize`).

**Pro ZRemote:** tento pattern je přímo aplikovatelný. V Local mode GPUI volá agenta po HTTP (stejně jako pro všechno ostatní), v Server mode GUI volá server který proxuje na agenta přes WS. Můžeme použít naše existující `DispatchRouter`.

### Co převzít z Okeny

- **GitProvider trait** — přímo zkopírovat pattern Local/Remote split.
- **DiffMode enum** — `WorkingTree / Staged / Commit / BranchCompare`. Možná přidat `CommitRange { from, to }` pro review PR.
- **Expander rows** pro collapsed context — klíčové UX pro velké soubory.
- **Word-level changed ranges** — viditelný skok v kvalitě oproti line-level diff.
- **Pre-highlight celého souboru** přes syntect — je to správná strategie.
- **`validate_git_ref`** + **`safe_repo_path`** patterns — už máme podobnou ochranu v `zremote-agent/src/project/git.rs` přes `GIT_CEILING_DIRECTORIES`, ale string-level validace chybí.
- **Shell-out přístup** — viz sekce "Rust git libs" níže, doporučujeme zůstat u shell-out.

### Co NEpřevzít

- Vlastní parsování unified diffu — obsahuje edge cases (rename detection, binary blobs, submodules). Doporučujeme `gix-diff` nebo `imara-diff` s vlastním rendererem.
- Vlastní scrollbar — GPUI 0.2+ má lepší scrollbar podporu, v našem projektu už máme `terminal_element.rs` bez custom scrollbaru.
- Mutace (stage/unstage/discard/delete) — scope našeho RFC je read-only.
- Remote přes `ActionRequest` enum — my máme jiný protokol (WebSocket + `CommandRequest`/`CommandResult` v `zremote-protocol`).

---

## Arbor analýza

**Repo:** https://github.com/penso/arbor (Rust, GPUI, 699⭐)
**Popis:** "Run agentic coding workflows in a fully native desktop app for Git worktrees, terminals, and diffs." — to je doslova náš use case, jen Arbor předpokládá lokální agenty (claude, codex), my remote.

### Architektura

- `arbor-core` — git operace + daemon client. `changes.rs` používá **`gix`** (gitoxide) pro status/blobs + `gix-diff::blob::v2` (wrapper nad `imara-diff`) pro line counting.
- `arbor-gui` — GPUI view. Klíčové moduly:
  - `diff_engine.rs` (598 řádků) — buduje `Vec<DiffLine>` z `gix` + `imara-diff`, hunks s leading/trailing context, collapsed gap lines ("… N unchanged lines hidden").
  - `diff_view.rs` (586 řádků) — rendering přes `uniform_list` + side-by-side + **zonemap** (mini-mapa jako canvas).
  - `changes_pane.rs` (994 řádků) — levý panel se seznamem změněných souborů.
  - `file_view.rs` — full-file view pro jednotlivý soubor.
- Má **dual přístup**: používá jak `gix` tak `git2` (pro různé operace).

### Jak získává git data

```rust
// crates/arbor-core/src/changes.rs
use gix::status::{UntrackedFiles, index_worktree::iter::Summary};
use gix_diff::blob::v2::{Algorithm, Diff, InternedInput};

pub fn changed_files(repo_path: &Path) -> Result<Vec<ChangedFile>, ChangesError> {
    let repo = gix::open(repo_path)?;
    let status_iter = repo.status(gix::progress::Discard)?
        .untracked_files(UntrackedFiles::Files)
        .into_index_worktree_iter(Vec::<BString>::new())?;
    // iteruje přes items, spočte summary (Added/Modified/Removed/Renamed/...)
}

pub fn diff_line_stats(old: &[u8], new: &[u8]) -> DiffLineSummary {
    let input = InternedInput::new(old, new);
    let diff = Diff::compute(Algorithm::Histogram, &input);
    // sečte before/after rozsahy hunků
}
```

Pro čtení HEAD obsahu:
```rust
let spec = format!("HEAD:{rela_path}");
let obj = repo.rev_parse_single(&spec)?;
let blob = obj.object()?;
blob.data.to_vec()
```

### Jak renderuje diff

- `build_worktree_diff_document` prochází `ChangedFile[]`, pro každý volá `build_file_diff_lines` který čte HEAD blob přes `gix` a worktree soubor přes `fs::read`.
- `build_side_by_side_diff_lines`: `imara-diff` s `Algorithm::Histogram`, `postprocess_lines`, pak manuálně páruje hunks a vytváří `DiffLine { left_line_number, right_line_number, left_text, right_text, kind }`. Mezi hunky vkládá collapsed gap line (`… N unchanged lines hidden`).
- Rendering: **`uniform_list`** (GPUI virtualizovaný list) + paralelní **zonemap** na pravé straně.
  - Zonemap je vykreslena vlastním `canvas()` v GPUI: background track, barevné marker spans pro hunks (added/removed/modified), draggable thumb reflecting visible range.
  - Click/drag zonemapy scrolluje list přes `scroll_handle.scroll_to_item(target, ScrollStrategy::Center)`.
- **`ropey`** (Rope struktura) pro efektivní line-indexaci textu (O(log n) random access).
- **Wrap pre computed** — `estimated_diff_wrap_columns` počítá kolik znaků se vejde do šířky listu, diff je rebuilt při resize okna.
- **Async build:** celý `build_worktree_diff_document` běží v `cx.background_spawn()`, UI mezitím ukazuje "Computing diff..." loader.

### Syntax highlighting

Arbor **má** `syntect` v Cargo.toml, ale v `diff_engine.rs` / `diff_view.rs` ho nevyužívá — řádky renderuje jako plain text s barvou podle DiffLineKind (Context/Added/Removed/Modified). Syntax highlighting je v `file_view.rs` pro full-file view, ale diff view je bez něho. To je **ústupek oproti Okena**.

### Co převzít z Arboru

- **`gix` + `imara-diff`** pro line stats i diff generation — je to čistě Rust, bez libgit2 nativní závislosti.
- **Zonemap/minimap** přes `canvas()` — hodně užitečná UX feature pro velké diffy. Okena nic takového nemá.
- **Virtualizovaný `uniform_list`** — je to správná cesta pro velké diffy v GPUI.
- **Async build v `background_spawn`** — neblokuje hlavní thread, state store drží `is_loading: bool`.
- **Wrap na list width** — diff se rebuilduje při resize, ale je to správné chování pro nowrap-like kód.
- **Tests pro diff algoritmus** — viz konec `diff_engine.rs`: unit testy pokrývají modified, insert, remove, hidden gaps. Must-have.

### Co NEpřevzít

- **Absence syntax highlightingu v diff view** — to je jasný nedostatek, Okena to má lépe.
- **Dual `gix` + `git2`** — zbytečná komplexita pro nás. Zvol jedno, drž se toho.
- **Detached task pattern** (`.detach()` na `cx.spawn`) — ZRemote CLAUDE.md výslovně zakazuje detached long-lived tasks. Arbor fire-and-forgets load, což je OK pro jednorázový build, ale naše konvence říká držet `Task<()>` field.
- **HashMap<PathBuf, usize>** pro file row indices — pro velké diffy je OK, ale pro streamování diffu si to budeme muset rozmyslet (viz dále).

---

## Rust git libs: `gix` vs `git2` vs shell-out

### Varianty

| Varianta | Pro | Proti |
|---|---|---|
| **shell-out na `git` binary** (aktuální stav) | Žádné další závislosti, git je installed všude, výstupy stabilní (unified diff je specifikace), už máme robustní `run_git` s timeoutem a bezpečnostními env vars | Proces overhead (5-30ms per call), parsing unified diffu je fragile (rename, binary, submodule edge cases), závislost na nainstalovaném git |
| **`git2`** (libgit2 binding) | Mature, feature-complete, API kopíruje libgit2 znalost | Nativní C závislost (`openssl-sys`, `libgit2-sys`), křehké buildy (macOS/Linux/Windows trpí), `unsafe` pod kapotou, kompilační čas |
| **`gix`** (gitoxide, pure Rust) | Zero-C-deps, rychlejší než git2 pro většinu operací (blob read, status, diff), idiomatic Rust API, aktivně vyvíjený, použít s `gix-diff` + `imara-diff` | Mladší, občas chybějící features (některé obscure options), semver-breaking bumps jsou běžné, learning curve |

### Náš use case

- **Read-only diff**: working tree vs index, index vs HEAD, commit ranges, branch compare.
- **Remote případ**: agent běží na serveru, ne na developerovi. Diff se serializuje do JSON a posílá přes WS do GUI.
- **Velikost binárky**: ZRemote má už `sqlx`, `axum`, `alacritty_terminal`, `tokio` — každé MB počítá pro distribuci.

### Doporučení

**Zůstat u shell-out + přidat `gix-diff` (nebo `imara-diff` přímo) na client-side pro řízený rendering.**

Důvody:
1. **Už máme robustní `run_git`** s timeouty, `GIT_CEILING_DIRECTORIES`, disabled prompts. Kód je testován, bezpečný.
2. **Server-side je shell-out stejně dobrý** jako libgit2 — spawn je ~5ms, náš `inspect_fast` volá `git` 3× a nepozorovatelně.
3. **`git diff` output je stabilní standard** — unified diff je textově definovaný Git dokumentací. Parser má deterministic struct.
4. **Alternativa: agent shell-outuje pro získání raw diff textu, client-side v GUI parsuje**. Tímto izolujeme `git` dependency na server (kde je téměř jistě nainstalovaný) a GUI nemusí nic o git vědět.
5. **Arbor důkaz**: arbor-core má jak `gix` tak `git2`, ale pro raw blob čtení by stejně stačil `git show HEAD:path`. Přínos `gix` je tam hlavně pro iteraci statusu, což shell-out `git status --porcelain=v2` umí taky.

**Co doporučuju mít v agentu jako nové:**
- `git diff --no-color --no-ext-diff [mode-specific flags]` — už umíme spustit, stačí wrappers pro DiffMode.
- `git show <rev>:<path>` — pro full-file contents (už máme `get_file_from_git`-like logiku v Okeně jako reference).
- `git ls-files --others --exclude-standard` — pro untracked soubory (také z Okeny).

**Parser unified diffu**: napsat vlastní na základě Okena `parse_unified_diff` (240 řádků), ale přidat integration testy proti `git diff` výstupu na realistických repozitářích. Alternativně: **použít `gix-diff` jako parser** (pokud existuje `parse_unified_diff` API — to je nutné ověřit v implementační fázi, `gix-diff` je primárně generator diffu, ne parser).

**Alternativa pro generování diffu (client-side):** když agent pošle jen **raw texty** (old_blob + new_blob) pro každý soubor, můžeme client-side použít `imara-diff` s `Algorithm::Histogram` — jako to dělá Arbor. Výhoda: agent jen shell-outuje pro obsahy, diff algoritmus běží na GPUI. Nevýhoda: více dat po síti pro velké soubory (celý obsah místo diff-onlu).

**Kompromis** (doporučuji):
- Agent generuje **unified diff** (`git diff ...`) — to je kompaktní, stabilní, binary-safe.
- Agent posílá **old+new blob contents** jen pro soubory, které user aktivně otevře (lazy load).
- Client-side parsuje unified diff do strukturované formy (hunks, lines), použije blob contents pro syntax highlighting (potřebuje kompletní soubor pro správný state syntektu).

---

## Syntax highlighting v Rustu pro GPUI

### Kandidáti

| Kandidát | Pros | Cons |
|---|---|---|
| **`syntect`** (Sublime syntaxy) | Mature, ~200+ jazyků, široká Sublime community, používá Okena i Arbor | Bundlované syntax-def soubory jsou velké (~5MB), pomalejší než tree-sitter na velkých souborech, regex-based (může být pomalé u pathologických vstupů) |
| **`tree-sitter`** (incremental parser) | Velmi rychlý (O(n) parsing, incremental updates), přesný AST, používá Zed interně | Potřebuje grammar crate per-jazyk (`tree-sitter-rust`, `tree-sitter-typescript`, atd.) — každý je ~500KB wasm/dylib, komplexnější setup, theming je na uživateli |
| **`two-face`** | Wrapper nad syntect s předloženými barevnými schématy | Ještě jeden layer, pro náš use case spíš syntect přímo |
| **Zed's highlighting** | Pure tree-sitter-based, GPUI-native, performance proven | **Není publikovaný crate** — zed-industries/zed je GPL, naše licence (???) by to musela akceptovat; těžko extrahovat bez fork |

### Performance reality check

Pro typický diff (≤ 1000 změněných řádků na soubor, ≤ 50 souborů):
- Syntect full-file highlight: ~10-50ms per soubor (Rust, TypeScript). Při async `background_spawn` to nevadí.
- Tree-sitter parse: ~1-5ms per soubor. Highlightuje přes query (ještě rychlejší).

Pro extrémní případy (generated code 20K řádků, monorepos, atd.):
- Syntect začne trhat na single file > 5 MB. Tree-sitter zůstane rychlý.

### Doporučení

**Použít `syntect` s lazy loading a cappingem file size.**

Důvody:
1. **Okena + Arbor oba používají syntect** — dostatek real-world důkazu že to v GPUI funguje.
2. **Grammars out of the box** — žádný per-jazyk crate management.
3. **GPUI a tree-sitter** — GPUI 0.2 nemá first-party integration, museli bychom sami dělat highlight span → element mapping. Syntect už má `HighlightLines` API které vrací `Vec<(Style, &str)>`, snadno konvertovatelné na `HighlightedSpan { color: Rgba, text: String }`.
4. **Binární velikost**: vývojáři jsou OK s +5MB pro syntax highlighting. Pokud by to vadilo, `syntect` má feature flag pro "plist-load" vs "dump-load" — minified data dumps zmenší dependency.
5. **Zed má vlastní tree-sitter highlighting ale není reusable jako crate** — nepřepisovat.

**Cap**: soubor > 1 MB nebo > 10k řádků → skip syntax highlighting, ukaž plain text s diff-level barvami. Okena to řeší nepřímo (limit via UX), my můžeme explicitně.

**Theme**: syntect má built-in theme set. Měli bychom použít náš `theme::*()` color palette a mapovat syntect color tokens (Keyword, Function, String, Comment) na naše theme keys. Nepoužívat syntect bundled themes — stylisticky se neshodují s ZRemote theme.

---

## Velké diffy: strategie

### Reálné hranice

- **1K řádků na soubor**, **50 souborů**, **50K celkem** — typická feature PR. Žádný problém.
- **10K řádků na soubor, 500 souborů** — refactor PR, generated code. Potřeba virtualizace a lazy load.
- **> 100K řádků** — obvykle vendored code, lockfiles, build outputs. Zobrazit summary, nenabízet diff.

### Co dělá Arbor

- **`uniform_list`** — GPUI virtualizovaný list, renderuje jen viditelné řádky. Scroll je O(1).
- **Single flat list** — všechny soubory v jednom seznamu s file header rows. `HashMap<PathBuf, usize>` mapuje file path → první row pro "scroll to file".
- **Collapsed gap lines** — každá sekce unchanged lines > 3 se zbalí do "… N unchanged lines hidden". Bez možnosti rozbalit (v Arboru).
- **Wrap pre computed** — při resize okna se diff přebuduje. Async `background_spawn`, UI nebrání.
- **Zonemap** — color-coded minimap celého diffu na pravé straně, click to scroll.

### Co dělá Okena

- **Expander rows** — collapsed gaps jsou **klikatelné**, user si může rozbalit. UX vítězství nad Arborem.
- **Page-by-file** — navigation v levém panelu, centrální pane zobrazuje jen aktivní soubor. Menší paměť než flat list.
- **Vlastní scroll implementace** — ovládá vlastní virtualizaci, ne `uniform_list`.

### Doporučení pro nás

**Hybrid: per-file view + virtualized list + expander rows + zonemap.**

1. **Levý panel = file tree** (už máme sidebar pattern v `zremote-gui/src/sidebar.rs`). Seznam souborů s +/- stats, kliknutí otevře central pane.
2. **Centrální pane = virtualized `uniform_list`** jednoho souboru. Není potřeba all-files scroll (jako Arbor). Menší paměť, snazší streaming.
3. **Expander rows** pro collapsed context — Okena style. `DIFF_HUNK_CONTEXT_LINES = 3` default, user může klikem rozbalit.
4. **Zonemap na pravé straně aktuálního souboru** — Arbor style. Marker spans pro hunks, thumb pro visible range.
5. **Lazy loading diffu**:
   - Agent vrací `DiffSummary { files: Vec<FileStats> }` rychle (jen `git diff --stat`).
   - User klikne na soubor → agent vrací `FileDiff { hunks, blobs }` pro ten jeden soubor.
   - Pro syntax highlighting: agent posílá full-file blobs (`old_content`, `new_content`), client-side syntect highlightuje.
6. **Cap na file size**:
   - Soubor > 1000 hunks nebo > 5 MB blob → UI ukáže "File too large to diff, showing summary" + link "View on disk".
   - Binary soubor → UI ukáže "Binary file, changed" bez textu.
7. **Streaming pro extrémní diffy**: agent může posílat hunks postupně přes WS (future work, ne MVP).

### Virtualizace v GPUI

GPUI 0.2 má `uniform_list(id, count, move |range, ...| { ... })` — renderuje jen `range` rows. Klíčový constraint: **všechny rows musí mít stejnou výšku**. Pro diff to platí (`DIFF_ROW_HEIGHT_PX = ~20`), pro expander rows taky.

Arbor to řeší `UniformListScrollHandle` — drží `track_scroll` handle pro programmatic scroll. Okena má vlastní řešení (pre GPUI 0.2).

**Pro nás: použít `uniform_list`.** Je to stejná technika jako pro `terminal_element.rs` (per-row rendering s cache).

---

## Review komentáře (scope našeho RFC)

Nebyl jsem požádán zkoumat jak Okena/Arbor řeší review komentáře — **ani jeden je nemá**. Okena má stage/unstage/discard (mutations), Arbor má agentic workflow (claude/codex commands), ale žádný "inline review comment" pattern.

**Research gap** — toto je novum pro nás. Doporučuji se inspirovat **GitHub PR review** nebo **Gerrit Code Review** UX:
- Klikátelná řádka → inline textarea nad řádkou.
- Komentáře jsou stored per-file + line number.
- Při odeslání se payload (comments + approve/request-changes) předá agentovi (který to zpracuje jako agent feedback / bude persist to disk / vložit do promptu…).

Tohle nespadá do researche git diff clients — nech to na task #2 (UX design).

---

## Doporučené závislosti do Cargo.toml

**Pro `zremote-agent`** (server-side, rendering diffu):
```toml
# Žádné nové git deps — pokračuj v shell-out přes std::process::Command.
# Už máme `run_git` wrapper v src/project/git.rs.
```

**Pro `zremote-gui`** (client-side, parsing + rendering):
```toml
[dependencies]
# Parsing unified diff. Inspirace Okena, vlastní impl ~250 řádků.
# Alternativně: pokud agent pošle blobs a client-side computes diff:
imara-diff = "0.2"          # line/word diff algoritmy, Histogram/Myers
# ropey       = "1.6"        # rope struktura pro efektivní line access
#                            # (opt-in, pokud kód >5K řádků per soubor)

# Syntax highlighting
syntect = "5.3"              # ~5MB bundled syntaxes, ~200 languages
```

**Rozhodnutí mezi two přístupy**:

**A) Agent posílá unified diff text + blobs** (preferuji):
- Agent: shell-out `git diff` → unified text. Shell-out `git show HEAD:path` pro blobs.
- GUI: vlastní unified diff parser (250 řádků, insp. Okena). Žádné `imara-diff` potřeba.
- Payload: cca 2-4KB per typický modified file.
- Pros: jednoduché, agent posílá stabilní formát, GUI nedeps na git libs.

**B) Agent posílá jen blobs, GUI computes diff přes `imara-diff`**:
- Agent: shell-out `git show` pro blobs. Spočítá seznam modified souborů přes `git status`.
- GUI: `imara-diff::Histogram` na `(old_bytes, new_bytes)`. 
- Payload: větší (celé soubory) ale flexibilnější (user může přepnout algoritmus, word-level granularity na client).
- Pros: client má full kontrolu nad diff algoritmem, word-level highlighting je elegantní (re-diff per modified line).
- Cons: větší traffic, GUI má git-like závislost.

**Doporučuji A** pro MVP (menší payload, jednodušší), **B** jako future work pro word-level highlighting.

### Verze (s ohledem na aktuální workspace)

V našem `Cargo.toml` (workspace-level) dopsat:
```toml
[workspace.dependencies]
syntect = "5.3"
# imara-diff = "0.2"  # jen pokud zvolíme variantu B
```

V `crates/zremote-gui/Cargo.toml`:
```toml
[dependencies]
syntect = { workspace = true }
# imara-diff = { workspace = true }  # jen pokud B
```

---

## Shrnutí

**Klíčové takeaway patterns pro náš RFC:**

1. **GitProvider trait** (Okena pattern) — abstrahuje lokální vs remote. V Local mode volá přímo agent-side funkce, v Server mode proxuje na agenta přes stávající `DispatchRouter` / WebSocket.

2. **Agent shell-outuje `git diff`** — žádné nové git knihovny server-side. Rozšířit `zremote-agent/src/project/git.rs` o `DiffMode` enum + `get_diff` / `get_file_contents` funkce po vzoru Okena.

3. **GUI renderuje přes virtualizovaný `uniform_list`** (Arbor pattern) s file-header rows. Per-file pane (Okena-like navigation).

4. **Syntax highlighting přes `syntect`** (oba projekty) s pre-highlightem celého souboru (ne jen hunk lines). Map syntect token → `theme::*()` color.

5. **Collapsed context s klikatelnými expandery** (Okena) — lepší UX než fixed gap lines.

6. **Zonemap minimap** (Arbor) přes GPUI `canvas()` — klikátelná, draggable, color-coded spans.

7. **Lazy load**: agent vrací summary rychle, full file diff až na request. Cap na file size / hunk count.

8. **Async build v `background_spawn`** (Arbor) s explicit `Task<()>` field (naše CLAUDE.md konvence), ne `.detach()`.

9. **Security**: validace git refs (`validate_git_ref`, Okena), path traversal ochrana (`safe_repo_path`, Okena) — už částečně máme přes `GIT_CEILING_DIRECTORIES`, ale string-level validation pro branch/commit názvy chybí.

10. **Testing**: unit testy per-funkci (`parse_unified_diff`, `build_side_by_side_diff_lines`, expander pairing), integration testy proti realistickým diffům (`tempfile` repo + git init + commits + `git diff`).

Review komentáře — scope mimo tuto research, čeká se na UX design v task #2.

# RFC-007: Worktree-aware UX

**Status:** Approved — ready to implement
**Date:** 2026-04-17
**Builds on:** v0.13.3 worktree-aware project detection (`parent_project_id`, scanner, repair)

## Context

V0.13.3 rozpoznává linked worktrees a linkuje je na parent project. Sidebar je umí zobrazit. Tím končí backend-heavy fáze.

Druhá fáze je UX: udělat z worktrees first-class občany — rychlé přepínání, viditelná hierarchie, discoverable creation flow, bezpečný lifecycle, CLI parita. 4 UX designers (paralelní průzkum) identifikovali konkrétní akce, blockers a decision pointy. Tenhle RFC syntetizuje jejich reporty do phased implementation planu.

### Klíčové zjištění (blocker)

**Terminal panel dnes NEFILTRUJE sessions podle aktivního projektu** (`terminal_panel.rs` neobsahuje `project_id` filter). Dokud tohle neopravíme, worktree switcher nemá efekt — user "přepne" ale vidí stále stejné terminály. Phase 1 musí tohle vyřešit.

## Goals

1. Uživatel v sidebaru vidí hierarchii parent→worktrees, včetně branch, dirty state, ahead/behind.
2. Vytvoření worktree je discoverable z GUI (context menu, command palette, keyboard shortcut) i CLI.
3. Přepnutí kontextu (projekt/worktree) je rychlé (overlay, MRU) a správně filtruje terminals/knowledge/view state.
4. Lifecycle (delete, prune) má safety rails a je dostupný z GUI i CLI.
5. Mobile má stripped-down sadu operací (žádné destruktivní ops default).

## Non-goals (vyřadit z v1)

- Rename/move worktree (v2)
- Squash-merge detection s auto-fetch (v2 — zatím jen merge-commit check)
- Cross-worktree file-level akce ("open same file in worktree X")
- Custom `stale_threshold` UI (použít hardcoded 14d v v1)

## Architecture overview

```
zremote-gui
├─ views/
│  ├─ sidebar.rs                 MODIFY: hierarchický render, collapse, context menu
│  ├─ worktree_switcher.rs       NEW: overlay pro Cmd+Shift+W
│  ├─ project_detail.rs          MODIFY: Clean up panel (prune UI)
│  ├─ worktree_create_modal.rs   NEW: modal pro vytvoření
│  ├─ worktree_delete_modal.rs   NEW: modal pro bezpečné mazání
│  ├─ terminal_panel.rs          MODIFY: filtr sessions podle selected project_id
│  ├─ command_palette/           MODIFY: SwitchToWorktree, NewWorktree actions
│  └─ main_view.rs               MODIFY: breadcrumb v topbaru
├─ app_state.rs                  MODIFY: selected_project_id, expanded_projects, last_tab
├─ persistence.rs                MODIFY: RecentProject LRU
└─ icons.rs                      MODIFY: GitBranchPlus, GitMerge

zremote-agent
├─ local/routes/projects/
│  ├─ worktree.rs                MODIFY: base_ref, structured errors, prune endpoint
│  └─ git.rs                     MODIFY: branches list endpoint, fs-gone detection
├─ project/
│  ├─ scanner.rs                 MODIFY: periodic git-only refresh + missing-on-disk check
│  ├─ git.rs                     MODIFY: list_branches, check_merged, move_worktree(v2)
│  └─ repair.rs                  MODIFY: prune_missing_worktrees

zremote-core
├─ queries/
│  ├─ projects.rs                MODIFY: ORDER BY parent grouping; list_memories_with_parent
│  ├─ knowledge.rs               MODIFY: parent-fallback query
│  └─ ui_state.rs                NEW: project_ui_state table (last_tab, last_session_id)
└─ migrations/
   └─ 0XX_project_ui_state.sql   NEW

zremote-cli
└─ commands/worktree.rs          MODIFY: remove options, prune, show, shell; aliases

zremote-protocol
└─ events.rs                     MODIFY: WorktreeCreationProgress event
```

## Phased plan

### Phase 1 — Foundation (unblock switcher)

**Blocker fix + sidebar render + data freshness.**

1. **Terminal panel filter podle `selected_project_id`** — když je vybraný worktree, terminal panel ukazuje jen jeho sessions; ostatní sessions přesunout do "hidden" dropdown s countem.
2. **Hierarchický sidebar render** — `compute_items` vrací `ProjectNode { project, worktrees: Vec<ProjectNode>, sessions }`. Worktrees vždy pod parentem, ne flat. Indent 24, chevron pro collapse. Default collapsed pokud 4+ worktrees.
3. **Persistence** — `expanded_projects: HashSet<String>` v `persistence.rs`, restore při startu.
4. **Status badges** — dirty dot, ↑N/↓M labels používající existující `git_is_dirty`/`git_ahead`/`git_behind`. Žádné nové DB sloupce.
5. **Periodický git refresh** — nový task v agent (`project/git_refresh.rs`) co každých 30 s updatuje git fields pro **všechny registered projects na hostu**. Cost je bounded přes `LIMIT 1000` v SELECT + interval. Ne full scan (používá `inspect_fast`). GUI-visibility filtr byl v RFC zvažován jako další optimalizace, ale odložen na budoucí fázi — dnešní verze je cheaper než plný scan a funguje bez obousměrné signalizace expanded/visible stavu z GUI do agenta.
6. **Breadcrumb** — topbar `parent ▸ branch` když je vybraný worktree, clickable parent.

**Tests:** `compute_items` sort order, collapse persistence, terminal filter match.

### Phase 2 — Creation flow

1. **API rozšíření** — `CreateWorktreeRequest` + `base_ref: Option<String>`; `GitInspector::create_worktree` signatura. Structured error enum `WorktreeError { code, hint }` (branch_exists, path_collision, detached_head, locked, unmerged).
2. **Branch list endpoint** — `GET /api/projects/:id/git/branches` → `{ local, remote, current }` pro autocomplete a inline validaci.
3. **GUI `WorktreeCreateModal`** — fields: branch (autofocus, new/existing segmented), base ref (advanced, default HEAD), path (auto-suggest, live update), start-session checkbox (default ON).
4. **Discovery triggers** — right-click menu v sidebaru na parent row; `Icon::GitBranchPlus` hover action; command palette `New worktree`; keyboard shortcut.
5. **CLI upgrade** — `zremote wt` alias, positional branch, `--json`, `--interactive`, `--open`, `--dry-run`, `--base`.
6. **`WorktreeCreationProgress` event** — pro big repos (async job pattern), GUI pokrok modal.

**Tests:** base_ref round-trip, structured error mapping, modal keyboard flow, CLI `--dry-run`.

### Phase 3 — Switcher & context per worktree

1. **`WorktreeSwitcher` overlay** (`Cmd+Shift+W` default, konfigurovat) — fuzzy search worktrees aktuálního parenta; `Tab` rozšíří na all-worktrees; `Enter` přepne; `Create worktree…` jako poslední item.
2. **Command palette akce** — `PaletteAction::SwitchToWorktree` (integrace s MRU rankingem).
3. **Recent worktrees** — `persistence::RecentProject` LRU (max 20).
4. **`project_ui_state` tabulka** — (project_id PRIMARY, last_tab, last_session_id, updated_at). Restore při switch.
5. **Knowledge parent fallback** — `list_memories_with_parent` query, result obsahuje `origin_project_id` pro badge. Write jde na aktivní worktree.
6. **Session name collision** — detect duplicate name napříč worktrees sourozenci, prefix `branch · name` v tabu.

**Tests:** switcher fuzzy, MRU persistence, knowledge fallback SQL, ui_state restore.

### Phase 4 — Lifecycle

1. **DELETE endpoint rozšíření** — query params `?force&delete_branch&keep_dir`. `keep_dir=true` skipne `git worktree remove`.
2. **`WorktreeDeleteModal`** — checkboxy (remove-from-disk default ON, delete-branch default OFF, force když dirty), typed confirmation když destruktivní.
3. **Prune endpoint** — `POST /api/projects/:pid/worktrees/prune` body: `{ dry_run, criteria: ["missing", "merged", "stale_days"] }`. Returns list kandidátů + důvody.
4. **FS-gone detekce v scanneru** — nový sloupec `fs_missing_since` (nullable timestamp), scanner to označí; po 24h auto-prune nebo manual.
5. **Clean up panel** v project detail — samostatný view pro batch prune s preview.
6. **CLI rozšíření** — `wt remove` (interactive prompt, options), `wt prune` (criteria flags), `wt show`, `wt shell`.
7. **Stale detection (D7) — odložené z Phase 1.** Přidat sloupec `git_last_commit_at` do `projects` (migrace + backfill přes `inspect`), populovat v `inspect_fast` a `inspect` přes `git log -1 --format=%ct HEAD`. Sidebar render pak znovu zapne 60% opacity pro worktrees starší než 14 dní + Clean up panel stale-14d criteria. Phase 1 ship obsahuje stub `is_stale` vracející `false`, aby render path nevyžadoval další změnu až field dorazí.

**Tests:** delete query params, prune dry-run accuracy, FS-gone detection, CLI prompt flow.

### Phase 5 — Polish + mobile + deferred (v2 backlog)

- Mobile: stripped-down list + swipe overflow, destructive ops hidden by default.
- Merged detection (squash): `git cherry` + patch-id fallback, behind button.
- Rename/move worktree.
- Cross-worktree "run command here" akce.

## Decisions (approved 2026-04-17)

**D1 — Primary click action na worktree v sidebaru: Restore/open terminal.**
Klik obnoví poslední terminal session pro ten worktree, nebo otevře novou pokud žádná není. Match s power-user flow.

**D2 — Collapse default: Collapsed při 4+ worktrees.**
Auto-expand když některý child má aktivní session nebo běžící agentic loop. Persistence per-project přes `expanded_projects: HashSet<String>` v `persistence.rs`.

**D3 — Knowledge scope: Auto-fallback parent+worktree.**
Query vrací `IN (worktree, parent)` vždy. Každá memory má `origin_project_id` badge v Knowledge view. Zápis nové memory jde do aktivního worktree. Žádná konfigurace, žádný toggle.

**D4 — Keyboard shortcut pro switcher: `Cmd+K, w` (leader).**
Dvou-krok: `Cmd+K` otevře leader palette, `w` otevře worktree switcher. Rozšiřitelné pro další git akce (`Cmd+K, b` branch, `Cmd+K, p` project). Žádná dedikovaná jednokroková zkratka.

**D5 — Delete worktree s unmerged branch: Povolit s warning bannerem + jednoduché potvrzovací tlačítko.**
Modal zobrazí warning "branch is unmerged, N commits will be lost". Jen standardní `[Cancel] [Delete]` tlačítka (červené), **žádné typed confirmation** (na desktopu je to zbytečná friction). Safety stačí warning text.

**D6 — Mobile destructive ops: Povolit s extra potvrzením.**
Mobile má plnou sadu destruktivních akcí (remove from disk, delete branch, force), ale všechny vyžadují **typed confirmation** (user musí opsat branch name). Rozdíl oproti desktopu: na malém screenu je typed confirm nutný guard proti omýlu.

**D7 — Stale threshold: Hardcoded 14 dní.**
Worktree bez commitu 14+ dní = stale. Sidebar render @60% opacity, Clean up panel ho listuje jako prune kandidáta. Bez konfigurace v v1. Configurable až pokud přijde feedback.

## Critical files (read before implementing)

- `crates/zremote-gui/src/views/sidebar.rs:855-1322` — rendering, compute_items
- `crates/zremote-gui/src/views/terminal_panel.rs` — session panel (bez filtru!)
- `crates/zremote-gui/src/views/command_palette/` — action registry, ranking
- `crates/zremote-gui/src/app_state.rs`, `persistence.rs` — state, MRU infra
- `crates/zremote-agent/src/local/routes/projects/worktree.rs:42-401` — create + delete
- `crates/zremote-agent/src/project/git.rs` — GitInspector (helpery)
- `crates/zremote-agent/src/project/scanner.rs:77-198` — scan loop, repair
- `crates/zremote-core/src/queries/projects.rs`, `sessions.rs`, `knowledge.rs` — DB fns
- `crates/zremote-cli/src/commands/worktree.rs` — CLI subcommand

## Risks

- **Git operations latence** u big repos (monorepo) — async job + progress events nutné (Phase 2).
- **FS-gone auto-prune** po 24h může překvapit uživatele, který má worktree na externím disku — udělat konfigurovatelné, nebo default jen "označit, manual prune".
- **Knowledge fallback** může krást data mezi worktrees pokud user má sensitivní memories — řešit přes `origin_project_id` filter toggle v Knowledge view.
- **Protocol compat** — nová pole v `CreateWorktreeRequest`, nový endpoint `prune`, nový event `WorktreeCreationProgress` — vše přidávané, safe (rule z CLAUDE.md).

## Verification

- Unit testy pro každou phase (viz test list per-phase výše).
- End-to-end: vytvořit main repo + 3 worktrees, ověřit render, switch, create (GUI i CLI), delete, prune.
- Clippy + fmt clean.
- Visual check (`/visual-test`) pro sidebar hierarchii a switcher overlay.

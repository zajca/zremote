# RFC: Enhanced Project Intelligence -- Framework, Architecture & Convention Detection

**Status:** Draft
**Date:** 2026-03-31
**Author:** zajca
**Parent:** [RFC v0.10.0 Agent Intelligence](README.md) (Phase 4)
**Depends on:** -

---

## 1. Problem Statement

The current project scanner (`crates/zremote-agent/src/project/scanner.rs`) discovers projects by marker file **presence** only. It maps `Cargo.toml` to "rust", `package.json` to "node", `pyproject.toml` to "python". Three markers, three languages, zero depth.

This means:
- A Next.js project and a bare `npm init` project both show as "node"
- A Django monolith and a FastAPI microservice both show as "python"
- Monorepo vs single-package is invisible
- No convention data (linting, formatting, CI) is surfaced
- No package manager detection (npm vs pnpm vs yarn, pip vs uv vs poetry)

For Phase 6 (Context Delivery), the agent needs to inject project context into AI sessions. "rust" is too shallow -- "Rust/Axum with GPUI frontend, workspace monorepo, clippy pedantic" is actionable.

### Current scanner flow

```
ProjectScanner::detect_project(dir)
  -> check MARKERS array (3 entries: Cargo.toml, package.json, pyproject.toml)
  -> check .git directory
  -> build ProjectInfo { path, name, project_type, has_claude_config, ... }
  -> return (no file content is ever read)
```

### Current `ProjectInfo` (zremote-protocol)

```rust
pub struct ProjectInfo {
    pub path: String,
    pub name: String,
    pub has_claude_config: bool,
    pub has_zremote_config: bool,
    pub project_type: String,       // "rust" | "node" | "python" | "unknown"
    pub git_info: Option<GitInfo>,
    pub worktrees: Vec<WorktreeInfo>,
}
```

No fields for frameworks, architecture, conventions, or package manager.

---

## 2. Goals

- **Read marker file contents** to extract framework, architecture, and convention data
- **Extend** `ProjectInfo` with new optional fields (backwards-compatible via `#[serde(default)]`)
- **Extend** the DB schema with new columns (nullable, additive migration)
- **Persist** enriched data through the existing scan -> upsert -> REST pipeline
- **Keep detection fast** -- only read files already proven to exist (markers), parse minimally
- **Add markers** for Go (`go.mod`) and PHP (`composer.json`) languages
- **Unit-testable** -- framework detection logic in pure functions, no filesystem in tests

### Non-goals

- Deep AST analysis or source code parsing
- Runtime detection (running `npm ls`, `cargo metadata`, etc.)
- Automatic convention enforcement
- Language server integration

---

## 3. Design

### 3.1 Extended `ProjectInfo`

In `crates/zremote-protocol/src/project.rs`, add fields to the existing `ProjectInfo` struct. **All new fields MUST use `#[serde(default)]`** for backward compatibility. `Vec` fields default to empty, `Option` fields default to `None`.

```rust
/// Architecture pattern detected for the project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArchitecturePattern {
    MonorepoPnpm,
    MonorepoLerna,
    MonorepoNx,
    MonorepoTurborepo,
    MonorepoCargo,
    Mvc,
    Microservices,
    Monolith,
    #[serde(other)]
    Unknown,
}

/// A detected convention (linter, formatter, test framework, build tool).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Convention {
    /// Category of convention.
    pub kind: ConventionKind,
    /// Name identifier, e.g. "eslint", "clippy", "github_actions".
    pub name: String,
    /// Config file that triggered detection, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConventionKind {
    Linter,
    Formatter,
    TestFramework,
    BuildTool,
    #[serde(other)]
    Unknown,
}

pub struct ProjectInfo {
    // Existing fields (unchanged)
    pub path: String,
    pub name: String,
    pub has_claude_config: bool,
    #[serde(default)]
    pub has_zremote_config: bool,
    pub project_type: String,
    #[serde(default)]
    pub git_info: Option<GitInfo>,
    #[serde(default)]
    pub worktrees: Vec<WorktreeInfo>,

    // NEW fields (all optional, backwards-compatible)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frameworks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<ArchitecturePattern>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conventions: Vec<Convention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_manager: Option<String>,
}
```

All new fields use `#[serde(default)]` so old agents sending without these fields will deserialize correctly on new servers (and vice versa). `Vec` fields default to `vec![]`, `Option` fields default to `None`.

### 3.2 New Scanner Module: `intelligence.rs`

New file: `crates/zremote-agent/src/project/intelligence.rs`

This module contains pure functions that read marker file contents and extract structured intelligence. The scanner calls these after detecting a project.

```rust
/// Result of deep-scanning a project directory for framework/architecture/convention data.
#[derive(Debug, Default)]
pub struct ProjectIntelligence {
    pub frameworks: Vec<String>,
    pub architecture: Option<ArchitecturePattern>,
    pub conventions: Vec<Convention>,
    pub package_manager: Option<String>,
}

/// Analyze a project directory. Reads marker files that are known to exist.
/// Called after basic detection confirms the project type.
pub fn analyze(dir: &Path, project_type: &str) -> ProjectIntelligence;

/// Detect frameworks from marker file contents.
fn detect_frameworks(dir: &Path, project_type: &str) -> Vec<String>;

/// Detect architecture pattern from directory structure and config files.
fn detect_architecture(dir: &Path, project_type: &str) -> Option<ArchitecturePattern>;

/// Detect conventions (linter, formatter, CI, testing).
fn detect_conventions(dir: &Path) -> Vec<Convention>;

/// Detect package manager from lock files and config.
fn detect_package_manager(dir: &Path, project_type: &str) -> Option<String>;
```

### 3.3 Framework Detection

Read the dependency section of marker files. Use simple string matching on dependency names -- no full TOML/JSON parser needed for key presence checks, but we use `serde_json` (already a dependency) for JSON and basic TOML key scanning for TOML files.

| Marker | Language | Dependency key | Framework mappings |
|--------|----------|----------------|--------------------|
| `package.json` | node | `dependencies` + `devDependencies` | `next` -> "Next.js", `react` -> "React", `vue` -> "Vue", `svelte` -> "Svelte", `@angular/core` -> "Angular", `express` -> "Express", `nestjs` -> "NestJS", `nuxt` -> "Nuxt" |
| `Cargo.toml` | rust | `[dependencies]` | `axum` -> "Axum", `actix-web` -> "Actix", `gpui` -> "GPUI", `rocket` -> "Rocket", `warp` -> "Warp", `tauri` -> "Tauri", `bevy` -> "Bevy", `leptos` -> "Leptos" |
| `pyproject.toml` | python | `[project.dependencies]` or `[tool.poetry.dependencies]` | `django` -> "Django", `fastapi` -> "FastAPI", `flask` -> "Flask", `starlette` -> "Starlette", `sqlalchemy` -> "SQLAlchemy" |
| `requirements.txt` | python | Line-by-line package names (strip version specifiers) | Same mappings as above. Fallback when no `pyproject.toml` exists |
| `setup.py` | python | Not parsed for frameworks (too complex). Language detection only | - |
| `go.mod` | go | `require` block | `github.com/gin-gonic/gin` -> "Gin", `github.com/gofiber/fiber` -> "Fiber", `github.com/labstack/echo` -> "Echo", `github.com/gorilla/mux` -> "Gorilla" |
| `composer.json` | php | `require` | `symfony/` -> "Symfony", `laravel/framework` -> "Laravel", `slim/slim` -> "Slim" |

**Implementation strategy for each format:**

- **JSON** (`package.json`, `composer.json`): Parse with `serde_json::Value`, iterate dependency keys.
- **TOML** (`Cargo.toml`, `pyproject.toml`): Read file as string, scan for dependency names after `[dependencies]` / `[project.dependencies]` section headers. Use simple line-by-line scanning -- no TOML parser dependency needed. Each line matching `^dep_name\s*=` or `^dep_name\s*\{` is a hit.
- **go.mod**: Read file as string, scan `require (...)` block for module paths.
- **requirements.txt**: Read file as string, line-by-line. Strip version specifiers (`==`, `>=`, `~=`, etc.) and extras (`[dev]`). Match package name against framework mappings.

**Bounded reads**: Read at most 64KB of each marker file. This covers any reasonable manifest.

### 3.4 Architecture Detection

Check for structural patterns by probing specific paths (directory existence, file existence). No recursive directory walking.

**Monorepo member counting:** For monorepo patterns that need member counts, read the workspace config file at the project root rather than recursing the filesystem:
- **Cargo workspaces:** Read `Cargo.toml`, parse `[workspace].members` array (glob patterns). Count the patterns -- no filesystem expansion needed.
- **pnpm workspaces:** Read `pnpm-workspace.yaml`, parse `packages` array. Count the entries.
- **Lerna/Nx/Turborepo:** Read the respective config file for packages/projects array.

**Max recursion depth:** If any detection logic requires directory traversal (e.g., checking for subdirectory manifests in microservices detection), enforce a max recursion depth of **5 levels** from the project root.

| Pattern | Detection criteria |
|---------|--------------------|
| `MonorepoPnpm` | `pnpm-workspace.yaml` exists |
| `MonorepoLerna` | `lerna.json` exists |
| `MonorepoNx` | `nx.json` exists |
| `MonorepoTurborepo` | `turbo.json` exists |
| `MonorepoCargo` | `Cargo.toml` contains `[workspace]` and has >3 members (read from `[workspace].members` array, no recursion needed) |
| `Mvc` | At least 2 of: `controllers/`, `models/`, `views/` directories exist |
| `Microservices` | `docker-compose.yml` or `docker-compose.yaml` exists with >3 services, AND separate subdirectories each containing their own manifest (`package.json`, `Cargo.toml`, etc.). Multiple Dockerfiles alone do NOT qualify. |
| `Monolith` | Single project root with no workspace/monorepo markers (default, not stored -- absence of pattern) |

Return the **first matching** pattern (checked in the order above). `None` if no pattern matches (treated as single-project/monolith).

The architecture value is serialized as snake_case via `#[serde(rename_all = "snake_case")]` on the `ArchitecturePattern` enum: `"monorepo_pnpm"`, `"monorepo_cargo"`, `"mvc"`, `"microservices"`, etc.

### 3.5 Convention Detection

Probe for well-known config files. Return a list of `Convention` structs with `kind`, `name`, and optional `config_file`.

| Convention name | Kind | Detection | `config_file` |
|-----------------|------|-----------|----------------|
| `eslint` | `Linter` | `.eslintrc*` or `eslint.config.*` exists | detected file path |
| `prettier` | `Formatter` | `.prettierrc*` or `prettier.config.*` exists | detected file path |
| `clippy` | `Linter` | `clippy.toml` or `.clippy.toml` exists, or `Cargo.toml` contains `[lints.clippy]` | detected file path |
| `rustfmt` | `Formatter` | `rustfmt.toml` or `.rustfmt.toml` exists | detected file path |
| `ruff` | `Linter` | `ruff.toml` or `pyproject.toml` contains `[tool.ruff]` | detected file path |
| `black` | `Formatter` | `pyproject.toml` contains `[tool.black]` | `pyproject.toml` |
| `github_actions` | `BuildTool` | `.github/workflows/` directory exists | `None` |
| `docker` | `BuildTool` | `Dockerfile` or `docker-compose.yml` exists | detected file path |
| `ci_gitlab` | `BuildTool` | `.gitlab-ci.yml` exists | `.gitlab-ci.yml` |
| `editorconfig` | `Formatter` | `.editorconfig` exists | `.editorconfig` |
| `pre_commit` | `Linter` | `.pre-commit-config.yaml` exists | `.pre-commit-config.yaml` |
| `husky` | `Linter` | `.husky/` directory exists | `None` |
| `biome` | `Linter` | `biome.json` or `biome.jsonc` exists | detected file path |
| `typescript` | `BuildTool` | `tsconfig.json` exists | `tsconfig.json` |

Return sorted (by `name`), deduplicated list.

### 3.6 Package Manager Detection

| Language | Detection order |
|----------|----------------|
| node | `pnpm-lock.yaml` -> "pnpm", `yarn.lock` -> "yarn", `bun.lockb` -> "bun", `package-lock.json` -> "npm" |
| python | `uv.lock` -> "uv", `poetry.lock` -> "poetry", `Pipfile.lock` -> "pipenv", `requirements.txt` -> "pip" |
| rust | Always "cargo" (single package manager) |
| go | Always "go" (single package manager) |
| php | Always "composer" (single package manager) |

Check lock file existence in order; first match wins.

### 3.7 Scanner Integration

Modify `ProjectScanner::detect_project()` to call `intelligence::analyze()` after basic detection:

```rust
// In detect_project(), after building the initial ProjectInfo:
let intel = intelligence::analyze(dir, project_type.unwrap_or("unknown"));

Some(ProjectInfo {
    path: dir.to_string_lossy().to_string(),
    name,
    has_claude_config,
    has_zremote_config,
    project_type: project_type.unwrap_or("unknown").to_string(),
    git_info,
    worktrees,
    // NEW
    frameworks: intel.frameworks,
    architecture: intel.architecture,
    conventions: intel.conventions,
    package_manager: intel.package_manager,
})
```

### 3.8 Extended Markers

**Prerequisite:** The `MARKERS` constant in `crates/zremote-agent/src/project/scanner.rs` must be extended before intelligence detection can work for Go and PHP projects. This is step 6 in the implementation phases.

Add new markers to the `MARKERS` array:

```rust
const MARKERS: &[(&str, &str)] = &[
    ("Cargo.toml", "rust"),
    ("package.json", "node"),
    ("pyproject.toml", "python"),
    ("requirements.txt", "python"),   // NEW - fallback for Python projects without pyproject.toml
    ("setup.py", "python"),           // NEW - legacy Python projects
    ("go.mod", "go"),                 // NEW
    ("composer.json", "php"),         // NEW
];
```

**Note on Python markers:** `requirements.txt` and `setup.py` are added as fallback markers for Python projects that do not use `pyproject.toml`. The scanner already deduplicates by directory, so a project with both `pyproject.toml` and `requirements.txt` is detected only once. Framework detection for `requirements.txt`-only projects is limited (no structured dependency data), but language detection and convention scanning still apply.

### 3.9 Database Migration

New migration: `crates/zremote-core/migrations/020_project_intelligence.sql`

```sql
-- Add project intelligence columns
ALTER TABLE projects ADD COLUMN frameworks TEXT DEFAULT '[]';
ALTER TABLE projects ADD COLUMN architecture TEXT DEFAULT NULL;
ALTER TABLE projects ADD COLUMN conventions TEXT DEFAULT '[]';
ALTER TABLE projects ADD COLUMN package_manager TEXT DEFAULT NULL;
```

- `frameworks`: JSON array of strings, e.g. `'["Next.js","React"]'`
- `architecture`: nullable string, e.g. `'monorepo_pnpm'`
- `conventions`: JSON array of strings, e.g. `'["eslint","prettier","typescript"]'`
- `package_manager`: nullable string, e.g. `'pnpm'`

### 3.10 Query Layer Updates

In `crates/zremote-core/src/queries/projects.rs`:

1. Add 4 new fields to `ProjectRow`. **All new fields MUST use `#[serde(default)]`** since old database rows will not have these columns populated:

```rust
pub struct ProjectRow {
    // ... existing fields ...

    #[serde(default)]
    pub frameworks: Option<String>,     // JSON array string, e.g. '["Next.js","React"]'
    #[serde(default)]
    pub architecture: Option<String>,   // snake_case enum value, e.g. "monorepo_cargo"
    #[serde(default)]
    pub conventions: Option<String>,    // JSON array of Convention structs
    #[serde(default)]
    pub package_manager: Option<String>,// e.g. "pnpm"
}
```

2. Update `PROJECT_COLUMNS` constant to include the new columns in the SELECT list: `frameworks, architecture, conventions, package_manager`.

3. Update the `From<ProjectRow> for ProjectInfo` conversion (or equivalent mapping) to deserialize JSON strings back into `Vec<String>`, `Option<ArchitecturePattern>`, `Vec<Convention>`. Use `serde_json::from_str().unwrap_or_default()` for Vec fields to handle malformed data gracefully.

### 3.11 Scan Route Updates

In `crates/zremote-agent/src/local/routes/projects/scan.rs`, extend the UPDATE query in `trigger_scan()` to persist the new fields:

```rust
sqlx::query(
    "UPDATE projects SET project_type = ?, has_claude_config = ?, has_zremote_config = ?, \
     git_branch = ?, git_commit_hash = ?, git_commit_message = ?, \
     git_is_dirty = ?, git_ahead = ?, git_behind = ?, git_remotes = ?, git_updated_at = ?, \
     frameworks = ?, architecture = ?, conventions = ?, package_manager = ? \
     WHERE id = ?",
)
// ... existing binds ...
.bind(serde_json::to_string(&info.frameworks).unwrap_or_default())
.bind(&info.architecture)
.bind(serde_json::to_string(&info.conventions).unwrap_or_default())
.bind(&info.package_manager)
```

Same update in `crates/zremote-agent/src/local/routes/projects/crud.rs` `add_project()`.

### 3.12 Server-mode Sync

In `crates/zremote-agent/src/connection/dispatch.rs` and `crates/zremote-core/`, the `ProjectInfo` flows through `AgentMessage::ProjectList`. Since we are adding `#[serde(default)]` fields to `ProjectInfo`, old agents will send without these fields and new servers will deserialize them as empty/None. New agents will send the fields and old servers will ignore them (serde default behavior with `deny_unknown_fields` not set). No protocol version bump needed.

---

## 4. Files

### CREATE

| File | Description |
|------|-------------|
| `crates/zremote-agent/src/project/intelligence.rs` | Framework, architecture, convention, and package manager detection logic |
| `crates/zremote-core/migrations/020_project_intelligence.sql` | Add 4 new columns to projects table |

### MODIFY

| File | Change |
|------|--------|
| `crates/zremote-protocol/src/project.rs` | Add `ArchitecturePattern` enum, `Convention` struct, `ConventionKind` enum; add `frameworks`, `architecture`, `conventions`, `package_manager` fields to `ProjectInfo`; add backward compat test; update roundtrip test |
| `crates/zremote-agent/src/project/mod.rs` | Add `pub mod intelligence;` |
| `crates/zremote-agent/src/project/scanner.rs` | Import `intelligence`, call `analyze()` in `detect_project()`, add go.mod and composer.json markers |
| `crates/zremote-core/src/queries/projects.rs` | Add 4 fields to `ProjectRow`, update `PROJECT_COLUMNS` |
| `crates/zremote-agent/src/local/routes/projects/scan.rs` | Persist new fields in UPDATE query |
| `crates/zremote-agent/src/local/routes/projects/crud.rs` | Persist new fields in add_project UPDATE query |

---

## 5. Implementation Phases

This is a single-phase implementation (low-medium complexity).

### Phase 1: Intelligence module + protocol + DB + wiring

1. Create `intelligence.rs` with `analyze()`, `detect_frameworks()`, `detect_architecture()`, `detect_conventions()`, `detect_package_manager()`
2. Add new fields to `ProjectInfo` in protocol crate
3. Create migration `020_project_intelligence.sql`
4. Add new fields to `ProjectRow` and update `PROJECT_COLUMNS`
5. Wire `intelligence::analyze()` into `ProjectScanner::detect_project()`
6. Add go.mod and composer.json markers
7. Update scan and crud routes to persist new fields
8. Write unit tests for all detection functions
9. Update existing scanner tests to verify new fields

**Estimated scope:** ~400 lines of new code (intelligence.rs), ~50 lines of modifications across existing files.

---

## 6. Risk Assessment

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Marker file read slows down scan | Medium | Low | Read max 64KB per file, only files already confirmed to exist. Current scan takes ~50ms for ~20 projects; reading 3-5 small config files per project adds negligible I/O |
| Manifest reading I/O cost per project | Medium | Medium | **Lazy parsing:** read and parse manifests only when a project is accessed via API (e.g., project detail endpoint), not during the initial directory scan. Cache parsed results with file mtime check -- re-parse only if the file changed since last read. Initial scan detects language/markers only; intelligence is populated on first access |
| TOML parsing false positives | Low | Medium | Line-by-line scanning may match commented-out dependencies. Acceptable -- false positive framework is harmless, and commented deps are rare in practice |
| Malformed manifests (invalid JSON, TOML parse errors) | Low | Medium | Silently skip with `WARN`-level log. Return partial results -- languages detected, frameworks/conventions empty. Never fail the entire scan due to one bad manifest. Use `serde_json::from_str().ok()` and equivalent for TOML |
| New DB columns break old agent | Low | None | All columns are nullable with defaults. Old agents never SELECT these columns. Migration is additive |
| New markers (go.mod, composer.json) match directories that aren't really projects | Low | Low | Same risk as existing markers; go.mod and composer.json are highly specific to Go/PHP projects |
| Architecture detection false positives | Medium | Medium | Multiple Dockerfiles alone do NOT indicate microservices. Require: separate subdirectories with their own manifests + docker-compose with >3 services. See section 3.4 for refined criteria |

---

## 7. Protocol Compatibility

| Change | Safe? | Reason |
|--------|-------|--------|
| New optional fields on `ProjectInfo` with `#[serde(default)]` | Yes | Old deserializers ignore unknown fields; new deserializers default missing fields |
| New DB columns with defaults | Yes | Old code never selects/inserts these columns; SQLite applies defaults |
| New marker types ("go", "php") | Yes | `project_type` is already a free-form string, UI already handles unknown types gracefully |
| No new message types | Yes | Uses existing `AgentMessage::ProjectList` |

No protocol version bump required. No deployment order constraints.

---

## 8. Testing

### Unit Tests (in `intelligence.rs`)

| Test | Description |
|------|-------------|
| `detect_nextjs_from_package_json` | package.json with `"next"` in dependencies -> frameworks contains "Next.js" |
| `detect_react_from_package_json` | package.json with `"react"` -> frameworks contains "React" |
| `detect_vue_from_package_json` | package.json with `"vue"` -> frameworks contains "Vue" |
| `detect_angular_from_package_json` | package.json with `"@angular/core"` -> "Angular" |
| `detect_express_from_package_json` | package.json with `"express"` -> "Express" |
| `detect_axum_from_cargo_toml` | Cargo.toml with `axum = "0.8"` -> frameworks contains "Axum" |
| `detect_gpui_from_cargo_toml` | Cargo.toml with `gpui` dependency -> "GPUI" |
| `detect_multiple_rust_frameworks` | Cargo.toml with both axum and gpui -> both detected |
| `detect_django_from_pyproject` | pyproject.toml with django dependency -> "Django" |
| `detect_fastapi_from_pyproject` | pyproject.toml with fastapi -> "FastAPI" |
| `detect_gin_from_go_mod` | go.mod with `github.com/gin-gonic/gin` -> "Gin" |
| `detect_laravel_from_composer` | composer.json with `laravel/framework` -> "Laravel" |
| `detect_symfony_from_composer` | composer.json with `symfony/framework-bundle` -> "Symfony" |
| `detect_monorepo_pnpm` | pnpm-workspace.yaml exists -> architecture "monorepo_pnpm" |
| `detect_monorepo_cargo` | Cargo.toml with [workspace] and >3 members -> "monorepo_cargo" |
| `detect_mvc_pattern` | controllers/ + models/ + views/ dirs -> "mvc" |
| `detect_microservices` | docker-compose.yml with >3 services -> "microservices" |
| `detect_no_architecture` | Plain project -> architecture is None |
| `detect_eslint_convention` | .eslintrc.json exists -> conventions contains "eslint" |
| `detect_prettier_convention` | .prettierrc exists -> "prettier" |
| `detect_clippy_convention` | clippy.toml exists -> "clippy" |
| `detect_github_actions` | .github/workflows/ exists -> "github_actions" |
| `detect_typescript` | tsconfig.json exists -> "typescript" |
| `detect_multiple_conventions` | Multiple config files -> all detected, sorted |
| `detect_pnpm_package_manager` | pnpm-lock.yaml exists -> "pnpm" |
| `detect_yarn_package_manager` | yarn.lock exists -> "yarn" |
| `detect_npm_package_manager` | package-lock.json exists -> "npm" |
| `detect_uv_package_manager` | uv.lock exists -> "uv" |
| `detect_poetry_package_manager` | poetry.lock exists -> "poetry" |
| `detect_pip_package_manager` | requirements.txt exists, no other lock -> "pip" |
| `detect_cargo_package_manager` | Rust project -> "cargo" |
| `detect_go_package_manager` | Go project -> "go" |
| `no_frameworks_for_empty_deps` | package.json with empty dependencies -> empty frameworks list |
| `malformed_json_returns_empty` | Invalid package.json -> empty frameworks, no panic |

### Protocol Backward Compatibility Tests (in `project.rs` existing test module)

| Test | Description |
|------|-------------|
| `project_info_backward_compat_without_intelligence_fields` | Deserialize a `ProjectInfo` JSON that has NO `frameworks`, `architecture`, `conventions`, or `package_manager` fields. Verify: `frameworks` defaults to `vec![]`, `architecture` to `None`, `conventions` to `vec![]`, `package_manager` to `None`. Analogous to existing `project_info_backward_compat_without_git_fields` test. |

### Scanner Integration Tests (in `scanner.rs` existing test module)

| Test | Description |
|------|-------------|
| `detect_project_with_frameworks` | Create tempdir with Cargo.toml containing axum dep -> ProjectInfo has frameworks ["Axum"] |
| `detect_project_go` | Create tempdir with go.mod -> project_type "go" |
| `detect_project_php` | Create tempdir with composer.json -> project_type "php" |

### Query Tests (in `projects.rs` existing test module)

| Test | Description |
|------|-------------|
| `project_intelligence_columns_default` | Insert project, verify frameworks/architecture/conventions/package_manager default to empty/null |
| `project_intelligence_columns_persist` | Update project with intelligence data, re-read and verify |

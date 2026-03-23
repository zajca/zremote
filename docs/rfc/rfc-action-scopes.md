# RFC: Action Scopes - Context-Aware Script Actions

## Problem Statement

Script actions currently have only a primitive `worktree_scoped: bool` flag that determines whether an action appears in the "Project Actions" or "Worktree Actions" section. This provides no control over WHERE actions appear in the UI beyond that binary split.

Users want actions to appear in different UI contexts: the sidebar for quick access, the command palette for keyboard-driven workflows, or specific sections of the project page. The current boolean flag cannot express these requirements.

## Scope Definitions

| Scope | Where it appears | Example use case |
|-------|-----------------|------------------|
| `project` | ActionsTab "Project Actions" section | `cargo build --workspace` |
| `worktree` | WorktreeCard + ActionsTab "Worktree Actions" | `bun install` in worktree dir |
| `sidebar` | ProjectItem hover buttons in sidebar | Quick build/test shortcuts |
| `command_palette` | Cmd+K command palette | Any globally discoverable action |

An action can have multiple scopes simultaneously.

## Data Model Changes

### Rust (protocol crate)

New enum:
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActionScope {
    Project,
    Worktree,
    Sidebar,
    CommandPalette,
}
```

New field on `ProjectAction`:
```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub scopes: Vec<ActionScope>,
```

### Helper functions (Rust)

```
effective_scopes(action) -> Vec<ActionScope>
has_scope(action, scope) -> bool
```

## Backward Compatibility

`worktree_scoped: bool` remains in the struct but is superseded by `scopes` when present:

| Condition | Effective scopes |
|-----------|-----------------|
| `scopes` non-empty | Use `scopes` as-is, ignore `worktree_scoped` |
| `scopes` empty + `worktree_scoped: true` | `["worktree", "command_palette"]` |
| `scopes` empty + `worktree_scoped: false` | `["project", "command_palette"]` |

Old `.zremote/settings.json` files without `scopes` field continue to work unchanged. The `scopes` field uses `#[serde(default)]` so it deserializes as empty vec when absent.

## Example settings.json

```json
{
  "actions": [
    {
      "name": "Build",
      "command": "cargo build --workspace",
      "icon": "hammer",
      "scopes": ["project", "sidebar", "command_palette"]
    },
    {
      "name": "Quick Test",
      "command": "cargo test",
      "icon": "test-tube",
      "scopes": ["sidebar"]
    },
    {
      "name": "Setup Worktree",
      "command": "bun install",
      "scopes": ["worktree"]
    },
    {
      "name": "Legacy Action",
      "command": "echo hello",
      "worktree_scoped": false
    }
  ]
}
```

## UI Changes Per Scope

### Project scope
Actions with `project` scope appear in the ActionsTab "Project Actions" section. No visual change from current behavior.

### Worktree scope
Actions with `worktree` scope appear in ActionsTab "Worktree Actions" section and on WorktreeCard components. The `resolve_working_dir` function uses worktree path when action has `worktree` scope.

### Sidebar scope
Actions with `sidebar` scope appear as hover buttons on ProjectItem in the sidebar:
- 1 action: direct icon button
- 2+ actions: grouped behind a Zap icon dropdown menu

### Command palette scope
Actions with `command_palette` scope appear in the Cmd+K command palette. Both project-level and worktree-level command palette builders filter by this scope.

### Settings editor
The `worktree_scoped` checkbox is replaced with scope toggle pills. Each scope is a small pill button that can be toggled on/off. Labels: Project, Worktree, Sidebar, Palette.

## Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| Breaking existing settings.json | High | `scopes` defaults to empty, fallback to `worktree_scoped` logic |
| Protocol mismatch agent/server | Medium | New optional field with `#[serde(default)]`, safe per protocol rules |
| Scope confusion for users | Low | Clear labels, sensible defaults from legacy field |

# RFC: Scriptable Action Inputs

## Context & Problem Statement

Actions in ZRemote currently only support static `command` strings with a few built-in template variables (`{{worktree_path}}`, `{{branch}}`, `{{worktree_name}}`, `{{project_path}}`). The frontend auto-detects missing worktree/branch vars and shows a small popover (`ActionInputPopover`).

Users need a general-purpose input system for actions where:
- Any action can define custom input fields (text, select, multiline)
- Select inputs can have a `script` that runs on the backend to dynamically generate options
- Script output provides options with value + optional label (like rofi)
- Example: "Create Release" action has a `tag` select input with a script that outputs next semver versions

## Design Decisions

1. **New `ActionInput` type** (not extending `PromptInput`) - includes `script` field. Reuses existing `PromptInputType` enum (Text/Multiline/Select) to avoid duplication.
2. **Per-action resolve endpoint** - `POST /api/projects/:id/actions/:name/resolve-inputs` resolves all scripted inputs for one action in a single call.
3. **New protocol messages** for server mode - `ResolveActionInputs` / `ActionInputsResolved` following existing request/response pattern.
4. **No input dependencies in v1** - all scripts run independently when dialog opens.
5. **Same security model** - scripts come from `.zremote/settings.json`, same trust as action commands.
6. **ActionInputDialog handles worktree/branch too** - when action has custom inputs AND uses `{{worktree_path}}`/`{{branch}}` template vars, the dialog includes worktree selector and/or branch input alongside custom inputs. This replaces ActionInputPopover for such actions.
7. **Command Palette integration** - actions with inputs open ActionInputDialog from palette, same pattern as `openRunPrompt` for prompt templates.

## Script Output Format

One option per line. Tab-separated `value\tlabel`. If no tab, value = label. Lines starting with `#` or empty lines ignored.

```
0.2.4	Patch release
0.3.0	Minor release
1.0.0	Major release
```

## Settings.json Example

```json
{
  "name": "Create Release",
  "command": "git tag -a {{tag}} -m '{{message}}' && git push origin {{tag}}",
  "icon": "tag",
  "inputs": [
    {
      "name": "tag",
      "label": "Next tag",
      "input_type": "select",
      "script": "scripts/next-versions.sh"
    },
    {
      "name": "message",
      "label": "Release message",
      "input_type": "text",
      "placeholder": "Release notes..."
    }
  ]
}
```

## Architecture

```
User clicks action with inputs
        |
        v
ActionInputDialog opens
        |
        +--> Has scripted inputs? --> POST /api/projects/:id/actions/:name/resolve-inputs
        |                                    |
        |                            [Local mode: direct script execution]
        |                            [Server mode: ServerMessage::ResolveActionInputs -> Agent -> script -> ActionInputsResolved]
        |                                    |
        |                                    v
        |                            Resolved options populate selects
        |
        v
User fills form, clicks Run
        |
        v
POST /api/projects/:id/actions/:name/run  (body includes `inputs: Record<string,string>`)
        |
        v
expand_template() replaces {{custom_var}} placeholders alongside built-in ones
        |
        v
Terminal session created with expanded command
```

## Crate Dependency Graph

No new crates. Changes span existing crates:

```
zremote-protocol  (new types: ActionInput, ActionInputOption, ResolvedActionInput; new messages)
       |
       +---> zremote-core  (configure.rs prompt update)
       |
       +---> zremote-agent  (new: project/action_inputs.rs; updates: actions.rs, connection.rs, local/routes/projects.rs, local/mod.rs)
       |
       +---> zremote-server  (updates: state.rs, routes/projects.rs, routes/agents.rs, main.rs)

web/  (new: ActionInputDialog.tsx; updates: api.ts, ActionRow.tsx, CommandPalette.tsx, types.ts, project-actions.ts, worktree-actions.ts)
```

## Implementation Phases

### Phase 1: Protocol Types

**Files:** `crates/zremote-protocol/src/project.rs`

Add to `ProjectAction`:
```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub inputs: Vec<ActionInput>,
```

New types:
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionInputOption {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionInput {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "is_default_input_type")]
    pub input_type: PromptInputType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedActionInput {
    pub name: String,
    pub options: Vec<ActionInputOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
```

Update existing tests that construct `ProjectAction` to include `inputs: Vec::new()` / `inputs: vec![]`.

**Tests:** ActionInput roundtrip (full + minimal), ActionInputOption roundtrip, ActionInput with script, ProjectAction backward compat (JSON without `inputs`), ProjectAction with inputs roundtrip.

### Phase 2: Protocol Messages

**Files:** `crates/zremote-protocol/src/terminal.rs`

Add to `ServerMessage`:
```rust
ResolveActionInputs {
    request_id: uuid::Uuid,
    project_path: String,
    action_name: String,
},
```

Add to `AgentMessage`:
```rust
ActionInputsResolved {
    request_id: uuid::Uuid,
    inputs: Vec<zremote_protocol::project::ResolvedActionInput>,
    error: Option<String>,
},
```

**Tests:** roundtrip for both new variants.

### Phase 3: Script Execution Engine

**New file:** `crates/zremote-agent/src/project/action_inputs.rs`

```rust
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_SIZE: usize = 1_048_576; // 1 MB

pub fn parse_script_output(output: &str) -> Vec<ActionInputOption>
pub async fn resolve_script_options(script: &str, project_path: &Path, project_env: &HashMap<String, String>) -> Result<Vec<ActionInputOption>, String>
pub async fn resolve_action_inputs(action: &ProjectAction, project_path: &Path, project_env: &HashMap<String, String>) -> Vec<ResolvedActionInput>
```

Register `pub mod action_inputs;` in `crates/zremote-agent/src/project/mod.rs`.

**Tests:** parse_script_output (value only, value+label, comments, empty lines, mixed, empty output), resolve_script_options (success, timeout, non-zero exit), resolve_action_inputs (mixed script and static, no scripts, all scripts).

### Phase 4: Template Expansion

**Files:** `crates/zremote-agent/src/project/actions.rs`

Add `custom_inputs: HashMap<String, String>` to `TemplateContext`. Extend `expand_template` to iterate custom_inputs and replace `{{key}}` with value.

Update ALL `TemplateContext` construction sites to include `custom_inputs: HashMap::new()`.

**Tests:** expand_template_custom_inputs, expand_template_custom_inputs_mixed_with_builtins.

### Phase 5: Local Mode Endpoint + Run Update

**Files:** `crates/zremote-agent/src/local/routes/projects.rs`, `crates/zremote-agent/src/local/mod.rs`

New handler: `POST /api/projects/:project_id/actions/:action_name/resolve-inputs`

Update `RunActionRequest` - add `inputs: HashMap<String, String>` with `#[serde(default)]`.
Update `run_action` - pass `body.inputs` into `TemplateContext::custom_inputs`.

Register route in `build_router()`.

**Tests:** resolve_action_inputs_project_not_found, resolve_action_inputs_action_not_found, resolve_action_inputs_no_scripts, resolve_action_inputs_with_script, run_action_with_custom_inputs, run_action_request_deserialize_with_inputs.

### Phase 6: Server Mode Endpoint + Agent Handling

**Files:** `crates/zremote-server/src/state.rs`, `crates/zremote-server/src/routes/projects.rs`, `crates/zremote-server/src/routes/agents.rs`, `crates/zremote-agent/src/connection.rs`, `crates/zremote-server/src/main.rs`

Same request/response pattern as resolve_prompt: oneshot channel in state, send ServerMessage, wait with 15s timeout, return JSON.

Update server's `RunActionRequest` and action execution helpers to pass custom inputs.

**Tests:** resolve_action_inputs_project_not_found, resolve_action_inputs_host_offline, resolve_action_inputs_invalid_project_id, run_action_with_custom_inputs.

### Phase 7: Configure With Claude Prompt Update

**Files:** `crates/zremote-core/src/configure.rs`

Extend the `actions` schema section to include the new `inputs` field documentation. Add guidance on when to use inputs in Analysis Instructions.

Update test `test_prompt_contains_all_schema_fields`.

### Phase 8: Frontend Types & API

**Files:** `web/src/lib/api.ts`

New interfaces: `ActionInput`, `ActionInputOption`, `ResolvedActionInput`.
Update `ProjectAction` with optional `inputs`.
Update `RunActionRequest` with optional `inputs`.
Add `resolveActionInputs` to `api.projects`.

### Phase 9: ActionInputDialog Component

**New file:** `web/src/components/project/ActionInputDialog.tsx`

Full modal dialog following `RunPromptDialog.tsx` pattern. Key behaviors:
1. Detect worktree/branch needs from command template
2. Resolve scripted inputs on mount
3. Render form fields (text, multiline, select with static/scripted options)
4. Empty script output handling with retry
5. Per-input error display
6. Required field validation
7. Submit with collected values + worktree/branch
8. Keyboard shortcuts (Escape, Cmd+Enter)
9. Live command preview

**Tests:** ~9 tests covering all behaviors.

### Phase 10: ActionRow Integration

**Files:** `web/src/components/project/ActionRow.tsx`

Decision tree:
- Action has `inputs[]` with entries -> ActionInputDialog
- Action has NO inputs but needs worktree/branch -> ActionInputPopover (existing)
- Action has NO inputs and no missing vars -> direct run

**Tests:** ~3 tests covering decision tree.

### Phase 11: Command Palette Integration

**Files:** `web/src/components/command-palette/CommandPalette.tsx`, `types.ts`, `project-actions.ts`, `worktree-actions.ts`

Add `openActionInput` to `ActionDeps`. Actions with inputs open ActionInputDialog from palette.

**Tests:** ~2 tests.

### Phase 12: Tests & Verification

Full test suite run + manual verification with test action in `.zremote/settings.json`.

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Script execution security | Low | High | Same trust model as action commands (user-configured in settings.json). 10s timeout, 1MB output limit. |
| Backward compatibility | Low | Medium | `inputs` uses `#[serde(default, skip_serializing_if = "Vec::is_empty")]` - old JSON without inputs deserializes fine. |
| Script timeout blocking UI | Medium | Low | 10s timeout on backend, loading skeleton on frontend, per-input error display. |
| Protocol version mismatch | Low | Medium | New message types silently ignored by old agents/servers. |

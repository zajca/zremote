# RFC: Codex Integration Polish

## Status: Draft

## Date: 2026-05-14

## Problem Statement

ZRemote already has a working generic agent launcher architecture and a first
Codex integration:

- `agent_kind = "codex"` is in the protocol-level supported kinds.
- A default Codex profile is seeded by migration `027_codex_agent_profile.sql`.
- `CodexLauncher` builds a `codex` CLI command from an agent profile.
- The profile editor exposes Codex-specific fields.
- The command palette can launch every saved agent profile, including Codex.
- The agentic output analyzer has a Codex adapter.

The integration is usable, but still feels second-class compared with Claude
Code. The remaining gaps are mostly around GUI ergonomics, runtime readiness,
permission prompts, output parsing accuracy, and operational visibility.

The goal of this RFC is to plan the work needed to make Codex feel like a
first-class ZRemote agent without weakening the generic launcher model.

## Goals

1. Make Codex easy to launch from project rows via a small agent profile menu.
2. Improve Codex runtime detection so failed launches become visible errors,
   not silent terminal sessions.
3. Parse Codex output accurately enough for tokens, tools, touched files,
   model, approvals, and common errors.
4. Relay Codex permission/input prompts into ZRemote UI and notifications.
5. Improve Codex profile UX with safer controls and useful presets.
6. Add host capability detection for installed/authenticated Codex.
7. Add tests and real-output fixtures so parser and launch regressions are
   caught.
8. Document how to configure and use Codex in local and server mode.

## Non-Goals

- Replacing the generic `AgentLauncher` abstraction.
- Removing the legacy Claude task flow.
- Implementing a Codex-specific non-PTY transport.
- Auto-installing Codex on remote hosts.
- Automatically bypassing approvals. Profiles may expose approval/sandbox
  choices, but the user remains in control.
- Building project-specific profile inheritance in this pass.

## Current State

### Launcher

`crates/zremote-agent/src/agents/codex.rs` maps generic profile fields and
Codex-specific `settings_json` into a shell command:

- `model` -> `codex --model`
- `settings.config_profile` -> `codex --profile`
- `settings.sandbox` -> `codex --sandbox`
- `settings.approval_policy` -> `codex --ask-for-approval`
- `skip_permissions` -> `--dangerously-bypass-approvals-and-sandbox`
- `settings.search` -> `--search`
- `settings.no_alt_screen` -> `--no-alt-screen`
- `settings.config_overrides[]` -> repeated `-c key=value`
- `settings.custom_flags` -> appended free-form flags
- `extra_args[]` -> appended quoted args
- `initial_prompt` -> appended prompt, with a temp file path for long prompts

This is functional, but command construction still depends on shell string
composition. The launcher also has no post-spawn behavior.

### UI

The command palette already emits one `StartAgent` action per profile. This is
good and should remain the power-user path.

The project-row quick launch is still hard-coded to the default Claude profile.
That makes Codex discoverable in settings and command palette, but not in the
main project workflow.

### Monitoring

`crates/zremote-agent/src/agentic/adapters/codex.rs` detects a small set of
Codex lines:

- version banner
- token usage
- shell commands
- file operations
- generic input-needed lines
- a bare prompt

The token parser currently falls back to splitting total tokens when Codex
reports `input + output` in formats the regex does not capture. That is good
as a fallback, but not good enough for analytics.

### Lifecycle

Local mode returns success after the PTY is created and the launcher command is
written. Server mode similarly reports `Started` after command write. Neither
path proves that `codex` started successfully, found auth/config, or reached an
interactive prompt.

## Design

### 1. Project Row Agent Menu

#### Decision

Use a small menu on the project row agent button.

The row should keep a single compact agent action next to the existing new
session button. Clicking it opens a small menu listing launchable profiles,
grouped or labeled by agent kind. This avoids adding one icon per tool and
keeps project rows dense.

#### Behavior

- If there are no agent profiles, hide the agent button.
- If there is exactly one profile, clicking the button launches it directly.
- If there are multiple profiles, clicking opens the menu.
- The menu lists profiles as:
  - `Claude Code / Default`
  - `Codex / Default`
  - `Codex / Review`
- Default profiles appear first, then profiles sorted by kind display name and
  profile name.
- Disabled entries show a reason when host capability detection says the tool
  is unavailable.
- Selecting a profile calls the existing generic `launch_agent_for_project`
  path with `profile_id`, `host_id`, and `project_path`.

#### UI Notes

- Keep the button icon as `Zap` or change to `Bot` if `Zap` becomes overloaded.
- Tooltip when closed: `Start agent`.
- Tooltip when only one profile exists: `Start {kind} ({profile})`.
- The menu should be keyboard reachable through the command palette path even
  if the row menu itself is mouse-first.

#### Implementation Scope

- Replace `default_profile_for_kind("claude")` usage in sidebar row actions
  with a generic profile list.
- Add a lightweight project-row menu component or reuse an existing GPUI popover
  pattern if present.
- Keep `launch_agent_for_project` unchanged.
- Add tests for profile ordering and empty/single/multiple profile behavior
  where the current UI test surface allows it.

### 2. Codex Readiness and Launch Errors

#### Problem

Writing `codex ...\n` into a PTY does not mean Codex is running. Common failure
cases include:

- `codex: command not found`
- auth missing or expired
- invalid `~/.codex/config.toml`
- unsupported CLI flag after a Codex CLI update
- model/profile not found
- network unavailable

#### Design

Introduce a launch readiness detector for generic agent tasks.

For Codex, watch the early terminal output after spawn for:

- success signals:
  - Codex banner
  - interactive prompt
  - first assistant/status event
- failure signals:
  - command not found
  - unknown option
  - auth/login required
  - config parse error
  - profile not found
  - model not found

The detector should update session status and surface a UI toast or activity
event. Server mode should continue to send `Started` after PTY creation for
backwards compatibility, but then emit a follow-up status/error event when
readiness fails.

#### Implementation Scope

- Add provider-specific readiness patterns under the agentic/adapters layer or
  a small launcher readiness module.
- Store a short launch diagnostic in `sessions.error_message` when startup
  clearly fails.
- Add a timeout state: if no success or failure signal appears within a short
  window, keep the session active but mark readiness as unknown.

### 3. Codex Output Parser Accuracy

#### Token Usage

Improve `CODEX_TOKEN_RE` to parse the known Codex formats:

- `Token usage: 1.9K total (1K input + 900 output)`
- `Token usage: 1.9K total, input: 1K, output: 900`
- future variants with cached/read/write token fields when available

The parser should only split totals as a final fallback.

#### Tool Calls

Expand tool-call detection beyond:

- `Running ...`
- `Ran ...`

Expected categories:

- shell command
- file edit/add/delete
- file read/search if Codex prints it
- web/search when `--search` is enabled
- MCP/tool calls if exposed in Codex output

#### Files Touched

Keep `file_touched`, but add support for:

- paths with spaces
- multi-file edit summaries
- file paths in diff/apply-patch summaries

#### Model Detection

Runtime model should come from:

1. explicit profile model, when present
2. Codex output line, when present
3. config profile or default fallback, if discoverable

#### Implementation Scope

- Update `patterns.rs` and `adapters/codex.rs`.
- Add golden fixtures from real Codex terminal output.
- Add parser unit tests for every fixture.

### 4. Permission and Input Prompt Relay

#### Problem

Codex can ask for approval or user input in the terminal. ZRemote currently has
generic input-needed detection, but Codex approvals are not represented as
first-class actionable events.

#### Design

Codex adapter should classify approval prompts into structured events:

- command approval
- file edit approval
- network/search approval
- escalation/sandbox approval
- generic text input needed

The UI should display these as actionable pending items where possible:

- approve
- reject
- open terminal

The first implementation should use PTY input injection only for deterministic
command approval prompts. File edit, network, sandbox escalation, and generic
text prompts should fall back to opening the terminal until they can be handled
reliably. If Codex exposes a structured approval API later, this layer can
switch transports without changing the UI event model.

#### Implementation Scope

- Extend prompt detection patterns for Codex approval text.
- Map prompt events into the existing activity/notification system.
- Add Telegram notification support for pending Codex approval if the existing
  notification primitives already support generic agent events.
- Keep auto-approval policy separate from Codex CLI approval policy. The
  profile decides what Codex is allowed to ask; ZRemote decides whether to
  notify, auto-approve, or require manual action.

### 5. Codex Profile UX

#### Current Pain

Codex settings are mostly text fields. That is flexible, but it puts too much
burden on users to remember exact values.

#### Design

Replace free-text fields with safer controls where the option set is known:

- `sandbox`: dropdown
  - empty/default
  - `read-only`
  - `workspace-write`
  - `danger-full-access`
- `approval_policy`: dropdown
  - empty/default
  - `untrusted`
  - `on-failure`
  - `on-request`
  - `never`
- `search`: checkbox
- `no_alt_screen`: checkbox
- `config_profile`: text input with validation
- `config_overrides`: tag list
- `custom_flags`: advanced text input

Add profile templates:

- `Codex / Review`
  - sandbox: `read-only`
  - approval: `on-request`
  - prompt: review-oriented
- `Codex / Implement`
  - sandbox: `workspace-write`
  - approval: `on-request`
- `Codex / Autonomous`
  - sandbox: `workspace-write`
  - approval: `on-failure`
- `Codex / Full Trust`
  - skip permissions enabled
  - visually marked as high trust

These presets should be created automatically through migration/seeding so a
fresh install has useful Codex profiles immediately. High-trust profiles need
clear UI labeling, but they do not require an extra first-launch confirmation.

#### Config Import

Add an import action that reads `~/.codex/config.toml` and suggests matching
ZRemote profiles. In server mode this must run on the target host, not the GUI
machine.

### 6. Host Capability Detection

#### Problem

The protocol advertises Codex support statically, but a specific host may not
have Codex installed or authenticated.

#### Design

Add host-level agent capabilities:

```json
{
  "host_id": "...",
  "agents": {
    "codex": {
      "installed": true,
      "version": "0.98.0",
      "authenticated": true,
      "config_profiles": ["default", "work"],
      "last_checked_at": "..."
    },
    "claude": {
      "installed": true,
      "version": "...",
      "authenticated": null
    }
  }
}
```

Capabilities should be best-effort. Failure to check capabilities should not
break manual launch.

Capability detection may read `~/.codex/config.toml` on the target host to
discover configured Codex profiles. In server mode, this must run on the remote
agent host, not the GUI machine.

#### Implementation Scope

- Local and remote agents run lightweight commands:
  - `codex --version`
  - config inspection from `~/.codex/config.toml`
  - optional auth/status probe if Codex has a safe command for it
- Server stores or caches the latest capability snapshot.
- GUI uses capabilities to disable menu entries or show warnings.

### 7. Safer Launcher Internals

#### Current Risk

The launcher builds a shell command string. Validation blocks known dangerous
characters for custom flags, but structured arguments are still safer and
easier to reason about.

#### Design

Keep the PTY write model, but reduce shell-string risk:

- Build an internal structured argv first.
- Quote every argv segment with one shared shell quoting helper.
- Split `custom_flags` into argv using a shell-words parser instead of
  appending the raw string.
- Create long prompt temp files with restrictive permissions.
- Delete prompt temp files after command construction when possible, or use a
  lifecycle cleanup path.
- Avoid `$(cat file)` if a Codex-native prompt-file flag exists in the CLI
  version ZRemote supports.

#### Implementation Scope

- Introduce a small `CommandLine` helper shared by Claude/Codex only if it
  reduces duplication.
- Add tests for prompt values containing quotes, newlines, and shell-looking
  strings.
- Keep compatibility with existing profiles.

### 8. Analytics

#### Design

Codex sessions should feed the same agentic metrics surface as Claude where
possible:

- cumulative input/output tokens
- estimated cost when model is known
- file touches
- tool calls
- approval wait time
- session readiness/failure reason

Cost estimation should be explicitly marked estimated unless Codex reports an
authoritative cost.

#### Implementation Scope

- Improve model propagation from profile to metrics.
- Add known OpenAI model pricing entries only when stable enough for this app.
- Prefer config-driven pricing or versioned constants over hard-coded guesses
  when possible.

### 9. Documentation

Add user-facing docs:

- how to install/login to Codex on each host
- how to create a Codex profile
- recommended sandbox/approval combinations
- local mode vs server mode behavior
- how project-row menu and command palette launch differ
- troubleshooting:
  - command not found
  - auth required
  - invalid profile
  - missing model
  - approvals not appearing

Update README feature bullets so AI agent support does not read as
Claude-only.

## Implementation Plan

### Phase 1: Launch UX

Deliver the project-row small menu.

Tasks:

1. Replace hard-coded default Claude quick launch with a generic agent menu.
2. Sort profiles with defaults first.
3. Launch selected profile through existing `launch_agent_for_project`.
4. Keep command palette behavior unchanged.
5. Add focused tests for profile ordering and menu action construction where
   feasible.

Acceptance:

- A project row can launch Codex without opening command palette.
- Multiple profiles are selectable from one compact menu.
- Empty profile state does not show a broken button.

### Phase 2: Parser Fixtures

Deliver accurate Codex parsing for current real output.

Tasks:

1. Capture representative Codex terminal logs into `test-data`.
2. Fix token parsing for `total (input + output)` formats.
3. Add model, common error, and approval prompt detection.
4. Add tests around the fixtures.

Acceptance:

- Token breakdown no longer falls back to 50/50 split for known Codex formats.
- Common launch/auth/config errors are classified.
- Approval prompts create input-needed or approval events.

### Phase 3: Readiness Lifecycle

Deliver visible launch success/failure state.

Tasks:

1. Add a startup readiness detector for generic agent sessions.
2. Wire Codex success/failure patterns into it.
3. Persist clear failure messages in session state.
4. Surface local/server failures in UI toasts or activity panel.

Acceptance:

- `codex: command not found` results in an errored session or visible launch
  diagnostic.
- Successful Codex startup becomes distinguishable from "PTY command written".
- Unknown startup remains non-fatal.

### Phase 4: Permission Relay

Deliver actionable Codex approval prompts.

Tasks:

1. Classify Codex approval prompts.
2. Reuse existing pending-action/notification surfaces where possible.
3. Implement approve/reject PTY responses for deterministic command approval
   prompts only.
4. Add Telegram notifications if the generic event path supports it cleanly.

Acceptance:

- User sees a pending Codex approval outside the raw terminal.
- Approve/reject works for at least the common command approval prompt.
- Non-command and non-deterministic prompts fall back to "open terminal".

### Phase 5: Profile UX and Presets

Deliver safer Codex profile editing.

Tasks:

1. Replace known-value text fields with dropdowns.
2. Add Codex profile templates.
3. Add clear high-trust warning for skip permissions/full trust profile.
4. Add host-side Codex config profile import.

Acceptance:

- A user can create a useful Codex profile without remembering flag values.
- Invalid sandbox/approval values cannot be entered through normal controls.
- Existing profiles continue to load and save.
- High-trust profiles are clearly marked in UI; no extra first-launch
  confirmation is required.

### Phase 6: Capabilities and Docs

Deliver host capability visibility and onboarding docs.

Tasks:

1. Add best-effort host capability checks for Codex.
2. Show disabled/warning state in the project-row menu.
3. Document install/login/profile setup.
4. Update README feature language.

Acceptance:

- A host without Codex installed is visibly different from a host where Codex
  is ready.
- Users have a clear troubleshooting path.

## Test Plan

- Unit tests:
  - Codex command builder flags and quoting.
  - Codex settings validation.
  - Codex parser token formats.
  - Codex parser approval/error detection.
  - Profile menu ordering helper.
- Integration tests:
  - `POST /api/agent-tasks` with Codex profile.
  - server-mode `StartAgent` dispatch for Codex.
  - failed launch path marks session diagnostic.
- Fixture tests:
  - real Codex startup output.
  - real Codex token summary output.
  - real approval prompt output.
  - common failure output.
- Manual QA:
  - local mode with Codex installed.
  - local mode without Codex installed.
  - server mode with Codex installed only on remote host.
  - multiple profiles in project-row menu.
  - command palette launch remains unchanged.

## Risks and Mitigations

### Codex CLI Output Changes

Risk: Regex parsing can break when Codex changes TUI text.

Mitigation: Keep parser permissive, add fixtures, and treat unknown output as
non-fatal. Prefer structured Codex output if the CLI exposes one later.

### Approval Prompt Automation Is Fragile

Risk: PTY injection for approve/reject may be prompt-format dependent.

Mitigation: Start with detection and "open terminal"; add approve/reject only
for deterministic command approval prompts. Keep the event model
transport-agnostic.

### Menu Could Become Too Busy

Risk: Many profiles make project-row menu long.

Mitigation: Defaults first, grouped by kind, and command palette remains the
fast search path. Add filtering later only if real usage needs it.

### Capability Checks May Be Slow

Risk: Running CLI probes on many hosts can delay UI refresh.

Mitigation: Cache results, run checks asynchronously, and never block manual
launch on capability lookup.

### Shell Command Construction

Risk: Free-form flags and shell quoting are easy to get wrong.

Mitigation: Move toward structured argv, shared quoting tests, and shell-words
parsing for advanced flags.

## Resolved Decisions

1. When only one agent profile is available for a project row, the agent button
   launches it directly instead of opening the menu.
2. Capability detection may read `~/.codex/config.toml` on the target host.
3. Phase 4 approve/reject buttons cover only common command approval prompts.
4. Codex profile presets should be created automatically by migration/seeding.
5. High-trust profiles only need clear UI labeling; no extra first-launch
   confirmation is required.

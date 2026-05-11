#![allow(clippy::wildcard_imports)]

//! Agent Profiles tab for the Settings modal.
//!
//! Renders the split-pane CRUD editor described in RFC-003 Phase 6:
//!   - Left pane: list of saved profiles grouped by `agent_kind`.
//!   - Right pane: form editor for the selected profile (or "new" draft).
//!
//! Keyboard routing
//! ----------------
//! This tab doesn't use a GPUI native text input. Each editable field is a
//! plain `div` that becomes the "active input" on click; the tab's
//! `on_key_down` handler routes printable chars / backspace to the matching
//! `EditForm` string based on [`ActiveInput`]. This mirrors the pattern
//! already used in `command_palette::keybindings` and avoids pulling in a
//! heavier input widget just for settings.
//!
//! Validation
//! ----------
//! Client-side validation mirrors the rules in
//! `zremote_core::validation::agent_profile` (hand-inlined here -- the GUI
//! crate intentionally does not depend on `zremote-core`). The server is
//! the final authority; we validate locally only to give fast feedback on
//! obvious mistakes (name empty, shell metachars in tool/arg entries).

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use serde_json::json;

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::theme;
use zremote_client::{
    AgentKindInfo, AgentProfile, CreateAgentProfileRequest, UpdateAgentProfileRequest,
};

// ---------------------------------------------------------------------------
// Events emitted upward to `SettingsModal`.
// ---------------------------------------------------------------------------

/// Events raised by the profiles tab.
pub enum AgentProfilesTabEvent {
    /// A profile was created, updated, deleted, or set-default succeeded.
    /// The parent modal re-emits this so `MainView` can trigger
    /// `SidebarView::refresh_agent_profiles`.
    ProfilesChanged,
}

// ---------------------------------------------------------------------------
// Editor state.
// ---------------------------------------------------------------------------

/// Which editor "mode" the form represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorMode {
    /// Editing an existing profile (`agent_kind` is read-only).
    Edit,
    /// Drafting a new profile (`agent_kind` is the first kind in the list).
    Create,
}

/// Which tag list a remove-chip click targets. Using an enum keeps the chip
/// click handler `Copy` (no captured closure state), which sidesteps the
/// `cx.listener` output not being `Clone` when we need to iterate chips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagListKind {
    AllowedTools,
    ExtraArgs,
    DevelopmentChannels,
    CodexConfigOverrides,
}

/// Which input field is currently receiving key events.
///
/// Only printable characters and backspace are routed -- navigation keys are
/// ignored when an input is active, so the user can still hit Escape to
/// dismiss the modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ActiveInput {
    None,
    Name,
    Description,
    Model,
    InitialPrompt,
    NewTool,
    NewArg,
    NewChannel,
    NewEnvKey,
    NewEnvValue,
    ClaudeOutputFormat,
    ClaudeCustomFlags,
    CodexConfigProfile,
    CodexSandbox,
    CodexApprovalPolicy,
    NewCodexConfigOverride,
    CodexCustomFlags,
}

/// Mutable form state for the editor pane.
///
/// Held separately from the `AgentProfile` snapshot so edits don't clobber
/// the server's view until the user clicks Save.
#[derive(Debug, Clone, Default)]
struct EditForm {
    name: String,
    description: String,
    agent_kind: String,
    model: String,
    initial_prompt: String,
    skip_permissions: bool,
    allowed_tools: Vec<String>,
    extra_args: Vec<String>,
    /// Key-value pairs preserving insertion order so repeated edits don't
    /// reshuffle the UI.
    env_vars: Vec<(String, String)>,
    // Claude-specific (deserialized from profile.settings JSON on load).
    claude_development_channels: Vec<String>,
    claude_output_format: String,
    claude_print_mode: bool,
    /// Free-form flag blob. Matches both the agent-side runtime shape
    /// (`zremote_agent::claude::CommandOptions::custom_flags: Option<&str>`)
    /// and the core validator shape
    /// (`ClaudeSettingsShape::custom_flags: Option<String>`). Serialized
    /// to JSON as a single string, or omitted when empty.
    claude_custom_flags: String,
    // Codex-specific (deserialized from profile.settings JSON on load).
    codex_config_profile: String,
    codex_sandbox: String,
    codex_approval_policy: String,
    codex_config_overrides: Vec<String>,
    codex_search: bool,
    codex_no_alt_screen: bool,
    codex_custom_flags: String,

    // Transient input buffers for the tag-list editors. These are not part
    // of the saved profile; they just hold the string the user is typing
    // before they hit Enter to add it as a chip.
    new_tool_input: String,
    new_arg_input: String,
    new_channel_input: String,
    new_codex_config_override_input: String,
    new_env_key: String,
    new_env_value: String,
}

impl EditForm {
    /// Populate an `EditForm` from an existing profile for the edit pane.
    fn from_profile(profile: &AgentProfile) -> Self {
        // Extract claude-specific fields from the settings JSON if present.
        // Unknown shapes fall back to defaults -- the server guarantees the
        // payload validates against `ClaudeSettings` before it lands, so
        // this only matters for future kinds that add new fields.
        let settings = &profile.settings;
        let claude_development_channels = settings
            .get("development_channels")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let claude_output_format = settings
            .get("output_format")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let claude_print_mode = settings
            .get("print_mode")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let claude_custom_flags = settings
            .get("custom_flags")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let codex_config_profile = settings
            .get("config_profile")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let codex_sandbox = settings
            .get("sandbox")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let codex_approval_policy = settings
            .get("approval_policy")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let codex_config_overrides = settings
            .get("config_overrides")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let codex_search = settings
            .get("search")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let codex_no_alt_screen = settings
            .get("no_alt_screen")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let codex_custom_flags = settings
            .get("custom_flags")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Self {
            name: profile.name.clone(),
            description: profile.description.clone().unwrap_or_default(),
            agent_kind: profile.agent_kind.clone(),
            model: profile.model.clone().unwrap_or_default(),
            initial_prompt: profile.initial_prompt.clone().unwrap_or_default(),
            skip_permissions: profile.skip_permissions,
            allowed_tools: profile.allowed_tools.clone(),
            extra_args: profile.extra_args.clone(),
            env_vars: profile
                .env_vars
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            claude_development_channels,
            claude_output_format,
            claude_print_mode,
            claude_custom_flags,
            codex_config_profile,
            codex_sandbox,
            codex_approval_policy,
            codex_config_overrides,
            codex_search,
            codex_no_alt_screen,
            codex_custom_flags,
            new_tool_input: String::new(),
            new_arg_input: String::new(),
            new_channel_input: String::new(),
            new_codex_config_override_input: String::new(),
            new_env_key: String::new(),
            new_env_value: String::new(),
        }
    }

    /// Blank form used when switching into Create mode. The caller is
    /// responsible for setting `agent_kind` to a valid value before the
    /// user can save.
    fn blank(default_kind: String) -> Self {
        Self {
            agent_kind: default_kind,
            ..Self::default()
        }
    }

    /// Serialize the settings JSON blob for the current `agent_kind`.
    /// Returns `Value::Null` for unknown kinds -- server rejects them anyway.
    fn settings_json(&self) -> serde_json::Value {
        if self.agent_kind == "claude" {
            let mut obj = serde_json::Map::new();
            if !self.claude_development_channels.is_empty() {
                obj.insert(
                    "development_channels".to_string(),
                    json!(self.claude_development_channels),
                );
            }
            if !self.claude_output_format.is_empty() {
                obj.insert(
                    "output_format".to_string(),
                    json!(self.claude_output_format),
                );
            }
            if self.claude_print_mode {
                obj.insert("print_mode".to_string(), json!(true));
            }
            if !self.claude_custom_flags.is_empty() {
                obj.insert("custom_flags".to_string(), json!(self.claude_custom_flags));
            }
            serde_json::Value::Object(obj)
        } else if self.agent_kind == "codex" {
            let mut obj = serde_json::Map::new();
            if !self.codex_config_profile.is_empty() {
                obj.insert(
                    "config_profile".to_string(),
                    json!(self.codex_config_profile),
                );
            }
            if !self.codex_sandbox.is_empty() {
                obj.insert("sandbox".to_string(), json!(self.codex_sandbox));
            }
            if !self.codex_approval_policy.is_empty() {
                obj.insert(
                    "approval_policy".to_string(),
                    json!(self.codex_approval_policy),
                );
            }
            if !self.codex_config_overrides.is_empty() {
                obj.insert(
                    "config_overrides".to_string(),
                    json!(self.codex_config_overrides),
                );
            }
            if self.codex_search {
                obj.insert("search".to_string(), json!(true));
            }
            if self.codex_no_alt_screen {
                obj.insert("no_alt_screen".to_string(), json!(true));
            }
            if !self.codex_custom_flags.is_empty() {
                obj.insert("custom_flags".to_string(), json!(self.codex_custom_flags));
            }
            serde_json::Value::Object(obj)
        } else {
            serde_json::Value::Null
        }
    }

    fn env_vars_map(&self) -> BTreeMap<String, String> {
        self.env_vars.iter().cloned().collect()
    }

    /// Build a create request. Validates before returning.
    fn to_create_request(&self) -> Result<CreateAgentProfileRequest, String> {
        self.validate()?;
        Ok(CreateAgentProfileRequest {
            name: self.name.clone(),
            description: non_empty(&self.description),
            agent_kind: self.agent_kind.clone(),
            is_default: false,
            sort_order: 0,
            model: non_empty(&self.model),
            initial_prompt: non_empty(&self.initial_prompt),
            skip_permissions: self.skip_permissions,
            allowed_tools: self.allowed_tools.clone(),
            extra_args: self.extra_args.clone(),
            env_vars: self.env_vars_map(),
            settings: self.settings_json(),
        })
    }

    /// Build an update request. Validates before returning.
    fn to_update_request(&self) -> Result<UpdateAgentProfileRequest, String> {
        self.validate()?;
        Ok(UpdateAgentProfileRequest {
            name: self.name.clone(),
            description: non_empty(&self.description),
            sort_order: 0,
            model: non_empty(&self.model),
            initial_prompt: non_empty(&self.initial_prompt),
            skip_permissions: self.skip_permissions,
            allowed_tools: self.allowed_tools.clone(),
            extra_args: self.extra_args.clone(),
            env_vars: self.env_vars_map(),
            settings: self.settings_json(),
        })
    }

    /// Fast, client-side validation mirroring the rules in
    /// `zremote_core::validation::agent_profile`. Server still runs the full
    /// checks and length limits -- this is just immediate feedback.
    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Name is required".to_string());
        }

        if !self.model.is_empty() {
            validate_model(&self.model)?;
        }

        for tool in &self.allowed_tools {
            validate_allowed_tool(tool)?;
        }

        for arg in &self.extra_args {
            validate_extra_arg(arg)?;
        }

        for (k, v) in &self.env_vars {
            validate_env_var_key(k)?;
            validate_env_var_value(v)?;
        }

        // Claude-specific
        if self.agent_kind == "claude" {
            for ch in &self.claude_development_channels {
                validate_development_channel(ch)?;
            }
            if !self.claude_output_format.is_empty() {
                validate_output_format(&self.claude_output_format)?;
            }
            if !self.claude_custom_flags.is_empty() {
                validate_custom_flags(&self.claude_custom_flags)?;
            }
        } else if self.agent_kind == "codex" {
            if !self.codex_config_profile.is_empty() {
                validate_codex_config_profile(&self.codex_config_profile)?;
            }
            if !self.codex_sandbox.is_empty() {
                validate_codex_sandbox(&self.codex_sandbox)?;
            }
            if !self.codex_approval_policy.is_empty() {
                validate_codex_approval_policy(&self.codex_approval_policy)?;
            }
            for override_arg in &self.codex_config_overrides {
                validate_codex_config_override(override_arg)?;
            }
            if !self.codex_custom_flags.is_empty() {
                validate_custom_flags(&self.codex_custom_flags)?;
            }
        }

        Ok(())
    }
}

fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

// ---------------------------------------------------------------------------
// Inlined validators mirroring zremote_core::validation::agent_profile.
//
// These are intentionally hand-written so the GUI crate doesn't need to
// depend on zremote-core. They are kept close to the forms that use them so
// divergence from the core rules is obvious during code review.
// ---------------------------------------------------------------------------

const SHELL_METACHARS: &[char] = &[';', '|', '&', '>', '<', '$', '`', '\\', '\n', '\r', '\0'];

fn contains_shell_metachars(s: &str) -> bool {
    s.chars().any(|c| SHELL_METACHARS.contains(&c))
}

fn validate_model(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("model must not be empty".to_string());
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(format!("invalid model name: {s}"));
    }
    Ok(())
}

fn validate_allowed_tool(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("allowed tool must not be empty".to_string());
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '*')
    {
        return Err(format!("invalid tool name: {s}"));
    }
    Ok(())
}

fn validate_extra_arg(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("extra arg must not be empty".to_string());
    }
    if !s.starts_with('-') {
        return Err(format!("extra arg must start with '-': {s}"));
    }
    if contains_shell_metachars(s) {
        return Err(format!("extra arg contains shell metacharacters: {s}"));
    }
    Ok(())
}

fn validate_env_var_key(s: &str) -> Result<(), String> {
    let mut chars = s.chars();
    let first = chars
        .next()
        .ok_or_else(|| "env var key must not be empty".to_string())?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(format!("env var key must start with a letter or '_': {s}"));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("invalid env var key: {s}"));
    }
    Ok(())
}

fn validate_env_var_value(s: &str) -> Result<(), String> {
    if s.chars().any(|c| c == '\n' || c == '\r' || c == '\0') {
        return Err("env var value contains control characters".to_string());
    }
    Ok(())
}

fn validate_development_channel(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("development channel must not be empty".to_string());
    }
    if !s.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || c == '_'
            || c == '-'
            || c == ':'
            || c == '.'
            || c == '@'
            || c == '/'
    }) {
        return Err(format!("invalid development channel: {s}"));
    }
    Ok(())
}

fn validate_output_format(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("output format must not be empty".to_string());
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid output format: {s}"));
    }
    Ok(())
}

fn validate_custom_flags(s: &str) -> Result<(), String> {
    if contains_shell_metachars(s) {
        return Err(format!("custom flags contain shell metacharacters: {s}"));
    }
    Ok(())
}

fn validate_codex_config_profile(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("codex config profile must not be empty".to_string());
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return Err(format!("invalid codex config profile: {s}"));
    }
    Ok(())
}

fn validate_codex_sandbox(s: &str) -> Result<(), String> {
    match s {
        "read-only" | "workspace-write" | "danger-full-access" => Ok(()),
        _ => Err(format!("invalid codex sandbox: {s}")),
    }
}

fn validate_codex_approval_policy(s: &str) -> Result<(), String> {
    match s {
        "untrusted" | "on-failure" | "on-request" | "never" => Ok(()),
        _ => Err(format!("invalid codex approval policy: {s}")),
    }
}

fn validate_codex_config_override(s: &str) -> Result<(), String> {
    let Some((key, value)) = s.split_once('=') else {
        return Err(format!("codex config override must be key=value: {s}"));
    };
    if key.is_empty() {
        return Err("codex config override key must not be empty".to_string());
    }
    for part in key.split('.') {
        if part.is_empty()
            || !part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!("invalid codex config override key: {key}"));
        }
    }
    if value.chars().any(|c| c == '\n' || c == '\r' || c == '\0') {
        return Err("codex config override value contains control characters".to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AgentProfilesTab view.
// ---------------------------------------------------------------------------

pub struct AgentProfilesTab {
    focus_handle: FocusHandle,
    app_state: Arc<AppState>,
    profiles: Rc<Vec<AgentProfile>>,
    kinds: Rc<Vec<AgentKindInfo>>,
    selected_profile_id: Option<String>,
    edit_form: EditForm,
    editor_mode: EditorMode,
    save_error: Option<String>,
    saving: bool,
    active_input: ActiveInput,
    /// Set to `true` whenever `edit_form` is mutated by the user, cleared
    /// on a successful save or when a fresh form is loaded. Used by
    /// `set_profiles` to avoid silently clobbering unsaved edits when the
    /// sidebar's shared `Rc<Vec<AgentProfile>>` is rebuilt (e.g. after a
    /// concurrent CRUD refresh on a different profile).
    dirty: bool,
    /// Per-field inline validation errors. Populated live as the user types
    /// so each field renders a red border + inline error text below the
    /// hint line without waiting for a Save click. Keyed by the
    /// `ActiveInput` whose buffer is currently invalid; chip-list errors
    /// live under the `NewFoo` variant of the pending-entry buffer.
    field_errors: HashMap<ActiveInput, String>,
}

impl EventEmitter<AgentProfilesTabEvent> for AgentProfilesTab {}

impl Focusable for AgentProfilesTab {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl AgentProfilesTab {
    pub fn new(
        app_state: Arc<AppState>,
        profiles: Rc<Vec<AgentProfile>>,
        kinds: Rc<Vec<AgentKindInfo>>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Auto-select the first profile if any exist.
        let initial_selection = profiles.first().map(|p| p.id.clone());
        let edit_form = initial_selection
            .as_deref()
            .and_then(|id| profiles.iter().find(|p| p.id == id))
            .map(EditForm::from_profile)
            .unwrap_or_default();

        Self {
            focus_handle: cx.focus_handle(),
            app_state,
            profiles,
            kinds,
            selected_profile_id: initial_selection,
            edit_form,
            editor_mode: EditorMode::Edit,
            save_error: None,
            saving: false,
            active_input: ActiveInput::None,
            dirty: false,
            field_errors: HashMap::new(),
        }
    }

    /// Refresh the cached profile/kind lists from the parent (modal pushes
    /// this on every render so the tab stays in sync after CRUD calls).
    ///
    /// When the user has unsaved edits (`dirty == true`), the form is
    /// preserved verbatim -- only the cached lists are replaced so the
    /// left-pane list view stays in sync. The user's typed changes are
    /// never silently discarded by a background refresh.
    pub fn set_profiles(
        &mut self,
        profiles: Rc<Vec<AgentProfile>>,
        kinds: Rc<Vec<AgentKindInfo>>,
        cx: &mut Context<Self>,
    ) {
        // Short-circuit when nothing changed (Rc::ptr_eq) to avoid triggering
        // redundant re-renders on each MainView render.
        if Rc::ptr_eq(&self.profiles, &profiles) && Rc::ptr_eq(&self.kinds, &kinds) {
            return;
        }
        self.profiles = profiles;
        self.kinds = kinds;

        // Only refresh the form if the user has no unsaved edits. This
        // prevents data loss when a background refresh fires while the user
        // is typing (e.g. another CRUD action elsewhere triggers a sidebar
        // reload of the shared Rc).
        if !self.dirty
            && self.editor_mode == EditorMode::Edit
            && let Some(id) = self.selected_profile_id.as_deref()
        {
            if let Some(profile) = self.profiles.iter().find(|p| p.id == id) {
                self.edit_form = EditForm::from_profile(profile);
            } else {
                // Selected profile was deleted -- fall back to first, or empty.
                self.selected_profile_id = self.profiles.first().map(|p| p.id.clone());
                self.edit_form = self
                    .selected_profile_id
                    .as_deref()
                    .and_then(|id| self.profiles.iter().find(|p| p.id == id))
                    .map(EditForm::from_profile)
                    .unwrap_or_default();
            }
            self.field_errors.clear();
        }

        cx.notify();
    }

    /// Mark the edit form as dirty. Called from every input-mutation path
    /// (click handlers, key handler) so `set_profiles` knows not to clobber
    /// user edits during a background refresh.
    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Re-run the per-field validator for `field` against the current form
    /// buffer and update `field_errors` accordingly. Invoked from every
    /// user-mutation path (keystroke, chip add/remove) so the inline red
    /// border + error text updates in real time without waiting for Save.
    ///
    /// `ActiveInput::None` is a no-op -- we only revalidate fields the user
    /// is actively touching, which keeps this O(1) per keystroke instead
    /// of rescanning the whole form on each character.
    fn revalidate_field(&mut self, field: ActiveInput) {
        let result: Result<(), String> = match field {
            ActiveInput::None => return,
            ActiveInput::Name => {
                if self.edit_form.name.trim().is_empty() {
                    Err("Name is required".to_string())
                } else {
                    Ok(())
                }
            }
            ActiveInput::Description => Ok(()),
            ActiveInput::Model => {
                if self.edit_form.model.is_empty() {
                    Ok(())
                } else {
                    validate_model(&self.edit_form.model)
                }
            }
            ActiveInput::InitialPrompt => Ok(()),
            ActiveInput::NewTool => {
                if self.edit_form.new_tool_input.is_empty() {
                    Ok(())
                } else {
                    validate_allowed_tool(&self.edit_form.new_tool_input)
                }
            }
            ActiveInput::NewArg => {
                if self.edit_form.new_arg_input.is_empty() {
                    Ok(())
                } else {
                    validate_extra_arg(&self.edit_form.new_arg_input)
                }
            }
            ActiveInput::NewChannel => {
                if self.edit_form.new_channel_input.is_empty() {
                    Ok(())
                } else {
                    validate_development_channel(&self.edit_form.new_channel_input)
                }
            }
            ActiveInput::NewEnvKey => {
                if self.edit_form.new_env_key.is_empty() {
                    Ok(())
                } else {
                    validate_env_var_key(&self.edit_form.new_env_key)
                }
            }
            ActiveInput::NewEnvValue => validate_env_var_value(&self.edit_form.new_env_value),
            ActiveInput::ClaudeOutputFormat => {
                if self.edit_form.claude_output_format.is_empty() {
                    Ok(())
                } else {
                    validate_output_format(&self.edit_form.claude_output_format)
                }
            }
            ActiveInput::ClaudeCustomFlags => {
                if self.edit_form.claude_custom_flags.is_empty() {
                    Ok(())
                } else {
                    validate_custom_flags(&self.edit_form.claude_custom_flags)
                }
            }
            ActiveInput::CodexConfigProfile => {
                if self.edit_form.codex_config_profile.is_empty() {
                    Ok(())
                } else {
                    validate_codex_config_profile(&self.edit_form.codex_config_profile)
                }
            }
            ActiveInput::CodexSandbox => {
                if self.edit_form.codex_sandbox.is_empty() {
                    Ok(())
                } else {
                    validate_codex_sandbox(&self.edit_form.codex_sandbox)
                }
            }
            ActiveInput::CodexApprovalPolicy => {
                if self.edit_form.codex_approval_policy.is_empty() {
                    Ok(())
                } else {
                    validate_codex_approval_policy(&self.edit_form.codex_approval_policy)
                }
            }
            ActiveInput::NewCodexConfigOverride => {
                if self.edit_form.new_codex_config_override_input.is_empty() {
                    Ok(())
                } else {
                    validate_codex_config_override(&self.edit_form.new_codex_config_override_input)
                }
            }
            ActiveInput::CodexCustomFlags => {
                if self.edit_form.codex_custom_flags.is_empty() {
                    Ok(())
                } else {
                    validate_custom_flags(&self.edit_form.codex_custom_flags)
                }
            }
        };

        match result {
            Ok(()) => {
                self.field_errors.remove(&field);
            }
            Err(msg) => {
                self.field_errors.insert(field, msg);
            }
        }
    }

    /// Revalidate every "pending entry" chip/env buffer. Called after a
    /// chip is committed or removed so the inline error clears when the
    /// pending-entry buffer (the one just `std::mem::take()`-d) is empty.
    fn revalidate_list_fields(&mut self) {
        for f in [
            ActiveInput::NewTool,
            ActiveInput::NewArg,
            ActiveInput::NewChannel,
            ActiveInput::NewCodexConfigOverride,
            ActiveInput::NewEnvKey,
            ActiveInput::NewEnvValue,
        ] {
            self.revalidate_field(f);
        }
    }

    fn select_profile(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        if self.selected_profile_id.as_deref() == Some(profile_id)
            && self.editor_mode == EditorMode::Edit
        {
            return;
        }
        // Block profile switches while there are unsaved edits so typed
        // changes cannot be silently discarded. The error message prompts
        // the user to Save or Delete the draft first.
        if self.dirty {
            self.save_error =
                Some("Unsaved changes — Save or Delete before switching profiles".to_string());
            cx.notify();
            return;
        }
        if let Some(profile) = self.profiles.iter().find(|p| p.id == profile_id) {
            self.selected_profile_id = Some(profile_id.to_string());
            self.edit_form = EditForm::from_profile(profile);
            self.editor_mode = EditorMode::Edit;
            self.save_error = None;
            self.active_input = ActiveInput::None;
            self.dirty = false;
            self.field_errors.clear();
            cx.notify();
        }
    }

    fn start_create(&mut self, cx: &mut Context<Self>) {
        if self.dirty {
            self.save_error =
                Some("Unsaved changes — Save or Delete before creating a new profile".to_string());
            cx.notify();
            return;
        }
        let default_kind = self
            .kinds
            .first()
            .map_or_else(|| "claude".to_string(), |k| k.kind.clone());
        self.edit_form = EditForm::blank(default_kind);
        self.editor_mode = EditorMode::Create;
        self.selected_profile_id = None;
        self.save_error = None;
        self.active_input = ActiveInput::Name;
        // The blank create form starts "dirty" (the user is about to type),
        // but flag it only after they first mutate so the save_error guard
        // above works correctly on repeated clicks of "New".
        self.dirty = false;
        self.field_errors.clear();
        cx.notify();
    }

    // ---- CRUD ------------------------------------------------------------

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        match self.editor_mode {
            EditorMode::Create => {
                let req = match self.edit_form.to_create_request() {
                    Ok(r) => r,
                    Err(e) => {
                        self.save_error = Some(e);
                        cx.notify();
                        return;
                    }
                };
                self.save_error = None;
                self.saving = true;
                cx.notify();

                let api = self.app_state.api.clone();
                let handle = self.app_state.tokio_handle.clone();
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    // Match sidebar's convention: a JoinError (the task
                    // itself panicked or was cancelled) degrades to an
                    // error string rather than propagating the panic into
                    // the GPUI async executor.
                    let result = handle
                        .spawn(async move { api.create_agent_profile(&req).await })
                        .await
                        .map_err(|e| format!("task join failed: {e}"))
                        .and_then(|r| r.map_err(|e| format!("{e}")));
                    let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                        this.saving = false;
                        match result {
                            Ok(profile) => {
                                let id = profile.id.clone();
                                this.selected_profile_id = Some(id);
                                this.editor_mode = EditorMode::Edit;
                                this.save_error = None;
                                this.dirty = false;
                                this.field_errors.clear();
                                cx.emit(AgentProfilesTabEvent::ProfilesChanged);
                            }
                            Err(e) => {
                                this.save_error = Some(e);
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            EditorMode::Edit => {
                let Some(id) = self.selected_profile_id.clone() else {
                    return;
                };
                let req = match self.edit_form.to_update_request() {
                    Ok(r) => r,
                    Err(e) => {
                        self.save_error = Some(e);
                        cx.notify();
                        return;
                    }
                };
                self.save_error = None;
                self.saving = true;
                cx.notify();

                let api = self.app_state.api.clone();
                let handle = self.app_state.tokio_handle.clone();
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let id_for_api = id.clone();
                    let result = handle
                        .spawn(async move { api.update_agent_profile(&id_for_api, &req).await })
                        .await
                        .map_err(|e| format!("task join failed: {e}"))
                        .and_then(|r| r.map_err(|e| format!("{e}")));
                    let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                        this.saving = false;
                        match result {
                            Ok(_profile) => {
                                this.save_error = None;
                                this.dirty = false;
                                this.field_errors.clear();
                                cx.emit(AgentProfilesTabEvent::ProfilesChanged);
                            }
                            Err(e) => {
                                this.save_error = Some(e);
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some(id) = self.selected_profile_id.clone() else {
            return;
        };
        self.saving = true;
        self.save_error = None;
        cx.notify();

        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = handle
                .spawn(async move { api.delete_agent_profile(&id).await })
                .await
                .map_err(|e| format!("task join failed: {e}"))
                .and_then(|r| r.map_err(|e| format!("{e}")));
            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                this.saving = false;
                match result {
                    Ok(()) => {
                        this.selected_profile_id = None;
                        this.edit_form = EditForm::default();
                        this.save_error = None;
                        this.dirty = false;
                        this.field_errors.clear();
                        cx.emit(AgentProfilesTabEvent::ProfilesChanged);
                    }
                    Err(e) => {
                        this.save_error = Some(e);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_default(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some(id) = self.selected_profile_id.clone() else {
            return;
        };
        self.saving = true;
        self.save_error = None;
        cx.notify();

        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = handle
                .spawn(async move { api.set_default_agent_profile(&id).await })
                .await
                .map_err(|e| format!("task join failed: {e}"))
                .and_then(|r| r.map_err(|e| format!("{e}")));
            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                this.saving = false;
                match result {
                    Ok(_) => {
                        this.save_error = None;
                        cx.emit(AgentProfilesTabEvent::ProfilesChanged);
                    }
                    Err(e) => {
                        this.save_error = Some(e);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn duplicate(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some(id) = self.selected_profile_id.clone() else {
            return;
        };
        let Some(source) = self.profiles.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        let req = match duplicate_request(&source) {
            Ok(r) => r,
            Err(e) => {
                self.save_error = Some(e);
                cx.notify();
                return;
            }
        };

        self.saving = true;
        self.save_error = None;
        cx.notify();

        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = handle
                .spawn(async move { api.create_agent_profile(&req).await })
                .await
                .map_err(|e| format!("task join failed: {e}"))
                .and_then(|r| r.map_err(|e| format!("{e}")));
            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                this.saving = false;
                match result {
                    Ok(profile) => {
                        this.selected_profile_id = Some(profile.id);
                        this.editor_mode = EditorMode::Edit;
                        this.save_error = None;
                        this.dirty = false;
                        this.field_errors.clear();
                        cx.emit(AgentProfilesTabEvent::ProfilesChanged);
                    }
                    Err(e) => {
                        this.save_error = Some(e);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- Key routing -----------------------------------------------------

    fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        if self.active_input == ActiveInput::None {
            return;
        }

        // Enter on a "new chip" input adds the chip. Enter in the
        // multi-line Initial prompt buffer inserts a newline. All other
        // text inputs ignore Enter.
        if key == "enter" {
            if self.active_input == ActiveInput::InitialPrompt {
                self.edit_form.initial_prompt.push('\n');
                self.mark_dirty();
                cx.notify();
            } else {
                self.commit_chip_on_enter(cx);
            }
            cx.stop_propagation();
            return;
        }

        if key == "backspace" {
            self.active_buffer_mut().pop();
            self.mark_dirty();
            let active = self.active_input;
            self.revalidate_field(active);
            cx.notify();
            cx.stop_propagation();
            return;
        }

        // Consume ctrl/alt/platform combos so they don't leak into parent
        // handlers (the modal's escape handler still runs because we never
        // consume escape here).
        if mods.control || mods.alt || mods.platform {
            return;
        }

        if let Some(ch) = &event.keystroke.key_char {
            self.active_buffer_mut().push_str(ch);
            self.mark_dirty();
            let active = self.active_input;
            self.revalidate_field(active);
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn commit_chip_on_enter(&mut self, cx: &mut Context<Self>) {
        let mut committed = false;
        match self.active_input {
            ActiveInput::NewTool => {
                let v = std::mem::take(&mut self.edit_form.new_tool_input);
                if !v.is_empty() {
                    self.edit_form.allowed_tools.push(v);
                    committed = true;
                }
            }
            ActiveInput::NewArg => {
                let v = std::mem::take(&mut self.edit_form.new_arg_input);
                if !v.is_empty() {
                    self.edit_form.extra_args.push(v);
                    committed = true;
                }
            }
            ActiveInput::NewChannel => {
                let v = std::mem::take(&mut self.edit_form.new_channel_input);
                if !v.is_empty() {
                    self.edit_form.claude_development_channels.push(v);
                    committed = true;
                }
            }
            ActiveInput::NewCodexConfigOverride => {
                let v = std::mem::take(&mut self.edit_form.new_codex_config_override_input);
                if !v.is_empty() {
                    self.edit_form.codex_config_overrides.push(v);
                    committed = true;
                }
            }
            ActiveInput::NewEnvKey | ActiveInput::NewEnvValue => {
                let k = std::mem::take(&mut self.edit_form.new_env_key);
                let v = std::mem::take(&mut self.edit_form.new_env_value);
                if !k.is_empty() {
                    self.edit_form.env_vars.push((k, v));
                    committed = true;
                }
                self.active_input = ActiveInput::NewEnvKey;
            }
            _ => {}
        }
        if committed {
            self.mark_dirty();
        }
        self.revalidate_list_fields();
        cx.notify();
    }

    /// Removes a chip from the named tag list. Callers do not have to call
    /// `cx.notify()` -- this method handles the repaint itself so future call
    /// sites cannot silently drop the update.
    fn remove_tag(&mut self, kind: TagListKind, idx: usize, cx: &mut Context<Self>) {
        let vec = match kind {
            TagListKind::AllowedTools => &mut self.edit_form.allowed_tools,
            TagListKind::ExtraArgs => &mut self.edit_form.extra_args,
            TagListKind::DevelopmentChannels => &mut self.edit_form.claude_development_channels,
            TagListKind::CodexConfigOverrides => &mut self.edit_form.codex_config_overrides,
        };
        if idx < vec.len() {
            vec.remove(idx);
            self.dirty = true;
        }
        self.revalidate_list_fields();
        cx.notify();
    }

    fn active_buffer_mut(&mut self) -> &mut String {
        match self.active_input {
            ActiveInput::Name => &mut self.edit_form.name,
            ActiveInput::Description => &mut self.edit_form.description,
            ActiveInput::Model => &mut self.edit_form.model,
            ActiveInput::InitialPrompt => &mut self.edit_form.initial_prompt,
            ActiveInput::NewTool => &mut self.edit_form.new_tool_input,
            ActiveInput::NewArg => &mut self.edit_form.new_arg_input,
            ActiveInput::NewChannel => &mut self.edit_form.new_channel_input,
            ActiveInput::NewCodexConfigOverride => {
                &mut self.edit_form.new_codex_config_override_input
            }
            ActiveInput::NewEnvKey => &mut self.edit_form.new_env_key,
            ActiveInput::NewEnvValue => &mut self.edit_form.new_env_value,
            ActiveInput::ClaudeOutputFormat => &mut self.edit_form.claude_output_format,
            ActiveInput::ClaudeCustomFlags => &mut self.edit_form.claude_custom_flags,
            ActiveInput::CodexConfigProfile => &mut self.edit_form.codex_config_profile,
            ActiveInput::CodexSandbox => &mut self.edit_form.codex_sandbox,
            ActiveInput::CodexApprovalPolicy => &mut self.edit_form.codex_approval_policy,
            ActiveInput::CodexCustomFlags => &mut self.edit_form.codex_custom_flags,
            // `handle_key_down` early-exits when `active_input` is `None`,
            // so this arm can only be reached from a future call site that
            // forgets the guard. Panic loudly rather than silently routing
            // keystrokes to `edit_form.name`.
            ActiveInput::None => {
                unreachable!("active_buffer_mut called with ActiveInput::None")
            }
        }
    }

    // ---- Rendering helpers ----------------------------------------------

    fn render_left_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .id("agent-profiles-list")
            .flex()
            .flex_col()
            .w(px(240.0))
            .h_full()
            .border_r_1()
            .border_color(theme::border())
            .bg(theme::bg_secondary())
            .overflow_y_scroll();

        // Header: title + "New" button.
        list = list.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px(px(12.0))
                .py(px(10.0))
                .border_b_1()
                .border_color(theme::border())
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text_secondary())
                        .child("Profiles"),
                )
                .child(
                    div()
                        .id("new-profile-button")
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .px(px(6.0))
                        .py(px(2.0))
                        .rounded(px(4.0))
                        .bg(theme::bg_tertiary())
                        .text_size(px(11.0))
                        .text_color(theme::text_secondary())
                        .hover(|s| s.text_color(theme::text_primary()))
                        .child(icon(Icon::Plus).size(px(12.0)))
                        .child("New")
                        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.start_create(cx);
                        })),
                ),
        );

        if self.profiles.is_empty() {
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .py(px(32.0))
                    .px(px(16.0))
                    .child(
                        icon(Icon::Bot)
                            .size(px(24.0))
                            .text_color(theme::text_tertiary()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::text_tertiary())
                            .child("No profiles yet"),
                    )
                    .child(
                        div()
                            .id("empty-new-profile")
                            .cursor_pointer()
                            .px(px(10.0))
                            .py(px(4.0))
                            .rounded(px(4.0))
                            .bg(theme::accent())
                            .text_size(px(11.0))
                            .text_color(theme::text_primary())
                            .child("New Profile")
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                this.start_create(cx);
                            })),
                    ),
            );
            return list.into_any_element();
        }

        // Group profiles by agent_kind in declared order, fall back to
        // existing kind labels if a profile references an unknown kind.
        let mut kinds_seen: Vec<String> = Vec::new();
        for kind in self.kinds.iter() {
            kinds_seen.push(kind.kind.clone());
        }
        for profile in self.profiles.iter() {
            if !kinds_seen.contains(&profile.agent_kind) {
                kinds_seen.push(profile.agent_kind.clone());
            }
        }

        for kind in kinds_seen {
            let kind_profiles: Vec<&AgentProfile> = self
                .profiles
                .iter()
                .filter(|p| p.agent_kind == kind)
                .collect();
            if kind_profiles.is_empty() {
                continue;
            }
            list = list.child(
                div()
                    .px(px(12.0))
                    .pt(px(10.0))
                    .pb(px(4.0))
                    .text_size(px(10.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_tertiary())
                    .child(kind.to_uppercase()),
            );
            for profile in kind_profiles {
                list = list.child(self.render_profile_row(profile, cx));
            }
        }

        list.into_any_element()
    }

    fn render_profile_row(&self, profile: &AgentProfile, cx: &mut Context<Self>) -> AnyElement {
        let is_selected = self.editor_mode == EditorMode::Edit
            && self.selected_profile_id.as_deref() == Some(&profile.id);
        let profile_id = profile.id.clone();
        let name = profile.name.clone();
        let description = profile.description.clone();
        let is_default = profile.is_default;

        div()
            .id(SharedString::from(format!("profile-row-{}", profile.id)))
            .flex()
            .flex_col()
            .px(px(12.0))
            .py(px(6.0))
            .cursor_pointer()
            .when(is_selected, |s| s.bg(theme::bg_tertiary()))
            .hover(|s| s.bg(theme::bg_tertiary()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::text_primary())
                            .child(name),
                    )
                    .when(is_default, |s| {
                        s.child(
                            div()
                                .px(px(4.0))
                                .py(px(1.0))
                                .rounded(px(3.0))
                                .bg(theme::accent())
                                .text_size(px(9.0))
                                .text_color(theme::text_primary())
                                .child("default"),
                        )
                    }),
            )
            .when(description.is_some(), |s| {
                s.child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme::text_tertiary())
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(description.unwrap_or_default()),
                )
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.select_profile(&profile_id, cx);
            }))
            .into_any_element()
    }

    fn render_right_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        // Empty state: no profiles AND not creating.
        if self.profiles.is_empty() && self.editor_mode != EditorMode::Create {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.0))
                .child(
                    icon(Icon::Bot)
                        .size(px(32.0))
                        .text_color(theme::text_tertiary()),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme::text_secondary())
                        .child("No profiles yet"),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_tertiary())
                        .child("Create your first profile to get started"),
                )
                .child(
                    div()
                        .id("right-empty-new")
                        .cursor_pointer()
                        .mt(px(8.0))
                        .px(px(12.0))
                        .py(px(6.0))
                        .rounded(px(4.0))
                        .bg(theme::accent())
                        .text_size(px(12.0))
                        .text_color(theme::text_primary())
                        .child("New Profile")
                        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.start_create(cx);
                        })),
                )
                .into_any_element();
        }

        // Nothing selected but profiles exist -- shouldn't happen after
        // construction because we auto-select, but handle it defensively.
        if self.selected_profile_id.is_none() && self.editor_mode == EditorMode::Edit {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme::text_tertiary())
                        .child("Select a profile from the list"),
                )
                .into_any_element();
        }

        // Outer wrapper: a flex column that fills the right pane and owns
        // two stacked regions.
        //
        //   1. A scrollable form body (`flex_1 + min_h_0 + overflow_y_scroll`).
        //      `min_h_0` is the GPUI/taffy idiom for "inside a flex column,
        //      allow this child to shrink below its intrinsic height so its
        //      own overflow clips and scrolls". Without it the child grows
        //      to fit content and the scrollbar never appears.
        //   2. A sticky action bar, always rendered *outside* the scroll
        //      region so the Save/Delete/Duplicate/Set-default/Cancel buttons
        //      stay visible even when scrolled to the middle of a long form.
        div()
            .id("profile-editor")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("profile-editor-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.render_editor_body(cx)),
            )
            .child(self.render_action_bar(cx))
            .into_any_element()
    }

    fn render_editor_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = div().flex().flex_col().p(px(16.0)).gap(px(14.0));

        // --- Basics --------------------------------------------------------
        body = body.child(Self::render_section_header("Basics"));
        body = body.child(self.render_text_input(
            "Name",
            true,
            "Unique identifier for this profile (1-255 chars)",
            &self.edit_form.name,
            ActiveInput::Name,
            cx,
        ));
        body = body.child(self.render_text_input(
            "Description",
            false,
            "Optional: brief notes about what this profile does (max 1024 chars)",
            &self.edit_form.description,
            ActiveInput::Description,
            cx,
        ));

        // --- Launcher ------------------------------------------------------
        body = body.child(Self::render_section_header("Launcher"));
        body = body.child(self.render_kind_selector(cx));
        body = body.child(self.render_text_input(
            "Model",
            false,
            "Optional model id. Only alphanumerics, dots, dashes.",
            &self.edit_form.model,
            ActiveInput::Model,
            cx,
        ));
        body = body.child(self.render_textarea(
            "Initial prompt",
            false,
            "Optional: task or instruction. Press Enter for a new line. Max 64KB.",
            &self.edit_form.initial_prompt,
            ActiveInput::InitialPrompt,
            cx,
        ));
        body = body.child(self.render_checkbox(
            "Skip permissions",
            Some("Runs the launcher in its unsafe no-approval mode when the kind supports it."),
            self.edit_form.skip_permissions,
            cx.listener(|this, _: &ClickEvent, _w, cx| {
                this.edit_form.skip_permissions = !this.edit_form.skip_permissions;
                this.mark_dirty();
                cx.notify();
            }),
        ));

        // --- Tools ---------------------------------------------------------
        body = body.child(Self::render_section_header("Tools"));
        body = body.child(self.render_tag_list(
            "Allowed tools",
            "Restrict tools for launchers that support tool allowlists. Syntax: alphanumerics + underscore, colon, asterisk.",
            &self.edit_form.allowed_tools,
            &self.edit_form.new_tool_input,
            ActiveInput::NewTool,
            TagListKind::AllowedTools,
            cx,
        ));
        body = body.child(self.render_tag_list(
            "Extra args",
            "Pass additional CLI flags to the launcher. Each must start with '-' or '--'. No shell metacharacters.",
            &self.edit_form.extra_args,
            &self.edit_form.new_arg_input,
            ActiveInput::NewArg,
            TagListKind::ExtraArgs,
            cx,
        ));

        // --- Environment ---------------------------------------------------
        body = body.child(Self::render_section_header("Environment"));
        body = body.child(self.render_env_vars_editor(cx));

        // --- Claude-specific fields ----------------------------------------
        if self.edit_form.agent_kind == "claude" {
            body = body.child(Self::render_section_header("Claude settings"));
            body = body.child(self.render_claude_settings(cx));
        } else if self.edit_form.agent_kind == "codex" {
            body = body.child(Self::render_section_header("Codex settings"));
            body = body.child(self.render_codex_settings(cx));
        }

        body.into_any_element()
    }

    /// Top-of-section label: 13px semibold text_primary plus a thin bottom
    /// border that visually segments the form into logical groups (Basics,
    /// Launcher, Tools, Environment, Claude settings).
    fn render_section_header(title: &str) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .pt(px(4.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child(title.to_string()),
            )
            .child(div().h(px(1.0)).w_full().bg(theme::border()))
    }

    /// Render a field label row: "<label>" + optional red asterisk for
    /// required fields. 11px semibold text_secondary, matching the rest
    /// of the settings modal's label hierarchy.
    fn render_label(label: &str, required: bool) -> Div {
        let mut row = div().flex().items_center().gap(px(3.0)).child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_secondary())
                .child(label.to_string()),
        );
        if required {
            row = row.child(
                div()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::error())
                    .child("*"),
            );
        }
        row
    }

    /// Inline hint line shown beneath a field. Tertiary text, 11px, guides
    /// the user on format/purpose before they trigger server-side errors.
    fn render_hint(hint: &str) -> Div {
        div()
            .text_size(px(11.0))
            .text_color(theme::text_tertiary())
            .child(hint.to_string())
    }

    /// Inline field-level error line: rendered below the hint when the
    /// field's buffer fails client-side validation. Error color, 11px.
    fn render_field_error(msg: &str) -> Div {
        div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .text_size(px(11.0))
            .text_color(theme::error())
            .child(
                icon(Icon::AlertTriangle)
                    .size(px(11.0))
                    .text_color(theme::error()),
            )
            .child(msg.to_string())
    }

    fn render_kind_selector(&self, cx: &mut Context<Self>) -> AnyElement {
        let is_create = self.editor_mode == EditorMode::Create;
        let current_kind = self.edit_form.agent_kind.clone();
        let kinds = self.kinds.clone();

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(Self::render_label("Agent kind", true));

        if is_create {
            // Horizontal pill picker -- each kind is a clickable chip.
            let mut picker = div().flex().gap(px(6.0));
            for kind in kinds.iter() {
                let is_active = kind.kind == current_kind;
                let kind_id = kind.kind.clone();
                picker = picker.child(
                    div()
                        .id(SharedString::from(format!("kind-{}", kind.kind)))
                        .px(px(10.0))
                        .py(px(4.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_size(px(12.0))
                        .when(is_active, |s| {
                            s.bg(theme::accent()).text_color(theme::text_primary())
                        })
                        .when(!is_active, |s| {
                            s.bg(theme::bg_tertiary())
                                .text_color(theme::text_secondary())
                        })
                        .hover(|s| s.text_color(theme::text_primary()))
                        .child(kind.display_name.clone())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            if this.edit_form.agent_kind != kind_id {
                                // Symmetric reset of claude-specific fields so
                                // switching kinds never leaves stale hidden
                                // state behind. Applies both when leaving and
                                // when returning to "claude".
                                this.edit_form.claude_development_channels.clear();
                                this.edit_form.claude_output_format.clear();
                                this.edit_form.claude_print_mode = false;
                                this.edit_form.claude_custom_flags.clear();
                                this.edit_form.new_channel_input.clear();
                                this.edit_form.codex_config_profile.clear();
                                this.edit_form.codex_sandbox.clear();
                                this.edit_form.codex_approval_policy.clear();
                                this.edit_form.codex_config_overrides.clear();
                                this.edit_form.codex_search = false;
                                this.edit_form.codex_no_alt_screen = false;
                                this.edit_form.codex_custom_flags.clear();
                                this.edit_form.new_codex_config_override_input.clear();
                                this.field_errors.remove(&ActiveInput::NewChannel);
                                this.field_errors.remove(&ActiveInput::ClaudeOutputFormat);
                                this.field_errors.remove(&ActiveInput::ClaudeCustomFlags);
                                this.field_errors.remove(&ActiveInput::CodexConfigProfile);
                                this.field_errors.remove(&ActiveInput::CodexSandbox);
                                this.field_errors.remove(&ActiveInput::CodexApprovalPolicy);
                                this.field_errors
                                    .remove(&ActiveInput::NewCodexConfigOverride);
                                this.field_errors.remove(&ActiveInput::CodexCustomFlags);
                                this.edit_form.agent_kind = kind_id.clone();
                                this.mark_dirty();
                                cx.notify();
                            }
                        })),
                );
            }
            wrapper = wrapper.child(picker);
            wrapper = wrapper.child(Self::render_hint(
                "Which launcher runs the agent. Cannot be changed after creation.",
            ));
        } else {
            let display = kinds
                .iter()
                .find(|k| k.kind == current_kind)
                .map(|k| k.display_name.clone())
                .unwrap_or(current_kind);
            wrapper = wrapper.child(
                div()
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .bg(theme::bg_tertiary())
                    .border_1()
                    .border_color(theme::border())
                    .text_size(px(12.0))
                    .text_color(theme::text_tertiary())
                    .child(display),
            );
            wrapper = wrapper.child(Self::render_hint(
                "Launcher kind is fixed once the profile is created.",
            ));
        }

        wrapper.into_any_element()
    }

    fn render_text_input(
        &self,
        label: &str,
        required: bool,
        hint: &str,
        value: &str,
        field: ActiveInput,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_active = self.active_input == field;
        let has_error = self.field_errors.contains_key(&field);
        let input_id = SharedString::from(format!("input-{label}"));
        let border = if has_error {
            theme::error()
        } else if is_active {
            theme::accent()
        } else {
            theme::border()
        };
        let display_value = value.to_string();

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(Self::render_label(label, required))
            .child(
                div()
                    .id(input_id)
                    .cursor_pointer()
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(4.0))
                    .bg(theme::bg_tertiary())
                    .border_1()
                    .border_color(border)
                    .text_size(px(12.0))
                    .text_color(theme::text_primary())
                    .min_h(px(28.0))
                    .hover(|s| s.border_color(theme::accent()))
                    .child(if display_value.is_empty() {
                        div()
                            .text_color(theme::text_tertiary())
                            .child("click to edit")
                            .into_any_element()
                    } else {
                        div().child(display_value).into_any_element()
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.active_input = field;
                        cx.notify();
                    })),
            );
        if !hint.is_empty() {
            wrapper = wrapper.child(Self::render_hint(hint));
        }
        if let Some(err) = self.field_errors.get(&field) {
            wrapper = wrapper.child(Self::render_field_error(err));
        }
        wrapper.into_any_element()
    }

    fn render_textarea(
        &self,
        label: &str,
        required: bool,
        hint: &str,
        value: &str,
        field: ActiveInput,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_active = self.active_input == field;
        let has_error = self.field_errors.contains_key(&field);
        let border = if has_error {
            theme::error()
        } else if is_active {
            theme::accent()
        } else {
            theme::border()
        };
        let input_id = SharedString::from(format!("textarea-{label}"));
        let display_value = value.to_string();

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(Self::render_label(label, required))
            .child(
                div()
                    .id(input_id)
                    .cursor_pointer()
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(4.0))
                    .bg(theme::bg_tertiary())
                    .border_1()
                    .border_color(border)
                    .text_size(px(12.0))
                    .text_color(theme::text_primary())
                    .min_h(px(60.0))
                    .hover(|s| s.border_color(theme::accent()))
                    .child(if display_value.is_empty() {
                        div()
                            .text_color(theme::text_tertiary())
                            .child("click to edit")
                            .into_any_element()
                    } else {
                        // Split on '\n' so multi-line prompts render as
                        // multiple lines. GPUI does not honor literal
                        // newlines inside a single text node -- each line
                        // must be its own child div for wrapping to work.
                        // An empty trailing line (from a fresh newline)
                        // still renders as a zero-height row, giving the
                        // user a visual cue that the cursor moved down.
                        let mut column = div().flex().flex_col();
                        for line in display_value.split('\n') {
                            column = column.child(div().min_h(px(14.0)).child(line.to_string()));
                        }
                        column.into_any_element()
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.active_input = field;
                        cx.notify();
                    })),
            );
        if !hint.is_empty() {
            wrapper = wrapper.child(Self::render_hint(hint));
        }
        if let Some(err) = self.field_errors.get(&field) {
            wrapper = wrapper.child(Self::render_field_error(err));
        }
        wrapper.into_any_element()
    }

    fn render_checkbox(
        &self,
        label: &str,
        hint: Option<&str>,
        checked: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let label_owned = label.to_string();
        let id = SharedString::from(format!("checkbox-{label}"));
        let row = div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .on_click(on_click)
            .child(
                div()
                    .w(px(14.0))
                    .h(px(14.0))
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(theme::border())
                    .bg(if checked {
                        theme::accent()
                    } else {
                        theme::bg_tertiary()
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if checked {
                        icon(Icon::CheckCircle)
                            .size(px(10.0))
                            .text_color(theme::text_primary())
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text_primary())
                    .child(label_owned),
            );

        let mut wrapper = div().flex().flex_col().gap(px(2.0)).child(row);
        if let Some(h) = hint {
            // Indent the hint so it aligns under the label, not the checkbox.
            wrapper = wrapper.child(div().pl(px(22.0)).child(Self::render_hint(h)));
        }
        wrapper.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_tag_list(
        &self,
        label: &str,
        hint: &str,
        values: &[String],
        new_value: &str,
        new_field: ActiveInput,
        kind: TagListKind,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_active = self.active_input == new_field;
        let has_error = self.field_errors.contains_key(&new_field);
        let border = if has_error {
            theme::error()
        } else if is_active {
            theme::accent()
        } else {
            theme::border()
        };
        let input_id = SharedString::from(format!("taglist-input-{label}"));
        let new_display = new_value.to_string();

        let mut chips = div().flex().flex_wrap().gap(px(4.0));
        for (idx, value) in values.iter().enumerate() {
            let chip_id = SharedString::from(format!("chip-{label}-{idx}"));
            chips = chips.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(3.0))
                    .bg(theme::bg_tertiary())
                    .border_1()
                    .border_color(theme::border())
                    .text_size(px(11.0))
                    .text_color(theme::text_primary())
                    .child(value.clone())
                    .child(
                        div()
                            .id(chip_id)
                            .cursor_pointer()
                            .text_color(theme::text_tertiary())
                            .hover(|s| s.text_color(theme::error()))
                            .child(icon(Icon::X).size(px(10.0)))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                this.remove_tag(kind, idx, cx);
                            })),
                    ),
            );
        }

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(Self::render_label(label, false))
            .child(chips)
            .child(
                div()
                    .id(input_id)
                    .cursor_pointer()
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .bg(theme::bg_tertiary())
                    .border_1()
                    .border_color(border)
                    .text_size(px(11.0))
                    .text_color(theme::text_primary())
                    .min_h(px(24.0))
                    .hover(|s| s.border_color(theme::accent()))
                    .child(if new_display.is_empty() {
                        div()
                            .text_color(theme::text_tertiary())
                            .child("click, type, Enter to add")
                            .into_any_element()
                    } else {
                        div().child(new_display).into_any_element()
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.active_input = new_field;
                        cx.notify();
                    })),
            );
        if !hint.is_empty() {
            wrapper = wrapper.child(Self::render_hint(hint));
        }
        if let Some(err) = self.field_errors.get(&new_field) {
            wrapper = wrapper.child(Self::render_field_error(err));
        }
        wrapper.into_any_element()
    }

    fn render_env_vars_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = div().flex().flex_col().gap(px(4.0));
        for (idx, (k, v)) in self.edit_form.env_vars.iter().enumerate() {
            let row_id = SharedString::from(format!("env-row-{idx}"));
            let remove_id = SharedString::from(format!("env-remove-{idx}"));
            rows = rows.child(
                div()
                    .id(row_id)
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .px(px(6.0))
                            .py(px(3.0))
                            .rounded(px(3.0))
                            .bg(theme::bg_tertiary())
                            .border_1()
                            .border_color(theme::border())
                            .text_size(px(11.0))
                            .text_color(theme::text_primary())
                            .child(k.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_tertiary())
                            .child("="),
                    )
                    .child(
                        div()
                            .flex_1()
                            .px(px(6.0))
                            .py(px(3.0))
                            .rounded(px(3.0))
                            .bg(theme::bg_tertiary())
                            .border_1()
                            .border_color(theme::border())
                            .text_size(px(11.0))
                            .text_color(theme::text_primary())
                            .child(v.clone()),
                    )
                    .child(
                        div()
                            .id(remove_id)
                            .cursor_pointer()
                            .text_color(theme::text_tertiary())
                            .hover(|s| s.text_color(theme::error()))
                            .child(icon(Icon::X).size(px(12.0)))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                if idx < this.edit_form.env_vars.len() {
                                    this.edit_form.env_vars.remove(idx);
                                    this.mark_dirty();
                                    cx.notify();
                                }
                            })),
                    ),
            );
        }

        let key_has_error = self.field_errors.contains_key(&ActiveInput::NewEnvKey);
        let value_has_error = self.field_errors.contains_key(&ActiveInput::NewEnvValue);
        let key_border = if key_has_error {
            theme::error()
        } else if self.active_input == ActiveInput::NewEnvKey {
            theme::accent()
        } else {
            theme::border()
        };
        let value_border = if value_has_error {
            theme::error()
        } else if self.active_input == ActiveInput::NewEnvValue {
            theme::accent()
        } else {
            theme::border()
        };
        let key_display = self.edit_form.new_env_key.clone();
        let value_display = self.edit_form.new_env_value.clone();

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(Self::render_label("Environment variables", false))
            .child(rows)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .id("new-env-key")
                            .cursor_pointer()
                            .px(px(6.0))
                            .py(px(3.0))
                            .rounded(px(3.0))
                            .bg(theme::bg_tertiary())
                            .border_1()
                            .border_color(key_border)
                            .text_size(px(11.0))
                            .text_color(theme::text_primary())
                            .min_w(px(80.0))
                            .hover(|s| s.border_color(theme::accent()))
                            .child(if key_display.is_empty() {
                                div()
                                    .text_color(theme::text_tertiary())
                                    .child("KEY")
                                    .into_any_element()
                            } else {
                                div().child(key_display).into_any_element()
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                this.active_input = ActiveInput::NewEnvKey;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::text_secondary())
                            .child("="),
                    )
                    .child(
                        div()
                            .id("new-env-value")
                            .cursor_pointer()
                            .flex_1()
                            .px(px(6.0))
                            .py(px(3.0))
                            .rounded(px(3.0))
                            .bg(theme::bg_tertiary())
                            .border_1()
                            .border_color(value_border)
                            .text_size(px(11.0))
                            .text_color(theme::text_primary())
                            .hover(|s| s.border_color(theme::accent()))
                            .child(if value_display.is_empty() {
                                div()
                                    .text_color(theme::text_tertiary())
                                    .child("value")
                                    .into_any_element()
                            } else {
                                div().child(value_display).into_any_element()
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                this.active_input = ActiveInput::NewEnvValue;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("new-env-add")
                            .cursor_pointer()
                            .px(px(8.0))
                            .py(px(3.0))
                            .rounded(px(3.0))
                            .bg(theme::bg_tertiary())
                            .text_size(px(11.0))
                            .text_color(theme::text_secondary())
                            .hover(|s| s.text_color(theme::text_primary()))
                            .child("Add")
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                // Only commit if the key buffer is non-empty
                                // AND free of validation errors. Otherwise we
                                // would silently swallow the user's input.
                                if this.edit_form.new_env_key.is_empty()
                                    || this.field_errors.contains_key(&ActiveInput::NewEnvKey)
                                    || this.field_errors.contains_key(&ActiveInput::NewEnvValue)
                                {
                                    cx.notify();
                                    return;
                                }
                                let k = std::mem::take(&mut this.edit_form.new_env_key);
                                let v = std::mem::take(&mut this.edit_form.new_env_value);
                                this.edit_form.env_vars.push((k, v));
                                this.mark_dirty();
                                this.revalidate_list_fields();
                                cx.notify();
                            })),
                    ),
            )
            .child(Self::render_hint(
                "POSIX key names: letter or underscore first, then alphanumerics/underscore. Values are exported to the process.",
            ));

        if let Some(err) = self.field_errors.get(&ActiveInput::NewEnvKey) {
            wrapper = wrapper.child(Self::render_field_error(err));
        }
        if let Some(err) = self.field_errors.get(&ActiveInput::NewEnvValue) {
            wrapper = wrapper.child(Self::render_field_error(err));
        }
        wrapper.into_any_element()
    }

    fn render_claude_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        // The section header is rendered by `render_editor_body` -- keep this
        // helper focused on Claude-specific fields so the layout isn't
        // double-bordered.
        let mut section = div().flex().flex_col().gap(px(12.0));

        // Development channels (tag list)
        section = section.child(self.render_tag_list(
            "Development channels",
            "Claude Code channels to auto-approve (e.g., \"plugin:zremote@local\"). Colon, slash, dot, dash, at allowed.",
            &self.edit_form.claude_development_channels,
            &self.edit_form.new_channel_input,
            ActiveInput::NewChannel,
            TagListKind::DevelopmentChannels,
            cx,
        ));

        // Output format
        section = section.child(self.render_text_input(
            "Output format",
            false,
            "Optional: e.g., \"stream-json\", \"text\". Alphanumerics, dash, underscore only.",
            &self.edit_form.claude_output_format,
            ActiveInput::ClaudeOutputFormat,
            cx,
        ));

        // Print mode checkbox
        section = section.child(self.render_checkbox(
            "Print mode (-p)",
            Some("When checked, claude responds once and exits. Useful for automated tasks."),
            self.edit_form.claude_print_mode,
            cx.listener(|this, _: &ClickEvent, _w, cx| {
                this.edit_form.claude_print_mode = !this.edit_form.claude_print_mode;
                this.mark_dirty();
                cx.notify();
            }),
        ));

        // Custom flags (free-form)
        section = section.child(self.render_text_input(
            "Custom flags",
            false,
            "Optional: free-form flags appended to the command line. No shell metacharacters or backslash.",
            &self.edit_form.claude_custom_flags,
            ActiveInput::ClaudeCustomFlags,
            cx,
        ));

        section.into_any_element()
    }

    fn render_codex_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut section = div().flex().flex_col().gap(px(12.0));

        section = section.child(self.render_text_input(
            "Config profile",
            false,
            "Optional: passed to codex --profile. Alphanumerics, dot, dash, underscore only.",
            &self.edit_form.codex_config_profile,
            ActiveInput::CodexConfigProfile,
            cx,
        ));

        section = section.child(self.render_text_input(
            "Sandbox",
            false,
            "Optional: read-only, workspace-write, or danger-full-access.",
            &self.edit_form.codex_sandbox,
            ActiveInput::CodexSandbox,
            cx,
        ));

        section = section.child(self.render_text_input(
            "Approval policy",
            false,
            "Optional: untrusted, on-failure, on-request, or never.",
            &self.edit_form.codex_approval_policy,
            ActiveInput::CodexApprovalPolicy,
            cx,
        ));

        section = section.child(self.render_checkbox(
            "Search",
            Some("Pass --search so Codex can use live web search."),
            self.edit_form.codex_search,
            cx.listener(|this, _: &ClickEvent, _w, cx| {
                this.edit_form.codex_search = !this.edit_form.codex_search;
                this.mark_dirty();
                cx.notify();
            }),
        ));

        section = section.child(self.render_checkbox(
            "No alternate screen",
            Some("Pass --no-alt-screen so terminal scrollback remains visible."),
            self.edit_form.codex_no_alt_screen,
            cx.listener(|this, _: &ClickEvent, _w, cx| {
                this.edit_form.codex_no_alt_screen = !this.edit_form.codex_no_alt_screen;
                this.mark_dirty();
                cx.notify();
            }),
        ));

        section = section.child(self.render_tag_list(
            "Config overrides",
            "Passed as repeated -c key=value overrides, e.g. model_reasoning_effort=\"high\".",
            &self.edit_form.codex_config_overrides,
            &self.edit_form.new_codex_config_override_input,
            ActiveInput::NewCodexConfigOverride,
            TagListKind::CodexConfigOverrides,
            cx,
        ));

        section = section.child(self.render_text_input(
            "Custom flags",
            false,
            "Optional: free-form flags appended to the command line. No shell metacharacters or backslash.",
            &self.edit_form.codex_custom_flags,
            ActiveInput::CodexCustomFlags,
            cx,
        ));

        section.into_any_element()
    }

    fn render_action_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        // Delete / Duplicate / Set-as-Default all share the same gating
        // rule: only meaningful when an existing profile is selected.
        let has_selected_profile =
            self.editor_mode == EditorMode::Edit && self.selected_profile_id.is_some();
        let can_delete = has_selected_profile;
        let can_duplicate = has_selected_profile;
        let can_set_default = has_selected_profile;
        let save_label = match self.editor_mode {
            EditorMode::Create => "Create",
            EditorMode::Edit => "Save",
        };
        let saving = self.saving;

        let mut row = div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(16.0))
            .py(px(10.0))
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::bg_secondary());

        // Left: error text
        let error_block = if let Some(err) = &self.save_error {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .text_color(theme::error())
                .child(
                    icon(Icon::AlertTriangle)
                        .size(px(12.0))
                        .text_color(theme::error()),
                )
                .child(err.clone())
                .into_any_element()
        } else if saving {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .text_color(theme::text_secondary())
                .child(
                    icon(Icon::Loader)
                        .size(px(12.0))
                        .text_color(theme::text_secondary()),
                )
                .child("working...")
                .into_any_element()
        } else {
            div().into_any_element()
        };

        row = row.child(div().flex_1().child(error_block));

        // Right: action buttons. While `saving` is true every button is
        // visually dimmed and clicks early-return in the handlers anyway.
        // This prevents spam-clicks queueing multiple in-flight CRUD calls.
        let mut buttons = div().flex().items_center().gap(px(6.0));

        if can_set_default {
            buttons = buttons.child(
                div()
                    .id("btn-set-default")
                    .cursor_pointer()
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .bg(theme::bg_tertiary())
                    .text_size(px(11.0))
                    .text_color(theme::text_secondary())
                    .when(saving, |s| s.opacity(0.5))
                    .hover(|s| s.text_color(theme::text_primary()))
                    .child("Set default")
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.set_default(cx);
                    })),
            );
        }

        if can_duplicate {
            buttons = buttons.child(
                div()
                    .id("btn-duplicate")
                    .cursor_pointer()
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .bg(theme::bg_tertiary())
                    .text_size(px(11.0))
                    .text_color(theme::text_secondary())
                    .when(saving, |s| s.opacity(0.5))
                    .hover(|s| s.text_color(theme::text_primary()))
                    .child("Duplicate")
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.duplicate(cx);
                    })),
            );
        }

        if can_delete {
            buttons = buttons.child(
                div()
                    .id("btn-delete")
                    .cursor_pointer()
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .bg(theme::bg_tertiary())
                    .text_size(px(11.0))
                    .text_color(theme::error())
                    .when(saving, |s| s.opacity(0.5))
                    .hover(|s| s.bg(theme::warning_bg()))
                    .child("Delete")
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.delete(cx);
                    })),
            );
        }

        buttons = buttons.child(
            div()
                .id("btn-save")
                .cursor_pointer()
                .px(px(12.0))
                .py(px(4.0))
                .rounded(px(4.0))
                .bg(theme::accent())
                .text_size(px(11.0))
                .text_color(theme::text_primary())
                .when(saving, |s| s.opacity(0.5))
                .child(save_label)
                .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                    this.save(cx);
                })),
        );

        row = row.child(buttons);
        row.into_any_element()
    }
}

/// Build a `CreateAgentProfileRequest` that clones `source` with a
/// `" (Copy)"`-suffixed name. Kept as a free function so tests can exercise
/// the cloning logic without constructing a full tab entity.
fn duplicate_request(source: &AgentProfile) -> Result<CreateAgentProfileRequest, String> {
    if source.name.is_empty() {
        return Err("source profile has no name".to_string());
    }
    Ok(CreateAgentProfileRequest {
        name: format!("{} (Copy)", source.name),
        description: source.description.clone(),
        agent_kind: source.agent_kind.clone(),
        is_default: false,
        sort_order: source.sort_order,
        model: source.model.clone(),
        initial_prompt: source.initial_prompt.clone(),
        skip_permissions: source.skip_permissions,
        allowed_tools: source.allowed_tools.clone(),
        extra_args: source.extra_args.clone(),
        env_vars: source.env_vars.clone(),
        settings: source.settings.clone(),
    })
}

impl Render for AgentProfilesTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("agent-profiles-tab")
            .track_focus(&self.focus_handle)
            .flex()
            .size_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _w, cx| {
                // Escape is handled by the parent modal (we don't consume it).
                if event.keystroke.key.as_str() == "escape" {
                    return;
                }
                this.handle_key_down(event, cx);
            }))
            .child(self.render_left_pane(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .bg(theme::bg_primary())
                    .child(self.render_right_pane(cx)),
            )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // NOTE: `use gpui::*;` in the parent module brings `gpui::test` into
    // scope, which shadows the std `#[test]` attribute and causes a stack
    // overflow inside `gpui_macros` for non-async tests. Use the fully
    // qualified `#[core::prelude::rust_2021::test]` on every test function
    // in this module -- same pattern as `views::toast::tests`.
    use super::*;
    use std::collections::BTreeMap;

    fn sample_profile() -> AgentProfile {
        let mut env = BTreeMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        AgentProfile {
            id: "p1".to_string(),
            name: "Default".to_string(),
            description: Some("sample".to_string()),
            agent_kind: "claude".to_string(),
            is_default: true,
            sort_order: 0,
            model: Some("opus-4".to_string()),
            initial_prompt: Some("hi".to_string()),
            skip_permissions: false,
            allowed_tools: vec!["Read".to_string(), "Edit".to_string()],
            extra_args: vec!["--verbose".to_string()],
            env_vars: env,
            settings: json!({
                "development_channels": ["plugin:zremote@local"],
                "output_format": "stream-json",
                "print_mode": true,
                "custom_flags": "--extra-thinking",
            }),
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
        }
    }

    fn sample_codex_profile() -> AgentProfile {
        AgentProfile {
            id: "codex-1".to_string(),
            name: "Codex Default".to_string(),
            description: None,
            agent_kind: "codex".to_string(),
            is_default: true,
            sort_order: 0,
            model: Some("gpt-5.1-codex".to_string()),
            initial_prompt: Some("hi".to_string()),
            skip_permissions: true,
            allowed_tools: vec![],
            extra_args: vec!["--oss".to_string()],
            env_vars: BTreeMap::new(),
            settings: json!({
                "config_profile": "work",
                "sandbox": "workspace-write",
                "approval_policy": "on-request",
                "config_overrides": ["model_reasoning_effort=\"high\""],
                "search": true,
                "no_alt_screen": true,
                "custom_flags": "--enable experimental",
            }),
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
        }
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_from_profile_round_trips_claude_settings() {
        let profile = sample_profile();
        let form = EditForm::from_profile(&profile);
        assert_eq!(form.name, "Default");
        assert_eq!(form.description, "sample");
        assert_eq!(form.agent_kind, "claude");
        assert_eq!(form.model, "opus-4");
        assert_eq!(form.initial_prompt, "hi");
        assert_eq!(form.allowed_tools, vec!["Read", "Edit"]);
        assert_eq!(form.extra_args, vec!["--verbose"]);
        assert_eq!(form.env_vars, vec![("FOO".to_string(), "bar".to_string())]);
        assert_eq!(
            form.claude_development_channels,
            vec!["plugin:zremote@local"]
        );
        assert_eq!(form.claude_output_format, "stream-json");
        assert!(form.claude_print_mode);
        assert_eq!(form.claude_custom_flags, "--extra-thinking");
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_to_update_request_happy_path() {
        let profile = sample_profile();
        let form = EditForm::from_profile(&profile);
        let req = form.to_update_request().expect("should validate");
        assert_eq!(req.name, "Default");
        assert_eq!(req.model.as_deref(), Some("opus-4"));
        assert_eq!(req.allowed_tools, vec!["Read", "Edit"]);
        assert_eq!(req.extra_args, vec!["--verbose"]);
        assert_eq!(req.env_vars.get("FOO").map(String::as_str), Some("bar"));
        // Claude settings JSON should round-trip through non-empty fields.
        let settings = &req.settings;
        assert_eq!(
            settings
                .get("development_channels")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );
        assert_eq!(
            settings.get("output_format").and_then(|v| v.as_str()),
            Some("stream-json")
        );
        assert_eq!(
            settings
                .get("print_mode")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            settings.get("custom_flags").and_then(|v| v.as_str()),
            Some("--extra-thinking")
        );
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_to_create_request_happy_path() {
        let profile = sample_profile();
        let form = EditForm::from_profile(&profile);
        let req = form.to_create_request().expect("should validate");
        assert_eq!(req.name, "Default");
        assert_eq!(req.agent_kind, "claude");
        assert!(!req.is_default); // Create never sets default.
        assert_eq!(req.allowed_tools, vec!["Read", "Edit"]);
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_from_profile_round_trips_codex_settings() {
        let profile = sample_codex_profile();
        let form = EditForm::from_profile(&profile);
        assert_eq!(form.agent_kind, "codex");
        assert_eq!(form.codex_config_profile, "work");
        assert_eq!(form.codex_sandbox, "workspace-write");
        assert_eq!(form.codex_approval_policy, "on-request");
        assert_eq!(
            form.codex_config_overrides,
            vec!["model_reasoning_effort=\"high\""]
        );
        assert!(form.codex_search);
        assert!(form.codex_no_alt_screen);
        assert_eq!(form.codex_custom_flags, "--enable experimental");
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_to_update_request_round_trips_codex_settings() {
        let form = EditForm::from_profile(&sample_codex_profile());
        let req = form.to_update_request().expect("should validate");
        let settings = &req.settings;
        assert_eq!(
            settings.get("config_profile").and_then(|v| v.as_str()),
            Some("work")
        );
        assert_eq!(
            settings.get("sandbox").and_then(|v| v.as_str()),
            Some("workspace-write")
        );
        assert_eq!(
            settings.get("approval_policy").and_then(|v| v.as_str()),
            Some("on-request")
        );
        assert_eq!(
            settings
                .get("config_overrides")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );
        assert_eq!(
            settings.get("search").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            settings
                .get("no_alt_screen")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            settings.get("custom_flags").and_then(|v| v.as_str()),
            Some("--enable experimental")
        );
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_empty_name() {
        let mut form = EditForm::from_profile(&sample_profile());
        form.name = String::new();
        let result = form.to_update_request();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Name is required"));
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_shell_metachars_in_tools() {
        let mut form = EditForm::from_profile(&sample_profile());
        form.allowed_tools = vec!["Read;ls".to_string()];
        assert!(form.to_update_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_bad_extra_arg() {
        let mut form = EditForm::from_profile(&sample_profile());
        form.extra_args = vec!["not-a-flag".to_string()];
        assert!(form.to_update_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_shell_metachars_in_extra_arg() {
        let mut form = EditForm::from_profile(&sample_profile());
        form.extra_args = vec!["--foo;ls".to_string()];
        assert!(form.to_update_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_bad_env_key() {
        let mut form = EditForm::from_profile(&sample_profile());
        form.env_vars = vec![("1BAD".to_string(), "ok".to_string())];
        assert!(form.to_update_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_env_value_with_newline() {
        let mut form = EditForm::from_profile(&sample_profile());
        form.env_vars = vec![("GOOD".to_string(), "bad\nvalue".to_string())];
        assert!(form.to_update_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_bad_model() {
        let mut form = EditForm::from_profile(&sample_profile());
        form.model = "opus;ls".to_string();
        assert!(form.to_update_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_bad_output_format() {
        let mut form = EditForm::from_profile(&sample_profile());
        form.claude_output_format = "json format".to_string();
        assert!(form.to_update_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_bad_custom_flags() {
        let mut form = EditForm::from_profile(&sample_profile());
        form.claude_custom_flags = "--foo;rm".to_string();
        assert!(form.to_update_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_bad_development_channel() {
        let mut form = EditForm::from_profile(&sample_profile());
        form.claude_development_channels = vec!["plugin with space".to_string()];
        assert!(form.to_update_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_bad_codex_sandbox() {
        let mut form = EditForm::from_profile(&sample_codex_profile());
        form.codex_sandbox = "sometimes".to_string();
        assert!(form.to_update_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_rejects_bad_codex_config_override() {
        let mut form = EditForm::from_profile(&sample_codex_profile());
        form.codex_config_overrides = vec!["bad key=value".to_string()];
        assert!(form.to_update_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn edit_form_blank_has_kind_default() {
        let form = EditForm::blank("claude".to_string());
        assert_eq!(form.agent_kind, "claude");
        assert!(form.name.is_empty());
        // Cannot save a blank form (name is required).
        assert!(form.to_create_request().is_err());
    }

    #[core::prelude::rust_2021::test]
    fn duplicate_request_clones_and_suffixes_name() {
        let profile = sample_profile();
        let req = duplicate_request(&profile).expect("should clone");
        assert_eq!(req.name, "Default (Copy)");
        assert_eq!(req.agent_kind, "claude");
        assert!(!req.is_default);
        assert_eq!(req.allowed_tools, profile.allowed_tools);
        assert_eq!(req.extra_args, profile.extra_args);
        assert_eq!(req.env_vars, profile.env_vars);
        assert_eq!(req.settings, profile.settings);
    }

    #[core::prelude::rust_2021::test]
    fn duplicate_request_rejects_empty_source_name() {
        let mut profile = sample_profile();
        profile.name = String::new();
        assert!(duplicate_request(&profile).is_err());
    }

    #[core::prelude::rust_2021::test]
    fn settings_json_empty_when_no_claude_fields_set() {
        let form = EditForm {
            agent_kind: "claude".to_string(),
            ..Default::default()
        };
        let settings = form.settings_json();
        // Empty object -- nothing to preserve, server accepts this.
        assert_eq!(settings, json!({}));
    }

    #[core::prelude::rust_2021::test]
    fn settings_json_null_for_unknown_kind() {
        let form = EditForm {
            agent_kind: "future-kind".to_string(),
            ..Default::default()
        };
        assert_eq!(form.settings_json(), serde_json::Value::Null);
    }

    #[core::prelude::rust_2021::test]
    fn settings_json_serializes_custom_flags_as_string() {
        // Guard against regression to Vec<String> shape -- the server
        // validator's ClaudeSettingsShape decodes this as Option<String>,
        // so the JSON must be a scalar string, not a sequence.
        let form = EditForm {
            agent_kind: "claude".to_string(),
            claude_custom_flags: "--verbose --debug".to_string(),
            ..Default::default()
        };
        let settings = form.settings_json();
        assert_eq!(
            settings.get("custom_flags").and_then(|v| v.as_str()),
            Some("--verbose --debug"),
            "custom_flags must serialize as a JSON string, not an array"
        );
        assert!(
            settings.get("custom_flags").is_none_or(|v| !v.is_array()),
            "custom_flags must NOT be a JSON array"
        );
    }

    #[core::prelude::rust_2021::test]
    fn settings_json_omits_empty_custom_flags() {
        let form = EditForm {
            agent_kind: "claude".to_string(),
            ..Default::default()
        };
        let settings = form.settings_json();
        assert!(
            settings.get("custom_flags").is_none(),
            "empty custom_flags must not be serialized"
        );
    }
}

//! Centralized keyboard binding registry and dispatch.
//!
//! All global shortcuts are defined once here. Individual view handlers call
//! `dispatch_global_key()` instead of duplicating match logic. Modal-scoped
//! handlers (palette navigation, search input, switcher cycling) remain on
//! their own focused elements — only the shared global shortcuts are centralized.

use crate::views::command_palette::PaletteTab;

// ---------------------------------------------------------------------------
// Binding data
// ---------------------------------------------------------------------------

/// Modifier requirements for a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyModifiers {
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
}

impl KeyModifiers {
    const fn new(control: bool, shift: bool, alt: bool) -> Self {
        Self {
            control,
            shift,
            alt,
        }
    }

    fn matches(self, control: bool, shift: bool, alt: bool) -> bool {
        self.control == control && self.shift == shift && self.alt == alt
    }
}

/// Scope in which a binding is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyScope {
    /// Active everywhere (empty state, terminal, unless a modal consumes first).
    Global,
    /// Active only when a modal is focused (Esc to close).
    Modal,
}

/// A registered keyboard binding.
#[derive(Debug, Clone)]
pub struct KeyBinding {
    pub key: &'static str,
    pub modifiers: KeyModifiers,
    pub scope: KeyScope,
    pub action: KeyAction,
    /// Display label for the help modal (e.g. "Ctrl+K").
    pub label: &'static str,
    /// Description for the help modal.
    pub description: &'static str,
}

/// Actions that the dispatch function can return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    OpenCommandPalette(PaletteTab),
    OpenSessionSwitcher,
    OpenSearch,
    OpenHelp,
    CloseOverlay,
    ToggleActivityPanel,
    OpenSessionInNewWindow,
    /// Open the new-worktree creation modal for the current-parent context.
    /// Phase 2 ships a single-step shortcut; the D4 leader chord (`Cmd+K, n`)
    /// lands once the leader dispatch infrastructure is in place (Phase 3).
    OpenNewWorktree,
}

// ---------------------------------------------------------------------------
// Binding registry
// ---------------------------------------------------------------------------

/// All registered keyboard bindings. Order determines help modal display order.
pub static BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        key: "k",
        modifiers: KeyModifiers::new(true, false, false),
        scope: KeyScope::Global,
        action: KeyAction::OpenCommandPalette(PaletteTab::All),
        label: "Ctrl+K",
        description: "Command palette",
    },
    KeyBinding {
        key: "tab",
        modifiers: KeyModifiers::new(true, false, false),
        scope: KeyScope::Global,
        action: KeyAction::OpenSessionSwitcher,
        label: "Ctrl+Tab",
        description: "Switch session",
    },
    KeyBinding {
        key: "e",
        modifiers: KeyModifiers::new(true, true, false),
        scope: KeyScope::Global,
        action: KeyAction::OpenCommandPalette(PaletteTab::Sessions),
        label: "Ctrl+Shift+E",
        description: "Sessions",
    },
    KeyBinding {
        key: "p",
        modifiers: KeyModifiers::new(true, true, false),
        scope: KeyScope::Global,
        action: KeyAction::OpenCommandPalette(PaletteTab::Projects),
        label: "Ctrl+Shift+P",
        description: "Projects",
    },
    KeyBinding {
        key: "a",
        modifiers: KeyModifiers::new(true, true, false),
        scope: KeyScope::Global,
        action: KeyAction::OpenCommandPalette(PaletteTab::Actions),
        label: "Ctrl+Shift+A",
        description: "Actions",
    },
    KeyBinding {
        key: "f",
        modifiers: KeyModifiers::new(true, false, false),
        scope: KeyScope::Global,
        action: KeyAction::OpenSearch,
        label: "Ctrl+F",
        description: "Search in terminal",
    },
    KeyBinding {
        key: "f1",
        modifiers: KeyModifiers::new(false, false, false),
        scope: KeyScope::Global,
        action: KeyAction::OpenHelp,
        label: "F1",
        description: "Help",
    },
    KeyBinding {
        key: "i",
        modifiers: KeyModifiers::new(true, true, false),
        scope: KeyScope::Global,
        action: KeyAction::ToggleActivityPanel,
        label: "Ctrl+Shift+I",
        description: "Toggle activity panel",
    },
    KeyBinding {
        key: "o",
        modifiers: KeyModifiers::new(true, true, false),
        scope: KeyScope::Global,
        action: KeyAction::OpenSessionInNewWindow,
        label: "Ctrl+Shift+O",
        description: "Open session in new window",
    },
    KeyBinding {
        key: "n",
        modifiers: KeyModifiers::new(true, true, false),
        scope: KeyScope::Global,
        action: KeyAction::OpenNewWorktree,
        label: "Ctrl+Shift+N",
        description: "New worktree",
    },
    KeyBinding {
        key: "escape",
        modifiers: KeyModifiers::new(false, false, false),
        scope: KeyScope::Modal,
        action: KeyAction::CloseOverlay,
        label: "Escape",
        description: "Close overlay",
    },
];

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Match a keystroke against global-scope bindings.
/// Returns `Some(action)` if a binding matches, `None` otherwise.
pub fn dispatch_global_key(key: &str, control: bool, shift: bool, alt: bool) -> Option<KeyAction> {
    for binding in BINDINGS {
        if binding.scope == KeyScope::Global
            && binding.key == key
            && binding.modifiers.matches(control, shift, alt)
        {
            return Some(binding.action);
        }
    }
    None
}

/// Match a keystroke against modal-scope bindings (e.g. Esc to close).
pub fn dispatch_modal_key(key: &str, control: bool, shift: bool, alt: bool) -> Option<KeyAction> {
    for binding in BINDINGS {
        if binding.scope == KeyScope::Modal
            && binding.key == key
            && binding.modifiers.matches(control, shift, alt)
        {
            return Some(binding.action);
        }
    }
    None
}

/// Generate help modal shortcut entries from the binding registry.
/// Returns (label, description) pairs in display order.
/// Includes the "Shift Shift" double-shift entry which is not a standard binding.
pub fn help_shortcuts() -> Vec<(&'static str, &'static str)> {
    let mut shortcuts: Vec<(&str, &str)> =
        BINDINGS.iter().map(|b| (b.label, b.description)).collect();
    // Double-shift is detected via modifiers_changed, not key_down,
    // so it's not in the binding registry. Add it manually.
    shortcuts.insert(1, ("Shift Shift", "Command palette"));
    shortcuts
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn no_duplicate_bindings_in_same_scope() {
        let mut seen: HashSet<(KeyScope, &str, bool, bool, bool)> = HashSet::new();
        for binding in BINDINGS {
            let key = (
                binding.scope,
                binding.key,
                binding.modifiers.control,
                binding.modifiers.shift,
                binding.modifiers.alt,
            );
            assert!(
                seen.insert(key),
                "Duplicate binding: {:?} scope={:?} key={} ctrl={} shift={} alt={}",
                binding.label,
                binding.scope,
                binding.key,
                binding.modifiers.control,
                binding.modifiers.shift,
                binding.modifiers.alt,
            );
        }
    }

    #[test]
    fn dispatch_global_ctrl_k() {
        let action = dispatch_global_key("k", true, false, false);
        assert_eq!(action, Some(KeyAction::OpenCommandPalette(PaletteTab::All)));
    }

    #[test]
    fn dispatch_global_ctrl_tab() {
        let action = dispatch_global_key("tab", true, false, false);
        assert_eq!(action, Some(KeyAction::OpenSessionSwitcher));
    }

    #[test]
    fn dispatch_global_f1() {
        let action = dispatch_global_key("f1", false, false, false);
        assert_eq!(action, Some(KeyAction::OpenHelp));
    }

    #[test]
    fn dispatch_global_ctrl_shift_e() {
        let action = dispatch_global_key("e", true, true, false);
        assert_eq!(
            action,
            Some(KeyAction::OpenCommandPalette(PaletteTab::Sessions))
        );
    }

    #[test]
    fn dispatch_global_ctrl_shift_p() {
        let action = dispatch_global_key("p", true, true, false);
        assert_eq!(
            action,
            Some(KeyAction::OpenCommandPalette(PaletteTab::Projects))
        );
    }

    #[test]
    fn dispatch_global_ctrl_shift_a() {
        let action = dispatch_global_key("a", true, true, false);
        assert_eq!(
            action,
            Some(KeyAction::OpenCommandPalette(PaletteTab::Actions))
        );
    }

    #[test]
    fn dispatch_global_ctrl_f() {
        let action = dispatch_global_key("f", true, false, false);
        assert_eq!(action, Some(KeyAction::OpenSearch));
    }

    #[test]
    fn dispatch_modal_escape() {
        let action = dispatch_modal_key("escape", false, false, false);
        assert_eq!(action, Some(KeyAction::CloseOverlay));
    }

    #[test]
    fn dispatch_global_ctrl_i() {
        let action = dispatch_global_key("i", true, true, false);
        assert_eq!(action, Some(KeyAction::ToggleActivityPanel));
    }

    #[test]
    fn dispatch_global_ctrl_shift_o() {
        let action = dispatch_global_key("o", true, true, false);
        assert_eq!(action, Some(KeyAction::OpenSessionInNewWindow));
    }

    #[test]
    fn dispatch_unmatched_returns_none() {
        assert_eq!(dispatch_global_key("z", false, false, false), None);
        assert_eq!(dispatch_global_key("k", false, false, false), None);
    }

    #[test]
    fn help_shortcuts_includes_all_bindings_plus_double_shift() {
        let shortcuts = help_shortcuts();
        // All bindings + 1 for double-shift
        assert_eq!(shortcuts.len(), BINDINGS.len() + 1);
        // Double-shift is at index 1 (after Ctrl+K)
        assert_eq!(shortcuts[1], ("Shift Shift", "Command palette"));
    }

    #[test]
    fn help_shortcuts_order_matches_bindings() {
        let shortcuts = help_shortcuts();
        // First entry is the first binding (Ctrl+K)
        assert_eq!(shortcuts[0], ("Ctrl+K", "Command palette"));
        // Skip index 1 (double-shift), rest match bindings 1..
        for (i, binding) in BINDINGS.iter().enumerate().skip(1) {
            assert_eq!(shortcuts[i + 1], (binding.label, binding.description));
        }
    }

    // Scope must be Hash+Eq for the test above
    impl std::hash::Hash for KeyScope {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            core::mem::discriminant(self).hash(state);
        }
    }
}

//! Double-Shift detection for opening command palette.
//!
//! Tracks modifier state transitions via `ModifiersChangedEvent`. Two bare Shift
//! press-release cycles within 400ms trigger detection.
//!
//! GPUI does NOT fire `on_key_down` or `on_key_up` for bare modifier keys (both X11
//! and Wayland backends filter them out with `keysym.is_modifier_key()`). The only
//! event that fires is `ModifiersChangedEvent` via `on_modifiers_changed`.
//!
//! Detection logic:
//! 1. Shift pressed (shift goes true) -- record that shift is being held
//! 2. Shift released (shift goes false, no other modifiers held) -- this is a "bare shift tap"
//! 3. If two bare shift taps occur within 400ms, trigger the palette
//! 4. If any non-shift modifier was active during the press, it's not a bare tap
//! 5. If a regular key was pressed while shift was held (tracked via `on_key_down`),
//!    it's not a bare tap (prevents false triggers from Shift+A, Shift+Tab, etc.)

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

const DOUBLE_SHIFT_TIMEOUT_MS: u128 = 400;

/// Shared double-shift state usable from GPUI `Fn` closures via `Rc<Cell<>>`.
#[derive(Clone)]
pub struct DoubleShiftDetector {
    /// Whether shift was held down (saw shift=true in modifiers_changed).
    shift_held: Rc<Cell<bool>>,
    /// Whether any non-shift key was pressed while shift was held.
    had_key_during_shift: Rc<Cell<bool>>,
    /// Whether any other modifier (ctrl/alt/cmd) was active during the shift press.
    had_other_modifier: Rc<Cell<bool>>,
    /// Timestamp of the last valid bare shift tap (press+release with no other keys).
    last_bare_shift_tap: Rc<Cell<Option<Instant>>>,
}

impl DoubleShiftDetector {
    pub fn new() -> Self {
        Self {
            shift_held: Rc::new(Cell::new(false)),
            had_key_during_shift: Rc::new(Cell::new(false)),
            had_other_modifier: Rc::new(Cell::new(false)),
            last_bare_shift_tap: Rc::new(Cell::new(None)),
        }
    }

    /// Call from `on_modifiers_changed` handler. Returns `true` if double-shift detected.
    ///
    /// The `ModifiersChangedEvent` fires whenever any modifier key state changes.
    /// We detect shift press (shift becomes true) and shift release (shift becomes false).
    pub fn on_modifiers_changed(&self, shift: bool, control: bool, alt: bool, platform: bool) -> bool {
        let other_modifier_active = control || alt || platform;

        if shift && !self.shift_held.get() {
            // Shift just pressed
            self.shift_held.set(true);
            self.had_key_during_shift.set(false);
            self.had_other_modifier.set(other_modifier_active);
            return false;
        }

        if !shift && self.shift_held.get() {
            // Shift just released
            self.shift_held.set(false);

            // Only count as a bare shift tap if no other keys/modifiers were involved
            if self.had_key_during_shift.get() || self.had_other_modifier.get() || other_modifier_active {
                self.had_key_during_shift.set(false);
                self.had_other_modifier.set(false);
                return false;
            }

            // This was a clean bare shift tap
            if let Some(last) = self.last_bare_shift_tap.get() {
                if last.elapsed().as_millis() < DOUBLE_SHIFT_TIMEOUT_MS {
                    self.last_bare_shift_tap.set(None);
                    return true;
                }
            }

            self.last_bare_shift_tap.set(Some(Instant::now()));
        }

        // Other modifier changed (ctrl/alt/cmd) while shift is held
        if self.shift_held.get() && other_modifier_active {
            self.had_other_modifier.set(true);
        }

        false
    }

    /// Call from `on_key_down` handler to track non-modifier key presses during shift hold.
    /// This prevents false triggers from Shift+letter combos (typing capitals, shortcuts).
    ///
    /// Note: GPUI `on_key_down` only fires for non-modifier keys, so we don't need
    /// to filter out modifier key names here.
    pub fn on_key_down_during_shift(&self) {
        if self.shift_held.get() {
            self.had_key_during_shift.set(true);
        }
    }

    pub fn reset(&self) {
        self.shift_held.set(false);
        self.had_key_during_shift.set(false);
        self.had_other_modifier.set(false);
        self.last_bare_shift_tap.set(None);
    }
}

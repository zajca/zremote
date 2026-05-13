use alacritty_terminal::term::TermMode;

const CSI: &[u8] = b"\x1b[";
const ALT_SCROLL_UP: &[u8] = b"\x1b[A";
const ALT_SCROLL_DOWN: &[u8] = b"\x1b[B";
const LEGACY_MOUSE_OFFSET: usize = 32;
const LEGACY_MOUSE_MAX_VALUE: usize = u8::MAX as usize - LEGACY_MOUSE_OFFSET;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButtonKind {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    Drag,
    Move,
    Wheel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseGridPosition {
    pub col: usize,
    pub row: usize,
}

pub fn terminal_mouse_active(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
}

pub fn terminal_mouse_captures_local_pointer(mode: TermMode, modifiers: MouseModifiers) -> bool {
    terminal_mouse_active(mode) && !modifiers.shift
}

pub fn local_url_action_enabled(mode: TermMode, modifiers: MouseModifiers) -> bool {
    modifiers.control && (modifiers.shift || !terminal_mouse_active(mode))
}

pub fn encode_mouse_event(
    mode: TermMode,
    event: MouseEventKind,
    button: MouseButtonKind,
    pos: MouseGridPosition,
    modifiers: MouseModifiers,
) -> Option<Vec<u8>> {
    if !event_enabled(mode, event) {
        return None;
    }

    if mode.contains(TermMode::SGR_MOUSE) {
        let button_code = sgr_mouse_button_code(event, button)? + modifier_bits(modifiers);
        Some(encode_sgr_mouse_event(event, button_code, pos))
    } else {
        let button_code = legacy_mouse_button_code(event, button)? + modifier_bits(modifiers);
        encode_legacy_mouse_event(button_code, pos)
    }
}

pub fn encode_alt_scroll(mode: TermMode, lines: i32) -> Option<Vec<u8>> {
    if lines == 0 || terminal_mouse_active(mode) || !mode.contains(TermMode::ALT_SCREEN) {
        return None;
    }

    let sequence = if lines > 0 {
        ALT_SCROLL_UP
    } else {
        ALT_SCROLL_DOWN
    };
    let repeat = lines.unsigned_abs() as usize;
    let mut bytes = Vec::with_capacity(sequence.len() * repeat);

    for _ in 0..repeat {
        bytes.extend_from_slice(sequence);
    }

    Some(bytes)
}

fn event_enabled(mode: TermMode, event: MouseEventKind) -> bool {
    if !terminal_mouse_active(mode) {
        return false;
    }

    match event {
        MouseEventKind::Press | MouseEventKind::Release | MouseEventKind::Wheel => true,
        MouseEventKind::Drag => mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
        MouseEventKind::Move => mode.contains(TermMode::MOUSE_MOTION),
    }
}

fn sgr_mouse_button_code(event: MouseEventKind, button: MouseButtonKind) -> Option<usize> {
    match event {
        MouseEventKind::Press => match button {
            MouseButtonKind::Left => Some(0),
            MouseButtonKind::Middle => Some(1),
            MouseButtonKind::Right => Some(2),
            MouseButtonKind::WheelUp | MouseButtonKind::WheelDown => None,
        },
        MouseEventKind::Release => match button {
            MouseButtonKind::Left | MouseButtonKind::Middle | MouseButtonKind::Right => Some(0),
            MouseButtonKind::WheelUp | MouseButtonKind::WheelDown => None,
        },
        MouseEventKind::Drag => match button {
            MouseButtonKind::Left => Some(32),
            MouseButtonKind::Middle => Some(33),
            MouseButtonKind::Right => Some(34),
            MouseButtonKind::WheelUp | MouseButtonKind::WheelDown => None,
        },
        MouseEventKind::Move => Some(35),
        MouseEventKind::Wheel => match button {
            MouseButtonKind::WheelUp => Some(64),
            MouseButtonKind::WheelDown => Some(65),
            MouseButtonKind::Left | MouseButtonKind::Middle | MouseButtonKind::Right => None,
        },
    }
}

fn legacy_mouse_button_code(event: MouseEventKind, button: MouseButtonKind) -> Option<usize> {
    match event {
        MouseEventKind::Release => match button {
            MouseButtonKind::Left | MouseButtonKind::Middle | MouseButtonKind::Right => Some(3),
            MouseButtonKind::WheelUp | MouseButtonKind::WheelDown => None,
        },
        _ => sgr_mouse_button_code(event, button),
    }
}

fn modifier_bits(modifiers: MouseModifiers) -> usize {
    let mut bits = 0;

    if modifiers.shift {
        bits |= 4;
    }
    if modifiers.alt {
        bits |= 8;
    }
    if modifiers.control {
        bits |= 16;
    }

    bits
}

fn encode_sgr_mouse_event(
    event: MouseEventKind,
    button_code: usize,
    pos: MouseGridPosition,
) -> Vec<u8> {
    let suffix = if event == MouseEventKind::Release {
        'm'
    } else {
        'M'
    };
    format!(
        "\x1b[<{};{};{}{}",
        button_code,
        pos.col + 1,
        pos.row + 1,
        suffix
    )
    .into_bytes()
}

fn encode_legacy_mouse_event(button_code: usize, pos: MouseGridPosition) -> Option<Vec<u8>> {
    let x = pos.col.checked_add(1)?;
    let y = pos.row.checked_add(1)?;

    Some(vec![
        CSI[0],
        CSI[1],
        b'M',
        legacy_mouse_byte(button_code)?,
        legacy_mouse_byte(x)?,
        legacy_mouse_byte(y)?,
    ])
}

fn legacy_mouse_byte(value: usize) -> Option<u8> {
    if value > LEGACY_MOUSE_MAX_VALUE {
        return None;
    }

    u8::try_from(value + LEGACY_MOUSE_OFFSET).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(col: usize, row: usize) -> MouseGridPosition {
        MouseGridPosition { col, row }
    }

    fn sgr_click_mode() -> TermMode {
        TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE
    }

    #[test]
    fn terminal_mouse_active_false_without_mouse_modes() {
        assert!(!terminal_mouse_active(TermMode::NONE));
        assert!(!terminal_mouse_active(TermMode::SGR_MOUSE));
    }

    #[test]
    fn terminal_mouse_active_true_for_click_mode() {
        assert!(terminal_mouse_active(TermMode::MOUSE_REPORT_CLICK));
    }

    #[test]
    fn mouse_capture_requires_mouse_mode_and_no_shift_override() {
        assert!(!terminal_mouse_captures_local_pointer(
            TermMode::NONE,
            MouseModifiers::default()
        ));
        assert!(terminal_mouse_captures_local_pointer(
            TermMode::MOUSE_REPORT_CLICK,
            MouseModifiers::default()
        ));
        assert!(!terminal_mouse_captures_local_pointer(
            TermMode::MOUSE_REPORT_CLICK,
            MouseModifiers {
                shift: true,
                ..MouseModifiers::default()
            }
        ));
    }

    #[test]
    fn local_url_action_requires_ctrl_or_ctrl_shift_in_mouse_mode() {
        assert!(!local_url_action_enabled(
            TermMode::NONE,
            MouseModifiers::default()
        ));
        assert!(local_url_action_enabled(
            TermMode::NONE,
            MouseModifiers {
                control: true,
                ..MouseModifiers::default()
            }
        ));
        assert!(!local_url_action_enabled(
            TermMode::MOUSE_REPORT_CLICK,
            MouseModifiers {
                control: true,
                ..MouseModifiers::default()
            }
        ));
        assert!(local_url_action_enabled(
            TermMode::MOUSE_REPORT_CLICK,
            MouseModifiers {
                shift: true,
                control: true,
                ..MouseModifiers::default()
            }
        ));
    }

    #[test]
    fn sgr_left_press_uses_one_based_coordinates() {
        let bytes = encode_mouse_event(
            sgr_click_mode(),
            MouseEventKind::Press,
            MouseButtonKind::Left,
            pos(9, 4),
            MouseModifiers::default(),
        );

        assert_eq!(bytes, Some(b"\x1b[<0;10;5M".to_vec()));
    }

    #[test]
    fn sgr_middle_and_right_press_codes() {
        let middle = encode_mouse_event(
            sgr_click_mode(),
            MouseEventKind::Press,
            MouseButtonKind::Middle,
            pos(9, 4),
            MouseModifiers::default(),
        );
        let right = encode_mouse_event(
            sgr_click_mode(),
            MouseEventKind::Press,
            MouseButtonKind::Right,
            pos(9, 4),
            MouseModifiers::default(),
        );

        assert_eq!(middle, Some(b"\x1b[<1;10;5M".to_vec()));
        assert_eq!(right, Some(b"\x1b[<2;10;5M".to_vec()));
    }

    #[test]
    fn sgr_left_release_uses_lowercase_m() {
        let bytes = encode_mouse_event(
            sgr_click_mode(),
            MouseEventKind::Release,
            MouseButtonKind::Left,
            pos(9, 4),
            MouseModifiers::default(),
        );

        assert_eq!(bytes, Some(b"\x1b[<0;10;5m".to_vec()));
    }

    #[test]
    fn sgr_drag_sets_drag_button_code() {
        let bytes = encode_mouse_event(
            TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE,
            MouseEventKind::Drag,
            MouseButtonKind::Left,
            pos(10, 4),
            MouseModifiers::default(),
        );

        assert_eq!(bytes, Some(b"\x1b[<32;11;5M".to_vec()));
    }

    #[test]
    fn sgr_wheel_up_down_codes() {
        let up = encode_mouse_event(
            sgr_click_mode(),
            MouseEventKind::Wheel,
            MouseButtonKind::WheelUp,
            pos(9, 4),
            MouseModifiers::default(),
        );
        let down = encode_mouse_event(
            sgr_click_mode(),
            MouseEventKind::Wheel,
            MouseButtonKind::WheelDown,
            pos(9, 4),
            MouseModifiers::default(),
        );

        assert_eq!(up, Some(b"\x1b[<64;10;5M".to_vec()));
        assert_eq!(down, Some(b"\x1b[<65;10;5M".to_vec()));
    }

    #[test]
    fn sgr_modifiers_are_encoded() {
        let bytes = encode_mouse_event(
            sgr_click_mode(),
            MouseEventKind::Press,
            MouseButtonKind::Left,
            pos(9, 4),
            MouseModifiers {
                shift: true,
                alt: true,
                control: true,
            },
        );

        assert_eq!(bytes, Some(b"\x1b[<28;10;5M".to_vec()));
    }

    #[test]
    fn legacy_x10_left_press() {
        let bytes = encode_mouse_event(
            TermMode::MOUSE_REPORT_CLICK,
            MouseEventKind::Press,
            MouseButtonKind::Left,
            pos(9, 4),
            MouseModifiers::default(),
        );

        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 32, 42, 37]));
    }

    #[test]
    fn legacy_x10_rejects_large_coordinates() {
        let bytes = encode_mouse_event(
            TermMode::MOUSE_REPORT_CLICK,
            MouseEventKind::Press,
            MouseButtonKind::Left,
            pos(223, 4),
            MouseModifiers::default(),
        );

        assert_eq!(bytes, None);
    }

    #[test]
    fn click_mode_does_not_encode_drag() {
        let bytes = encode_mouse_event(
            sgr_click_mode(),
            MouseEventKind::Drag,
            MouseButtonKind::Left,
            pos(9, 4),
            MouseModifiers::default(),
        );

        assert_eq!(bytes, None);
    }

    #[test]
    fn drag_mode_encodes_drag() {
        let bytes = encode_mouse_event(
            TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE,
            MouseEventKind::Drag,
            MouseButtonKind::Left,
            pos(9, 4),
            MouseModifiers::default(),
        );

        assert_eq!(bytes, Some(b"\x1b[<32;10;5M".to_vec()));
    }

    #[test]
    fn motion_mode_encodes_passive_move() {
        let bytes = encode_mouse_event(
            TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE,
            MouseEventKind::Move,
            MouseButtonKind::Left,
            pos(9, 4),
            MouseModifiers::default(),
        );

        assert_eq!(bytes, Some(b"\x1b[<35;10;5M".to_vec()));
    }

    #[test]
    fn encode_alt_scroll_requires_alt_screen_and_no_mouse_mode() {
        assert_eq!(encode_alt_scroll(TermMode::NONE, 1), None);
        assert_eq!(encode_alt_scroll(TermMode::ALT_SCREEN, 0), None);
        assert_eq!(
            encode_alt_scroll(TermMode::ALT_SCREEN | TermMode::MOUSE_REPORT_CLICK, 1,),
            None
        );
    }

    #[test]
    fn encode_alt_scroll_repeats_arrow_keys_by_line_count() {
        let mode = TermMode::ALT_SCREEN;

        assert_eq!(encode_alt_scroll(mode, 2), Some(b"\x1b[A\x1b[A".to_vec()));
        assert_eq!(
            encode_alt_scroll(mode, -3),
            Some(b"\x1b[B\x1b[B\x1b[B".to_vec())
        );
    }
}

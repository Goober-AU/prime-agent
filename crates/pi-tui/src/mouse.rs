//! Port of packages/tui/src/mouse.ts.

use once_cell::sync::Lazy;
use regex::Regex;

/// Base SGR button code with modifier and motion bits removed; wheel up/down are 64/65.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub button: i64,
    /// One-based terminal column.
    pub x: i64,
    /// One-based terminal row.
    pub y: i64,
    /// True for SGR `M` reports (press, wheel, or drag), false for release `m`.
    pub press: bool,
    /// Whether the SGR motion bit is set.
    pub motion: bool,
    /// Modifier bits carried by the SGR report.
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

pub const MOUSE_WHEEL_UP: i64 = 64;
pub const MOUSE_WHEEL_DOWN: i64 = 65;
pub const MOUSE_BUTTON_LEFT: i64 = 0;

static SGR_MOUSE_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\x1b\[<(\d+);(\d+);(\d+)([Mm])$").unwrap());

const MODIFIER_SHIFT: i64 = 4;
const MODIFIER_ALT: i64 = 8;
const MODIFIER_CTRL: i64 = 16;
const MOTION_BIT: i64 = 32;

pub fn is_mouse_sequence(sequence: &str) -> bool {
    sequence.starts_with("\x1b[<") || sequence.starts_with("\x1b[M")
}

pub fn parse_sgr_mouse_event(sequence: &str) -> Option<MouseEvent> {
    let caps = SGR_MOUSE_PATTERN.captures(sequence)?;
    let raw: i64 = caps.get(1)?.as_str().parse().ok()?;
    let x: i64 = caps.get(2)?.as_str().parse().ok()?;
    let y: i64 = caps.get(3)?.as_str().parse().ok()?;
    let press = caps.get(4)?.as_str() == "M";
    Some(MouseEvent {
        button: raw & !(MODIFIER_SHIFT | MODIFIER_ALT | MODIFIER_CTRL | MOTION_BIT),
        x,
        y,
        press,
        motion: (raw & MOTION_BIT) != 0,
        shift: (raw & MODIFIER_SHIFT) != 0,
        alt: (raw & MODIFIER_ALT) != 0,
        ctrl: (raw & MODIFIER_CTRL) != 0,
    })
}

pub fn is_wheel_up(event: &MouseEvent) -> bool {
    event.press && event.button == MOUSE_WHEEL_UP
}

pub fn is_wheel_down(event: &MouseEvent) -> bool {
    event.press && event.button == MOUSE_WHEEL_DOWN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sgr_press_with_modifiers() {
        let e = parse_sgr_mouse_event("\x1b[<16;5;3M").unwrap();
        assert_eq!(e.button, 0);
        assert_eq!(e.x, 5);
        assert_eq!(e.y, 3);
        assert!(e.press);
        assert!(e.ctrl);
        assert!(!e.shift);
        assert!(!e.motion);
    }

    #[test]
    fn parses_wheel_and_release() {
        let up = parse_sgr_mouse_event("\x1b[<64;1;1M").unwrap();
        assert!(is_wheel_up(&up));
        let down = parse_sgr_mouse_event("\x1b[<65;1;1M").unwrap();
        assert!(is_wheel_down(&down));
        let release = parse_sgr_mouse_event("\x1b[<0;1;1m").unwrap();
        assert!(!release.press);
    }

    #[test]
    fn rejects_non_mouse_sequences() {
        assert!(!is_mouse_sequence("\x1b[A"));
        assert!(is_mouse_sequence("\x1b[<0;1;1M"));
        assert!(parse_sgr_mouse_event("\x1b[A").is_none());
    }
}

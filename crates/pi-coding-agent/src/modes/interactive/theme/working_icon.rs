//! Port of packages/coding-agent/src/modes/interactive/theme/working-icon.ts

/// `WORKING_ICON_FRAMES`
pub const WORKING_ICON_FRAMES: [&str; 4] = ["\u{25c7}", "\u{25c8}", "\u{25c6}", "\u{25c8}"];
/// `WORKING_ICON_INTERVAL_MS`
pub const WORKING_ICON_INTERVAL_MS: u64 = 250;

/// Port of `workingIconFrame`.
pub fn working_icon_frame(frame: i64) -> &'static str {
    let frames = WORKING_ICON_FRAMES;
    let len = frames.len() as i64;
    let index = ((frame % len) + len) % len;
    frames[index as usize]
}

// Process-wide frame counter for in-place chat tool markers, advanced by the
// interactive mode's single ticker and read during render.
static PULSE_FRAME: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Port of `getWorkingPulseFrame`.
pub fn get_working_pulse_frame() -> i64 {
    PULSE_FRAME.load(std::sync::atomic::Ordering::SeqCst)
}

/// Port of `setWorkingPulseFrame`.
pub fn set_working_pulse_frame(frame: i64) {
    PULSE_FRAME.store(frame, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_wrap_in_both_directions() {
        assert_eq!(working_icon_frame(0), "\u{25c7}");
        assert_eq!(working_icon_frame(1), "\u{25c8}");
        assert_eq!(working_icon_frame(2), "\u{25c6}");
        assert_eq!(working_icon_frame(3), "\u{25c8}");
        assert_eq!(working_icon_frame(4), "\u{25c7}");
        assert_eq!(working_icon_frame(-1), "\u{25c8}");
    }

    #[test]
    fn pulse_frame_is_process_wide() {
        set_working_pulse_frame(7);
        assert_eq!(get_working_pulse_frame(), 7);
        set_working_pulse_frame(0);
        assert_eq!(get_working_pulse_frame(), 0);
    }
}

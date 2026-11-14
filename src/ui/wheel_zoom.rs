//! Ctrl+wheel zoom shared by the editor and the Markdown preview.
use lgui::core::{WheelDelta, WheelUnit};
use std::cell::Cell;

/// Pixels of smooth scrolling that add up to one zoom step.
const PIXELS_PER_STEP: f32 = 40.0;

thread_local! {
    static PENDING: Cell<f32> = const { Cell::new(0.0) };
}

/// Whether the wheel event is a zoom gesture: Ctrl, or Cmd on macOS.
pub(crate) fn is_zoom(delta: WheelDelta) -> bool {
    delta.modifiers.ctrl() || delta.modifiers.meta()
}

/// Zoom steps for one wheel event (positive zooms in): a notch is one step, and
/// smooth pixel deltas accumulate until they add up to a notch.
pub(crate) fn steps(delta: WheelDelta) -> i32 {
    match delta.unit {
        WheelUnit::Lines => delta.y.signum() as i32 * (delta.y != 0.0) as i32,
        WheelUnit::Pixels => PENDING.with(|pending| {
            let total = pending.get() + delta.y;
            let whole = (total / PIXELS_PER_STEP).trunc();
            pending.set(total - whole * PIXELS_PER_STEP);
            whole as i32
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_follow_notches_and_accumulate_smooth_scrolling() {
        assert_eq!(steps(WheelDelta::lines(0.0, 1.0)), 1);
        assert_eq!(steps(WheelDelta::lines(0.0, -2.0)), -1);
        assert_eq!(steps(WheelDelta::lines(0.0, 0.0)), 0);
        let total: i32 = (0..10).map(|_| steps(WheelDelta::pixels(0.0, 12.0))).sum();
        assert_eq!(total, 3, "120px of smooth scrolling is three steps");
    }
}

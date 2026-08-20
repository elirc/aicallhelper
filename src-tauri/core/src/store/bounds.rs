//! Window geometry sanitisation (§8).
//!
//! Restoring saved geometry is a small feature with a big failure mode: a
//! window restored onto a monitor that no longer exists is invisible, and the
//! user has no way to get it back short of editing JSON. So the position is
//! only honoured when a meaningful slice of the window provably lands on a
//! display that exists *right now*.
//!
//! Pure and display-list-injected so every geometry case is unit-testable
//! without a second monitor.

use serde::{Deserialize, Serialize};

pub const MIN_WIDTH: u32 = 380;
pub const MIN_HEIGHT: u32 = 520;
pub const DEFAULT_WIDTH: u32 = 460;
pub const DEFAULT_HEIGHT: u32 = 700;

/// How much of the window must be on-screen, on each axis, for the saved
/// position to be reused. A title bar is ~32 px tall, so 40 px guarantees the
/// user can always grab the window and drag it somewhere better.
pub const MIN_VISIBLE_PX: i64 = 40;

/// Geometry as read from the settings file. Floats because JSON numbers are,
/// and because a hand-edited file can contain anything.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RawBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A monitor's usable area (excludes the taskbar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkArea {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The geometry to actually apply. `position: None` means "let the OS centre
/// it" — the honest outcome when the saved spot is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SanitizedBounds {
    pub width: u32,
    pub height: u32,
    pub position: Option<(i32, i32)>,
}

impl SanitizedBounds {
    /// First run, or geometry too corrupt to trust.
    pub fn fallback() -> Self {
        Self { width: DEFAULT_WIDTH, height: DEFAULT_HEIGHT, position: None }
    }
}

fn finite_round(v: f64) -> Option<i64> {
    if !v.is_finite() {
        return None;
    }
    let r = v.round();
    // Anything beyond i32 range is nonsense geometry, not a real monitor.
    if r > i32::MAX as f64 || r < i32::MIN as f64 {
        return None;
    }
    Some(r as i64)
}

/// Overlap of two 1-D intervals, in pixels. Negative means disjoint.
fn overlap(a_start: i64, a_len: i64, b_start: i64, b_len: i64) -> i64 {
    (a_start + a_len).min(b_start + b_len) - a_start.max(b_start)
}

/// Sanitise saved geometry against the displays that exist now.
///
/// Size is clamped up to the minimum *first*, and visibility is judged at that
/// clamped size — a saved 10x10 window would otherwise be ruled off-screen on
/// its tiny footprint and then restored at full size somewhere else.
pub fn sanitize_bounds(raw: Option<RawBounds>, displays: &[WorkArea]) -> SanitizedBounds {
    let Some(raw) = raw else {
        return SanitizedBounds::fallback();
    };

    // Corrupt values drop the geometry *as a unit*. Mixing a good width with a
    // defaulted height produces a shape the user never chose, which reads as a
    // bug rather than as a reset.
    let (Some(x), Some(y), Some(w), Some(h)) = (
        finite_round(raw.x),
        finite_round(raw.y),
        finite_round(raw.width),
        finite_round(raw.height),
    ) else {
        return SanitizedBounds::fallback();
    };

    if w <= 0 || h <= 0 {
        return SanitizedBounds::fallback();
    }

    let width = (w.max(MIN_WIDTH as i64)) as u32;
    let height = (h.max(MIN_HEIGHT as i64)) as u32;

    // With no display list we cannot prove anything about visibility; centring
    // is the safe answer.
    let visible = displays.iter().any(|d| {
        overlap(x, width as i64, d.x as i64, d.width as i64) >= MIN_VISIBLE_PX
            && overlap(y, height as i64, d.y as i64, d.height as i64) >= MIN_VISIBLE_PX
    });

    SanitizedBounds {
        width,
        height,
        position: if visible { Some((x as i32, y as i32)) } else { None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary() -> WorkArea {
        WorkArea { x: 0, y: 0, width: 1920, height: 1040 }
    }

    fn raw(x: f64, y: f64, width: f64, height: f64) -> Option<RawBounds> {
        Some(RawBounds { x, y, width, height })
    }

    #[test]
    fn no_saved_bounds_falls_back_to_defaults_centred() {
        assert_eq!(sanitize_bounds(None, &[primary()]), SanitizedBounds::fallback());
        assert_eq!(SanitizedBounds::fallback().position, None);
        assert_eq!(SanitizedBounds::fallback().width, DEFAULT_WIDTH);
        assert_eq!(SanitizedBounds::fallback().height, DEFAULT_HEIGHT);
    }

    #[test]
    fn a_normal_on_screen_window_is_restored_exactly() {
        let got = sanitize_bounds(raw(100.0, 120.0, 500.0, 800.0), &[primary()]);
        assert_eq!(got, SanitizedBounds { width: 500, height: 800, position: Some((100, 120)) });
    }

    #[test]
    fn size_is_clamped_up_to_the_window_minimum() {
        let got = sanitize_bounds(raw(10.0, 10.0, 100.0, 100.0), &[primary()]);
        assert_eq!(got.width, MIN_WIDTH);
        assert_eq!(got.height, MIN_HEIGHT);
    }

    #[test]
    fn fractional_values_are_rounded_to_integers() {
        let got = sanitize_bounds(raw(100.4, 119.6, 500.5, 800.4), &[primary()]);
        assert_eq!(got.position, Some((100, 120)));
        assert_eq!(got.width, 501);
        assert_eq!(got.height, 800);
    }

    #[test]
    fn negative_coordinates_are_valid_for_a_monitor_left_of_primary() {
        // The classic false positive: treating x < 0 as corrupt strands anyone
        // whose second monitor is arranged to the left.
        let left = WorkArea { x: -1920, y: 0, width: 1920, height: 1040 };
        let got = sanitize_bounds(raw(-1800.0, 50.0, 460.0, 700.0), &[left, primary()]);
        assert_eq!(got.position, Some((-1800, 50)));
    }

    #[test]
    fn a_window_on_a_monitor_that_no_longer_exists_loses_its_position() {
        // The unplug-monitor scenario from the QA script.
        let got = sanitize_bounds(raw(2600.0, 300.0, 460.0, 700.0), &[primary()]);
        assert_eq!(got.position, None);
        // Size is still honoured — only the position was unprovable.
        assert_eq!((got.width, got.height), (460, 700));
    }

    #[test]
    fn visibility_needs_forty_pixels_on_both_axes() {
        let d = primary();

        // Exactly 40 px of the window's right edge overlaps the display's left
        // edge: kept.
        let keep = sanitize_bounds(raw(-420.0, 100.0, 460.0, 700.0), &[d]);
        assert_eq!(keep.position, Some((-420, 100)));

        // One pixel less: dropped.
        let drop = sanitize_bounds(raw(-421.0, 100.0, 460.0, 700.0), &[d]);
        assert_eq!(drop.position, None);

        // Same test on the vertical axis.
        let keep_y = sanitize_bounds(raw(100.0, -660.0, 460.0, 700.0), &[d]);
        assert_eq!(keep_y.position, Some((100, -660)));
        let drop_y = sanitize_bounds(raw(100.0, -661.0, 460.0, 700.0), &[d]);
        assert_eq!(drop_y.position, None);
    }

    #[test]
    fn both_axes_must_pass_on_the_same_display() {
        // Horizontally over the primary, vertically over a display stacked
        // below it, but overlapping neither: an L-shaped arrangement must not
        // let two different monitors each satisfy one axis.
        let below = WorkArea { x: 3000, y: 1040, width: 1920, height: 1040 };
        let got = sanitize_bounds(raw(100.0, 1200.0, 460.0, 700.0), &[primary(), below]);
        assert_eq!(got.position, None);
    }

    #[test]
    fn visibility_is_judged_at_the_clamped_size() {
        // A saved 10x10 at x=-30 has no on-screen pixels at its stored size,
        // but at the clamped 380x520 it overlaps by 350 px and is perfectly
        // usable. Judging before clamping needlessly recentres it.
        let got = sanitize_bounds(raw(-30.0, 10.0, 10.0, 10.0), &[primary()]);
        assert_eq!((got.width, got.height), (MIN_WIDTH, MIN_HEIGHT));
        assert_eq!(got.position, Some((-30, 10)));
    }

    #[test]
    fn corrupt_numbers_drop_the_whole_geometry_not_one_field() {
        // A defaulted height welded onto a saved width is a shape the user
        // never chose.
        for bad in [
            raw(f64::NAN, 10.0, 460.0, 700.0),
            raw(10.0, f64::INFINITY, 460.0, 700.0),
            raw(10.0, 10.0, f64::NEG_INFINITY, 700.0),
            raw(10.0, 10.0, 460.0, f64::NAN),
            raw(1e300, 10.0, 460.0, 700.0),
            raw(10.0, 10.0, 1e300, 700.0),
        ] {
            assert_eq!(sanitize_bounds(bad, &[primary()]), SanitizedBounds::fallback(), "bad: {bad:?}");
        }
    }

    #[test]
    fn zero_or_negative_size_falls_back() {
        assert_eq!(sanitize_bounds(raw(10.0, 10.0, 0.0, 700.0), &[primary()]), SanitizedBounds::fallback());
        assert_eq!(sanitize_bounds(raw(10.0, 10.0, 460.0, -5.0), &[primary()]), SanitizedBounds::fallback());
    }

    #[test]
    fn an_empty_display_list_never_trusts_a_position() {
        // Enumeration failed; we cannot prove the window would be visible, and
        // guessing wrong makes the app unreachable.
        let got = sanitize_bounds(raw(100.0, 100.0, 460.0, 700.0), &[]);
        assert_eq!(got.position, None);
        assert_eq!((got.width, got.height), (460, 700));
    }

    #[test]
    fn a_window_spanning_two_monitors_is_kept() {
        let right = WorkArea { x: 1920, y: 0, width: 1920, height: 1040 };
        let got = sanitize_bounds(raw(1700.0, 100.0, 460.0, 700.0), &[primary(), right]);
        assert_eq!(got.position, Some((1700, 100)));
    }

    #[test]
    fn a_taskbar_only_overlap_is_not_visible() {
        // Work area excludes the taskbar, so a window sitting entirely over the
        // taskbar strip has no usable overlap.
        let d = WorkArea { x: 0, y: 0, width: 1920, height: 1040 };
        let got = sanitize_bounds(raw(100.0, 1030.0, 460.0, 700.0), &[d]);
        assert_eq!(got.position, None);
    }
}

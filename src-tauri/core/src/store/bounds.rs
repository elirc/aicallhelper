//! Window geometry sanitisation and dock placement (§8, §9).
//!
//! Restoring saved geometry is a small feature with a big failure mode: a
//! window restored onto a monitor that no longer exists is invisible, and the
//! user has no way to get it back short of editing JSON. So the position is
//! only honoured when a meaningful slice of the window provably lands on a
//! display that exists *right now*.
//!
//! Pure and display-list-injected so every geometry case is unit-testable
//! without a second monitor. The "dock to camera" math at the bottom follows
//! the same rule: the shell measures, this module decides.

use serde::{Deserialize, Serialize};

/// The window's minimum inner size in LOGICAL px at 100 % scale — the same
/// numbers the shell hands the window builder's `min_inner_size`, which tao
/// scales by DPI. The sanitizer applies them to PHYSICAL saved bounds
/// unscaled, on purpose (DOCK-4, option A): at 150 % the OS floor is 570x780
/// and clamps again after us, so the only effect is that a corrupt tiny save
/// is judged visible at 380x520 rather than 570x780 — harmless, and it keeps
/// `sanitize_bounds` free of a scale parameter it would otherwise have to
/// trust from the same untrusted file. The dock preset below DOES scale (it
/// is a design size); do not copy the unscaled clamp into new code.
pub const MIN_WIDTH: u32 = 380;
pub const MIN_HEIGHT: u32 = 520;
/// First-run size, LOGICAL px: the window builder applies these, and the
/// shell must not re-apply them as physical pixels (see `from_saved`).
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

/// The geometry to actually apply. `position: None` means "the saved spot is
/// gone (or there never was one) — let the shell place the window" (it docks
/// to the camera, and centres only if docking itself fails).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SanitizedBounds {
    pub width: u32,
    pub height: u32,
    pub position: Option<(i32, i32)>,
    /// True when `width`/`height` came from the settings file — PHYSICAL px,
    /// as the shell saved them, safe to `set_size` verbatim. False for the
    /// first-run fallback, whose DEFAULT_WIDTH x DEFAULT_HEIGHT are LOGICAL:
    /// the window builder has already applied them, and re-applying them as
    /// physical pixels shrinks the window by the DPI factor on every hi-DPI
    /// display (RS-7). The shell skips `set_size` when this is false.
    pub from_saved: bool,
}

impl SanitizedBounds {
    /// First run, or geometry too corrupt to trust.
    pub fn fallback() -> Self {
        Self { width: DEFAULT_WIDTH, height: DEFAULT_HEIGHT, position: None, from_saved: false }
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

    // With no display list we cannot prove anything about visibility; letting
    // the shell place the window is the safe answer.
    let visible = displays.iter().any(|d| {
        overlap(x, width as i64, d.x as i64, d.width as i64) >= MIN_VISIBLE_PX
            && overlap(y, height as i64, d.y as i64, d.height as i64) >= MIN_VISIBLE_PX
    });

    SanitizedBounds {
        width,
        height,
        position: if visible { Some((x as i32, y as i32)) } else { None },
        from_saved: true,
    }
}

// ---------------------------------------------------------------------------
// "Dock to camera": top-centre of the current display (§9)
// ---------------------------------------------------------------------------

/// Gap between the top of the work area and the window's outer top edge,
/// physical px. Small on purpose: every pixel here is a pixel further from
/// the webcam.
pub const DOCK_TOP_MARGIN: u32 = 8;
/// Docked preset width in LOGICAL px; the caller scales it by the monitor's
/// factor. ~75 characters of 16 px text per line — the top of the
/// comfortable reading measure.
pub const DOCK_WIDTH_LOGICAL: u32 = 600;
pub const DOCK_MAX_HEIGHT_LOGICAL: u32 = 720;
/// Share of the work-area height the docked window takes before clamping.
pub const DOCK_HEIGHT_FRACTION: f64 = 0.45;

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Outer top-left for a window of OUTER width `outer_width` docked to the
/// top-centre of `work`. Physical pixels, like everything else here. Integer
/// division floors the odd pixel of slack. A window wider than the display
/// hugs the display's LEFT edge rather than hanging off both sides — the
/// title bar must stay grabbable. Height is not an input: a top-anchored dock
/// never moves on it, and an unused parameter would be a lie in the signature.
pub fn dock_top_center(work: WorkArea, outer_width: u32) -> (i32, i32) {
    // i64 throughout: a work area at x = -1920 minus a wide window must not
    // wrap, and the result is clamped like `finite_round` does.
    let slack = i64::from(work.width) - i64::from(outer_width);
    let x = i64::from(work.x) + slack.max(0) / 2;
    let y = i64::from(work.y) + i64::from(DOCK_TOP_MARGIN);
    (clamp_i32(x), clamp_i32(y))
}

/// The docked preset INNER size (physical px) for a display with `work` area
/// and DPI `scale`: the logical width preset scaled and capped at the display;
/// 45 % of the work-area height clamped to [MIN_HEIGHT, DOCK_MAX_HEIGHT]
/// logical and never taller than the area under the top margin. Why logical
/// here when everything else is physical: the preset is a DESIGN size and
/// must look the same at 100 % and 150 % DPI, so it is scaled once by the
/// monitor's factor and physical from then on. A non-finite or non-positive
/// scale reads as 1.0 — a garbage factor must not produce a 0x0 window.
pub fn dock_preset_size(work: WorkArea, scale: f64) -> (u32, u32) {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let px = |logical: u32| (f64::from(logical) * scale).round() as u32;
    let width = px(DOCK_WIDTH_LOGICAL).min(work.width).max(1);
    let wanted = (f64::from(work.height) * DOCK_HEIGHT_FRACTION).round() as u32;
    let height = wanted
        .clamp(px(MIN_HEIGHT), px(DOCK_MAX_HEIGHT_LOGICAL))
        .min(work.height.saturating_sub(DOCK_TOP_MARGIN))
        .max(1);
    (width, height)
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
    fn fallback_is_not_from_saved() {
        // The fallback size is the builder's LOGICAL 460x700; flagging it as
        // saved would make the shell re-apply it as physical pixels and open a
        // shrunken window on every hi-DPI display (RS-7).
        assert!(!SanitizedBounds::fallback().from_saved);
        assert!(!sanitize_bounds(None, &[primary()]).from_saved);
        assert!(!sanitize_bounds(raw(f64::NAN, 1.0, 460.0, 700.0), &[primary()]).from_saved);
        assert!(!sanitize_bounds(raw(1.0, 1.0, 0.0, 700.0), &[primary()]).from_saved);
    }

    #[test]
    fn saved_bounds_are_from_saved() {
        // Whenever the size came from the file it is physical and safe to
        // apply — including when only the position was unprovable.
        assert!(sanitize_bounds(raw(100.0, 120.0, 500.0, 800.0), &[primary()]).from_saved);
        let lost = sanitize_bounds(raw(2600.0, 300.0, 460.0, 700.0), &[primary()]);
        assert_eq!(lost.position, None);
        assert!(lost.from_saved);
        // Even a clamped-up size is "from the file" — the file said it exists.
        assert!(sanitize_bounds(raw(10.0, 10.0, 100.0, 100.0), &[primary()]).from_saved);
    }

    #[test]
    fn a_normal_on_screen_window_is_restored_exactly() {
        let got = sanitize_bounds(raw(100.0, 120.0, 500.0, 800.0), &[primary()]);
        assert_eq!(
            got,
            SanitizedBounds { width: 500, height: 800, position: Some((100, 120)), from_saved: true }
        );
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

    // ------------------------------------------------------- dock math ----

    #[test]
    fn dock_centres_horizontally_and_hugs_the_top_margin() {
        assert_eq!(dock_top_center(primary(), 600), (660, 8));
    }

    #[test]
    fn odd_widths_and_odd_work_areas_floor_the_half_pixel() {
        // Integer division: the odd pixel of slack goes to the right side, and
        // the result is stable rather than alternating between two rounds.
        assert_eq!(dock_top_center(primary(), 601), (659, 8));
        let odd = WorkArea { x: 0, y: 0, width: 1919, height: 1040 };
        assert_eq!(dock_top_center(odd, 600), (659, 8));
    }

    #[test]
    fn dock_follows_a_monitor_left_of_primary() {
        // Negative origins are real (see the sanitizer's left-monitor case);
        // the i64 math must not clamp them to the primary.
        let left = WorkArea { x: -1920, y: 0, width: 1920, height: 1040 };
        assert_eq!(dock_top_center(left, 600), (-1260, 8));
    }

    #[test]
    fn a_top_docked_taskbar_pushes_the_dock_below_it() {
        // Work area excludes the taskbar, so a top taskbar shows up as y > 0
        // and the window lands under it, not behind it.
        let w = WorkArea { x: 0, y: 40, width: 1920, height: 1040 };
        assert_eq!(dock_top_center(w, 600), (660, 48));
    }

    #[test]
    fn a_window_wider_than_the_display_aligns_to_its_left_edge() {
        // Negative slack would centre the window off both edges and put the
        // title bar out of reach; hugging the left edge keeps it grabbable.
        let small = WorkArea { x: 100, y: 0, width: 500, height: 800 };
        assert_eq!(dock_top_center(small, 600), (100, 8));
    }

    #[test]
    fn dock_preset_size_scales_with_dpi_and_clamps_to_the_display() {
        // 45 % of 1040 = 468 < the 520 floor → 520.
        assert_eq!(dock_preset_size(primary(), 1.0), (600, 520));
        // 150 % DPI: 900 wide; 45 % of 2100 = 945, inside [780, 1080].
        let hidpi = WorkArea { x: 0, y: 0, width: 3840, height: 2100 };
        assert_eq!(dock_preset_size(hidpi, 1.5), (900, 945));
        // A tall display gets the fraction unclamped.
        let tall = WorkArea { x: 0, y: 0, width: 2560, height: 1400 };
        assert_eq!(dock_preset_size(tall, 1.0), (600, 630));
        // Never wider than the display.
        let narrow = WorkArea { x: 0, y: 0, width: 500, height: 800 };
        assert_eq!(dock_preset_size(narrow, 1.0).0, 500);
        // Never taller than the area under the margin: 500 - 8 beats the 520 floor.
        let short = WorkArea { x: 0, y: 0, width: 1920, height: 500 };
        assert_eq!(dock_preset_size(short, 1.0).1, 492);
        // A garbage scale factor reads as 100 %, never as a 0x0 window.
        assert_eq!(dock_preset_size(primary(), f64::NAN), (600, 520));
        assert_eq!(dock_preset_size(primary(), 0.0), (600, 520));
        assert_eq!(dock_preset_size(primary(), -2.0), (600, 520));
        assert_eq!(dock_preset_size(primary(), f64::INFINITY), (600, 520));
    }
}

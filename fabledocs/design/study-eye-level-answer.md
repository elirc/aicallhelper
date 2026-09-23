> **Historical material: archived proposal (2026-09-18).** A design study, not a description of the shipped app; its code sketches and measurements were not carried over verbatim. The decisions it fed are in `../DESIGN.md` and `../AUDIT.md`.

=== verdict
build

=== recommendation
Build a "Dock to camera" placement (core pure math + one sync shell command + a header button) and an answer-first main-view layout, plus a `launchPlacement: 'remembered' | 'camera'` setting so the window opens docked every time. Everything stays physical-pixel, reuses the existing debounced bounds saver for persistence, and is unit-testable without a monitor. No custom title bar, no second global hotkey, no auto-docking mid-call.

=== rationale
The user's ask ("answers at eye/camera level") is two independent problems and both are cheap to solve on this codebase:

1. WINDOW PLACEMENT. Today the window centres on first launch (lib.rs:84 `.center()`, window.rs:38 `window.center()`) and otherwise restores wherever it was dragged. There is no "put me under the webcam" action. Tauri 2.11.5 already exposes everything needed on `WebviewWindow`: `current_monitor()`, `primary_monitor()`, `Monitor::work_area()` (a `PhysicalRect` — physical px, taskbar excluded), `Monitor::scale_factor()`, `outer_size()`/`inner_size()`, `set_size()` (tao `set_inner_size`) and `set_position()` (tao `set_outer_position`, GetWindowRect-consistent with `outer_position()`). tao emits `WindowEvent::Moved` from WM_WINDOWPOSCHANGED for programmatic moves too, so the existing `handle_window_event -> schedule_bounds_save` path (window.rs:132-159) persists the docked geometry with zero new persistence code. The math is one pure function in `bounds.rs` next to `sanitize_bounds`, tested the same way.

2. IN-WINDOW ORDER. The answer panel is the 9th block in `MainView.tsx` (after header, local banner, status, Record, hotkey notice, level meter, Ask form + Interview prep toggle, style chips, transcript) and capped at `max-height: 240px` / 14px (styles.css `.answer-body`). Even a perfectly placed window puts the answer ~400px below the camera. Moving `<AnswerPanel>` to directly under the header, letting it `flex: 1` (the column currently gives the slack to `.history-bar { margin-top: auto }`) and bumping type to 15.5px puts the first line of the answer ~60px below the top edge of the display when docked.

Both changes are additive to the SPEC and touch only well-tested seams (bounds sanitizer, settings per-field fallback, Bridge interface, MainView props).

=== design
## A. Data model

### Core (`src-tauri/core/src/store/mod.rs`)
- New enum `LaunchPlacement { Remembered (default), Camera }` with `as_str()` / `parse_or_default()` mirroring `AnswerStyle` (llm/prompt.rs:16-44). Lives in `store/` — it is a persistence/window concern, not an LLM one. Re-export from `store::`.
- `Settings` gains `pub launch_placement: LaunchPlacement` (default `Remembered`); `Settings::view()` copies it; `SettingsView` gains `pub launch_placement: LaunchPlacement` (serde camelCase -> `launchPlacement`); `SettingsPatch` gains `pub launch_placement: Option<String>` (same shape as `llm_provider`/`answer_style`: parse-or-default on apply).
- Disk key: `"launchPlacement": "remembered" | "camera"`. Absent / non-string / unknown -> `Remembered` (per-field fallback, §8). Additive: old files load unchanged; a downgraded binary ignores the key. No migration step.

### Core (`src-tauri/core/src/store/settings.rs`)
- `apply_patch`: `if let Some(v) = patch.launch_placement { next.launch_placement = LaunchPlacement::parse_or_default(v.trim()); }`
- `settings_from_disk`: `launch_placement: obj.get("launchPlacement").and_then(Value::as_str).map(LaunchPlacement::parse_or_default).unwrap_or_default(),`
- `to_disk_json`: `m.insert("launchPlacement".into(), Value::String(s.launch_placement.as_str().to_string()));`
- Tests to amend: `good_value()` add `"launchPlacement": "camera"`; `good_settings()` add `launch_placement: LaunchPlacement::Camera`; `each_corrupt_field_falls_back_alone` array becomes `[...; 9]` with `("launchPlacement", json!(3), |s| s.launch_placement = LaunchPlacement::Remembered)`; `patch_round_trips_through_disk` sends `launch_placement: Some("camera".into())` and asserts the reload. New: `unknown_launch_placement_falls_back_to_remembered` (`"launchPlacement": "sideways"` -> Remembered, resume/keys intact).

### Core (`src-tauri/core/src/store/bounds.rs`) — pure dock math
- Constants: `DOCK_TOP_MARGIN: u32 = 8` (physical), `DOCK_WIDTH_LOGICAL: u32 = 600`, `DOCK_MAX_HEIGHT_LOGICAL: u32 = 720`, `DOCK_HEIGHT_FRACTION: f64 = 0.45`.
- `pub fn dock_top_center(work: WorkArea, outer_width: u32) -> (i32, i32)` — outer top-left: `x = work.x + max(0, work.width - outer_width) / 2` (integer division floors odd slack; a window wider than the display hugs the display's LEFT edge so the title bar stays grabbable), `y = work.y + DOCK_TOP_MARGIN`. Height deliberately NOT a parameter: a top-anchored dock never moves on height, and an unused parameter would be a lie in the signature. i64 arithmetic, clamped to i32 like `finite_round`.
- `pub fn dock_preset_size(work: WorkArea, scale: f64) -> (u32, u32)` — INNER size for the header-button preset: width `min(round(600*scale), work.width)`; height `round(work.height*0.45)` clamped to `[round(520*scale), round(720*scale)]` and then `min(work.height - DOCK_TOP_MARGIN)`. Non-finite/non-positive scale reads as 1.0. Examples: 1920x1040@1.0 -> (600, 520); 3840x2100@1.5 -> (900, 945); 2560x1400@1.0 -> (600, 630); 500x800@1.0 -> (500, 520); 1920x500@1.0 -> (600, 492).
- Why logical for the preset but physical for everything else: the saved bounds and the sanitizer are physical (existing decision); the preset is a DESIGN size ("~75 characters of 15.5px text per line") and must look the same at 100% and 150% DPI, so it is scaled once by the monitor's factor at the call site and physical from then on.

## B. Shell

### `src-tauri/src/window.rs`
- `pub enum DockSize { Preset, Keep }` — `Preset` for the button (wide reading preset), `Keep` for launch (the saved size is the user's; only the position is overridden).
- `pub fn dock_to_camera(window: &WebviewWindow, size: DockSize) -> Result<(), AppError>`: monitor = `current_monitor()` else `primary_monitor()` else `Err("Could not find the display this window is on.")`; `work = work_area_of(&monitor)` (extract the existing closure body in `current_work_areas` into `fn work_area_of(&Monitor) -> WorkArea` and reuse it); inner size = preset or current; frame delta `outer_size().width - inner_size().width` measured BEFORE any resize (the decoration width is a property of the window style, so measuring first avoids depending on `set_size` having been applied when the command is not on the main thread); `set_size(PhysicalSize(inner))` only for Preset; `set_position(PhysicalPosition(dock_top_center(work, inner_w + frame_w)))`. Windows' invisible resize borders (~7px/side at 100%) are inside GetWindowRect symmetrically, so centring the outer rect centres the visible frame; the top border is ~1px so the visible top lands ~DOCK_TOP_MARGIN below the work-area edge (below a top-docked taskbar, since `work.y` already excludes it).
- `restore_geometry` unchanged. Docking at launch is a second call in lib.rs (below) so `restore_geometry`'s contract (sanitize + centre fallback) stays exactly as tested.

### `src-tauri/src/lib.rs`
After `window::restore_geometry(&win, saved_bounds);` and before `set_content_protected`/`show()`:
```
if startup.launch_placement == LaunchPlacement::Camera {
    // Size came from the saved bounds above; only the position is replaced.
    // Still pre-show, so the first visible frame is already docked.
    let _ = window::dock_to_camera(&win, window::DockSize::Keep);
}
```
Register `commands::dock_to_camera` in `generate_handler!`.

### `src-tauri/src/commands.rs`
- `#[tauri::command] pub fn dock_to_camera(app: AppHandle) -> Envelope<()>` — sync (window ops belong on the event-loop thread; sync commands run there in Tauri 2), fetches the main window, `Envelope::from_result(window::dock_to_camera(&win, DockSize::Preset))`.
- `set_settings` side effect (after the lock is released, next to the always-on-top block): if `view.launch_placement == Camera && old != Camera`, call `dock_to_camera(&win, DockSize::Keep)` so choosing the option demonstrates itself immediately. Capture `before.launch_placement` in the same tuple as `old_hotkey`.

## C. Frontend

### `src/types.ts`
- `export type LaunchPlacement = 'remembered' | 'camera';`
- `SettingsView` += `launchPlacement: LaunchPlacement;`
- `SettingsPatch` += `launchPlacement?: LaunchPlacement;`

### `src/bridge.ts`
- `Bridge` += `dockToCamera(): Promise<Envelope<null>>;`; tauri impl `dockToCamera: () => call('dock_to_camera')`.

### `src/App.tsx`
- `useHotkeyGate` wrapper += `dockToCamera: () => inner.dockToCamera(),` (TS makes forgetting this a compile error).
- `const dockToCamera = useCallback(async () => { const env = await getBridge().dockToCamera(); if (!env.ok) setErrorRef.current(env.error); }, []);` passed as `onDock={dockToCamera}`.

### `src/views/MainView.tsx`
- Prop `onDock(): Promise<void> | void`.
- Header gains an icon button BEFORE the gear: `aria-label="Dock to camera"`, `title="Move this window to the top of the screen, under the webcam"`, NOT disabled when `settings == null` (it is a window operation, not a settings edit).
- New order (top to bottom): header (status dot · title · Dock to camera · gear) · **Suggested answer** panel · error box · status line · Record button · hotkey-taken notice · level meter + timer · Ask form (+ Interview prep toggle) · style chips · local-mode banner (when local) · **Question heard** panel (compact) · history bar · sr-only live region. Rationale: output at the top where the eyes are; the error box next to the output it interrupts; controls in the middle where the hand is; the transcript (a confidence signal, not something to read while speaking) demoted; the local-mode aside demoted because it is static.
- No component changes inside AnswerPanel/TranscriptPanel: the reorder is JSX order + CSS.

### `src/styles.css`
- `.main-view { height: 100vh; min-height: 0; overflow-y: auto; }` placed after `.app` so it overrides `min-height: 100vh` for the main view only (Settings keeps the scrolling column). The column is now fixed-height and the answer panel absorbs the slack; the view only scrolls when the minimum content genuinely does not fit (interview prep open at 520px tall).
- `.answer-panel { flex: 1 1 auto; min-height: 9em; }`, `.answer-body { flex: 1 1 auto; min-height: 0; max-height: none; font-size: 15.5px; line-height: 1.5; }`, headings inside `.answer-body` to 15px, inline code to 13.5px (keep them >= body).
- `.transcript-body { min-height: 2.6em; max-height: 72px; font-size: 12.5px; }`.
- `.history-bar { margin-top: 0; }` (the answer panel owns the slack now).
- Optional at the 600px preset: `.answer-body { max-width: 62ch; }` is NOT recommended — the panel is already the measure; leave it.

### `src/views/SettingsView.tsx`
- State `launchPlacement` from `settings.launchPlacement`; included in the whole-form patch (`{ resume, jobDescription, alwaysOnTop, llmProvider, answerStyle, hotkey, launchPlacement }`).
- Field after always-on-top: label "Window position at launch", `<select id="set-placement">` with options `remembered` -> "Remember where I left it", `camera` -> "Dock under the camera (top centre)"; help text "Docked windows sit at the top of the screen, so reading the answer looks like eye contact with the webcam. The ⬆ button on the main screen docks at any time."

### `src/views/testUtils.tsx`
- `baseSettings.launchPlacement = 'remembered'`; `FakeBridge.dockToCamera = vi.fn(async (): Promise<Envelope<null>> => ok(null));`.

## D. Persisted-bounds interplay (item 3)
- Button dock: `set_size` -> `Resized`, `set_position` -> `Moved`; both hit `schedule_bounds_save`; the 500ms debounce collapses them and saves the docked inner size + outer position exactly as a drag would. No new code.
- Launch with `camera`: `restore_geometry` applies the sanitized SIZE (and a position or centre), then `dock_to_camera(Keep)` overwrites only the position on the current-else-primary monitor. Saved size survives; the saved position is ignored by design. With `remembered`, a previously docked window still launches docked as long as the same display exists (the sanitizer keeps it) — `camera` matters for laptop/dock cycles and for "I dragged it away during the call, put it back next time".
- Monitor enumeration failure: `restore_geometry` centres (unchanged); `dock_to_camera` returns an error envelope which the UI shows in the error box — honest, not a silent centre.

## E. SPEC / docs amendments (exact statements that change)
- §8 fields list: add `launchPlacement` (`"remembered"` default | `"camera"`).
- §8 "Window bounds" bullet: append "When `launchPlacement` is `camera`, the sanitized SIZE is applied and the position is then replaced by the dock target (top-centre of the display the window is on, else the primary) — saved position ignored, saved size kept. Docking moves persist through the same debounced save."
- §9 "Window" paragraph: append "A **Dock to camera** action (header button, command `dock_to_camera`) places the window horizontally centred at the top of the current display's work area (8 px below the edge; wide preset 600 logical px; height 45 % of the work area clamped to 520–720 logical px, never wider or taller than the display). Position math is pure and physical-pixel; a window wider than the display aligns to its left edge."
- §9 "Main view (top to bottom)" — replace the order with: "header with a status dot + app title + Dock to camera button + gear (Settings) button · 'Suggested answer' panel (fills the free height; 15.5 px text; …unchanged placeholder/tags/chip/buttons…) · error box (`role="alert"`) · status line · big Record button … · notice line when the hotkey is TAKEN … · level meter + mm:ss timer while recording · Ask form … · style chips … · local-mode banner (local provider only) · 'Question heard' panel (compact strip, ≤72 px; …unchanged placeholders/tags…) · history bar (hidden until 2+ entries)". Every pinned string is unchanged; only order, sizing and the new button are new.
- §9 "Settings view" list: insert "launch placement select ('Remember where I left it' / 'Dock under the camera (top centre)')" after the always-on-top checkbox.
- ARCHITECTURE.md ~587-590 (restore path): mention the post-restore dock hop. TESTING.md: add the new test entries under bounds.rs, settings.rs, bridge.test.ts, MainView.test.tsx (header 2 -> 4), App.test.tsx, SettingsView.test.tsx. IDEAS.md #12 stays (orthogonal). DEVELOPMENT.md IPC table row for `dock_to_camera`.

## F. Tests to add (item 4)
Core `bounds.rs` (6): `dock_centres_horizontally_and_hugs_the_top_margin` ((1920x1040, 600) -> (660, 8)); `odd_widths_and_odd_work_areas_floor_the_half_pixel` (601 -> 659; 1919-wide -> 659); `dock_follows_a_monitor_left_of_primary` ((-1920,0) -> (-1260, 8)); `a_top_docked_taskbar_pushes_the_dock_below_it` (work.y=40 -> y 48); `a_window_wider_than_the_display_aligns_to_its_left_edge` ((100,0,500x800), 600 -> (100, 8)); `dock_preset_size_scales_with_dpi_and_clamps_to_the_display` (the five examples in section A, plus NaN scale -> as 1.0).
Core `settings.rs`: the four amendments + `unknown_launch_placement_falls_back_to_remembered`.
Frontend: `bridge.test.ts` — extend "never rejects" with `b.dockToCamera()` and "command wiring" with `toHaveBeenLastCalledWith('dock_to_camera', undefined)`. `MainView.test.tsx` header block — `docks to the camera from the header button` (click -> `onDock` once) and `the dock button works before settings load` (`settings: null` -> enabled). `App.test.tsx` — `dock button calls the bridge and surfaces a refusal` (`bridge.dockToCamera` called once; `mockResolvedValueOnce(err('internal','Could not find the display this window is on.'))` -> alert text). `SettingsView.test.tsx` — `sends a changed launch placement` (selectOptions 'camera' -> `patch.launchPlacement === 'camera'`) and add `expect(patch.launchPlacement).toBe(baseSettings.launchPlacement)` to "omits untouched key fields but always sends the rest of the form".
No shell test for `dock_to_camera` itself (needs a live window); the shell function is a thin adapter over the two pure functions, matching how `restore_geometry` is treated today.

## G. Effort
Core pure fns + tests: S. Settings field across mod/settings/tests: S-M. Shell command + lib wiring + set_settings hop: S. Frontend button, App wiring, reorder, CSS, SettingsView field, tests: M. Docs: S. About one working day end to end; each layer is independently shippable (core -> shell -> frontend).

=== code_sketch
// ===================== src-tauri/core/src/store/bounds.rs (append) =====================

// ---------------------------------------------------------------------------
// "Dock to camera": top-centre of the current display (§9)
// ---------------------------------------------------------------------------

/// Gap between the top of the work area and the window's outer top edge.
/// Small on purpose: every pixel here is a pixel further from the webcam.
pub const DOCK_TOP_MARGIN: u32 = 8;
/// Docked preset width in LOGICAL px; the caller scales it by the monitor's
/// factor. ~75 characters of 15.5 px text per line — the top of the
/// comfortable reading measure.
pub const DOCK_WIDTH_LOGICAL: u32 = 600;
pub const DOCK_MAX_HEIGHT_LOGICAL: u32 = 720;
/// Share of the work-area height the docked window takes before clamping.
pub const DOCK_HEIGHT_FRACTION: f64 = 0.45;

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Outer top-left for a window of OUTER width `outer_width` docked to the
/// top-centre of `work`. Physical pixels, like everything else here. A window
/// wider than the display hugs the display's left edge rather than hanging
/// off both sides — the title bar must stay grabbable. Height is not an
/// input: a top-anchored dock never moves on it.
pub fn dock_top_center(work: WorkArea, outer_width: u32) -> (i32, i32) {
    let slack = i64::from(work.width) - i64::from(outer_width);
    let x = i64::from(work.x) + slack.max(0) / 2;
    let y = i64::from(work.y) + i64::from(DOCK_TOP_MARGIN);
    (clamp_i32(x), clamp_i32(y))
}

/// The docked preset INNER size (physical px) for a display with `work` area
/// and DPI `scale`: the logical width preset scaled and capped at the display;
/// 45 % of the work-area height clamped to [MIN_HEIGHT, DOCK_MAX_HEIGHT]
/// logical and never taller than the area under the top margin.
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
mod dock_tests {
    use super::*;
    fn primary() -> WorkArea { WorkArea { x: 0, y: 0, width: 1920, height: 1040 } }

    #[test]
    fn dock_centres_horizontally_and_hugs_the_top_margin() {
        assert_eq!(dock_top_center(primary(), 600), (660, 8));
    }
    #[test]
    fn odd_widths_and_odd_work_areas_floor_the_half_pixel() {
        assert_eq!(dock_top_center(primary(), 601), (659, 8));
        let odd = WorkArea { x: 0, y: 0, width: 1919, height: 1040 };
        assert_eq!(dock_top_center(odd, 600), (659, 8));
    }
    #[test]
    fn dock_follows_a_monitor_left_of_primary() {
        let left = WorkArea { x: -1920, y: 0, width: 1920, height: 1040 };
        assert_eq!(dock_top_center(left, 600), (-1260, 8));
    }
    #[test]
    fn a_top_docked_taskbar_pushes_the_dock_below_it() {
        // Work area excludes the taskbar, so a top taskbar shows up as y > 0.
        let w = WorkArea { x: 0, y: 40, width: 1920, height: 1040 };
        assert_eq!(dock_top_center(w, 600), (660, 48));
    }
    #[test]
    fn a_window_wider_than_the_display_aligns_to_its_left_edge() {
        let small = WorkArea { x: 100, y: 0, width: 500, height: 800 };
        assert_eq!(dock_top_center(small, 600), (100, 8));
    }
    #[test]
    fn dock_preset_size_scales_with_dpi_and_clamps_to_the_display() {
        assert_eq!(dock_preset_size(primary(), 1.0), (600, 520)); // 45% of 1040 < min → 520
        let hidpi = WorkArea { x: 0, y: 0, width: 3840, height: 2100 };
        assert_eq!(dock_preset_size(hidpi, 1.5), (900, 945));
        let tall = WorkArea { x: 0, y: 0, width: 2560, height: 1400 };
        assert_eq!(dock_preset_size(tall, 1.0), (600, 630));
        let narrow = WorkArea { x: 0, y: 0, width: 500, height: 800 };
        assert_eq!(dock_preset_size(narrow, 1.0).0, 500);
        let short = WorkArea { x: 0, y: 0, width: 1920, height: 500 };
        assert_eq!(dock_preset_size(short, 1.0).1, 492); // 500 - margin beats the 520 floor
        assert_eq!(dock_preset_size(primary(), f64::NAN), (600, 520));
    }
}

// ===================== src-tauri/core/src/store/mod.rs (add) =====================

/// Where the window goes on launch (§8/§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LaunchPlacement {
    /// Restore the saved bounds through the sanitizer — the original behaviour.
    #[default]
    Remembered,
    /// Re-dock to the top-centre of the current display every launch, keeping
    /// the saved size.
    Camera,
}

impl LaunchPlacement {
    pub fn as_str(self) -> &'static str {
        match self { LaunchPlacement::Remembered => "remembered", LaunchPlacement::Camera => "camera" }
    }
    /// The settings file is user-writable; unknown values fall back (§8).
    pub fn parse_or_default(raw: &str) -> Self {
        match raw { "camera" => LaunchPlacement::Camera, _ => LaunchPlacement::Remembered }
    }
}
// Settings      += pub launch_placement: LaunchPlacement,   (Default: LaunchPlacement::default())
// Settings::view += launch_placement: self.launch_placement,
// SettingsView  += pub launch_placement: LaunchPlacement,
// SettingsPatch += pub launch_placement: Option<String>,

// ===================== src-tauri/src/window.rs (add) =====================

use app_core::store::{dock_preset_size, dock_top_center, sanitize_bounds, RawBounds, WorkArea};
use app_core::AppError;
use tauri::Monitor;

/// The wide reading preset (header button) or the size the window already
/// has (launch with `launchPlacement: camera` — the saved size is the user's;
/// only the position is overridden).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockSize { Preset, Keep }

fn work_area_of(m: &Monitor) -> WorkArea {
    let area = m.work_area();
    WorkArea { x: area.position.x, y: area.position.y, width: area.size.width, height: area.size.height }
}
// (current_work_areas: `.map(work_area_of)` replaces its inline closure.)

fn os_err(e: tauri::Error) -> AppError {
    AppError::internal(format!("Could not move the window: {e}"))
}

/// Move the window to the top-centre of the display it is on (primary when
/// that cannot be determined) — directly under a webcam, so reading the
/// answer reads as eye contact. The Moved/Resized events this raises go
/// through the normal debounced bounds save, so the docked geometry persists
/// exactly like a drag would.
pub fn dock_to_camera(window: &WebviewWindow, size: DockSize) -> Result<(), AppError> {
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| AppError::internal("Could not find the display this window is on."))?;
    let work = work_area_of(&monitor);

    let inner = window.inner_size().map_err(os_err)?;
    let outer = window.outer_size().map_err(os_err)?;
    // Measured BEFORE any resize: the decoration width is a property of the
    // window style, not of its current size, and reading outer_size straight
    // after a dispatched set_size would race the event loop off-main-thread.
    let frame_w = outer.width.saturating_sub(inner.width);

    let (inner_w, inner_h) = match size {
        DockSize::Preset => dock_preset_size(work, monitor.scale_factor()),
        DockSize::Keep => (inner.width, inner.height),
    };
    if size == DockSize::Preset {
        window.set_size(PhysicalSize::new(inner_w, inner_h)).map_err(os_err)?;
    }
    let (x, y) = dock_top_center(work, inner_w + frame_w);
    window.set_position(PhysicalPosition::new(x, y)).map_err(os_err)
}

// ===================== src-tauri/src/commands.rs (add) =====================

#[tauri::command]
pub fn dock_to_camera(app: AppHandle) -> Envelope<()> {
    let Some(win) = app.get_webview_window(window::MAIN_WINDOW) else {
        return Envelope::err(AppError::internal("The main window is not available."));
    };
    Envelope::from_result(window::dock_to_camera(&win, window::DockSize::Preset))
}

// in set_settings, alongside the always-on-top hop (lock already released):
//   if view.launch_placement == LaunchPlacement::Camera && old_launch_placement != LaunchPlacement::Camera {
//       if let Some(win) = app.get_webview_window(window::MAIN_WINDOW) {
//           let _ = window::dock_to_camera(&win, window::DockSize::Keep);
//       }
//   }

// ===================== src-tauri/src/lib.rs (after restore_geometry) =====================
//   if startup.launch_placement == LaunchPlacement::Camera {
//       // Size came from the saved bounds above; only the position is replaced.
//       // Still pre-show, so the first visible frame is already docked.
//       let _ = window::dock_to_camera(&win, window::DockSize::Keep);
//   }
//   generate_handler![ ..., commands::dock_to_camera ]

// ===================== src/types.ts =====================
export type LaunchPlacement = 'remembered' | 'camera';
export interface SettingsView {
  /* ...existing... */
  /** Where the window opens: the saved bounds, or docked under the webcam. */
  launchPlacement: LaunchPlacement;
}
export interface SettingsPatch {
  /* ...existing... */
  launchPlacement?: LaunchPlacement;
}

// ===================== src/bridge.ts =====================
export interface Bridge {
  /* ...existing... */
  /** Move the window to the top-centre of its display (under the webcam). */
  dockToCamera(): Promise<Envelope<null>>;
}
// createTauriBridge: dockToCamera: () => call('dock_to_camera'),
// App.tsx useHotkeyGate wrapper: dockToCamera: () => inner.dockToCamera(),

// ===================== src/App.tsx =====================
const dockToCamera = useCallback(async () => {
  const env = await getBridge().dockToCamera();
  // A refused dock (no display found) is real information; the error box is
  // where every other core refusal lands.
  if (!env.ok) setErrorRef.current(env.error);
}, []);
// <MainView ... onDock={dockToCamera} />

// ===================== src/views/MainView.tsx (header + order) =====================
<header className="app-header">
  <span className={`status-dot status-dot--${session.state}`} aria-hidden="true" />
  <h1 className="app-title">AI Call Assistant</h1>
  {/* A window operation, not a settings edit: live before settings load. */}
  <button
    type="button"
    className="icon-button"
    aria-label="Dock to camera"
    title="Move this window to the top of the screen, under the webcam"
    onClick={() => void onDock()}
  >
    <span aria-hidden="true">⬆</span>
  </button>
  <button ref={gearRef} /* ...unchanged gear... */ />
</header>

{/* Answer first: the output sits at the top of the window, which the dock
    action puts directly under the webcam. */}
<AnswerPanel viewed={session.viewed} streaming={streaming} canRegenerate={canRegenerate}
             onRegenerate={session.regenerate} onCopyError={session.setError} />
<ErrorBox error={session.error} />
<StatusLine ... />
<RecordButton ... />
{/* hotkey-taken notice, recording row, AskForm, StyleChips — unchanged */}
{settings?.llmProvider === 'local' && ( <aside className="local-mode-banner" ...>…</aside> )}
<TranscriptPanel question={session.viewed?.question ?? ''} recording={recording && viewingLive} />
<HistoryBar ... />
<span role="status" className="sr-only">{announcement}</span>

// ===================== src/styles.css =====================
/* Main view: a fixed-height column so the answer panel (flex: 1) absorbs
   every spare pixel — tall or short window, the answer grows, not dead
   space. Scrolls only when the minimum content genuinely does not fit. */
.main-view {
  height: 100vh;
  min-height: 0;
  overflow-y: auto;
}

/* Answer first and largest: the reading surface at eye level. */
.answer-panel {
  flex: 1 1 auto;
  min-height: 9em;
}

.answer-body {
  flex: 1 1 auto;
  min-height: 0;
  max-height: none;
  font-size: 15.5px;
  line-height: 1.5;
}

.answer-body h1, .answer-body h2, .answer-body h3,
.answer-body h4, .answer-body h5, .answer-body h6 {
  font-size: 15px;
}

.answer-body code {
  font-size: 13.5px;
}

/* Transcript demoted: a compact confidence strip under the controls. */
.transcript-body {
  min-height: 2.6em;
  max-height: 72px;
  font-size: 12.5px;
}

.history-bar {
  margin-top: 0; /* the answer panel owns the slack now */
}

// ===================== src/views/SettingsView.tsx (field) =====================
<div className="field">
  <label className="field-label" htmlFor="set-placement">Window position at launch</label>
  <select id="set-placement" value={launchPlacement}
          aria-describedby="set-placement-help"
          onChange={(e) => setLaunchPlacement(e.target.value as LaunchPlacement)}>
    <option value="remembered">Remember where I left it</option>
    <option value="camera">Dock under the camera (top centre)</option>
  </select>
  <p id="set-placement-help" className="field-help">
    Docked windows sit at the top of the screen, so reading the answer looks like eye contact with the webcam. The ⬆ button on the main screen docks at any time.
  </p>
</div>

// ===================== tests (frontend) =====================
// MainView.test.tsx — header block
it('docks to the camera from the header button', async () => {
  const user = userEvent.setup();
  const { onDock } = renderMain();
  await user.click(screen.getByRole('button', { name: 'Dock to camera' }));
  expect(onDock).toHaveBeenCalledTimes(1);
});
it('the dock button works before settings load', () => {
  renderMain({}, { settings: null });
  expect(screen.getByRole('button', { name: 'Dock to camera' })).toBeEnabled();
});
// App.test.tsx
it('dock button calls the bridge and surfaces a refusal', async () => {
  const user = userEvent.setup();
  await renderApp();
  bridge.dockToCamera.mockResolvedValueOnce(err('internal', 'Could not find the display this window is on.'));
  await user.click(screen.getByRole('button', { name: 'Dock to camera' }));
  expect(bridge.dockToCamera).toHaveBeenCalledTimes(1);
  expect(await screen.findByRole('alert')).toHaveTextContent('Could not find the display this window is on.');
});
// bridge.test.ts — command wiring
await b.dockToCamera();
expect(mockInvoke).toHaveBeenLastCalledWith('dock_to_camera', undefined);
// SettingsView.test.tsx
it('sends a changed launch placement', async () => {
  const user = userEvent.setup();
  const { onSave } = renderSettings();
  await user.selectOptions(screen.getByLabelText('Window position at launch'), 'camera');
  await user.click(screen.getByRole('button', { name: 'Save' }));
  expect(lastPatch(onSave).launchPlacement).toBe('camera');
});

=== risks
- SPEC §9 "Main view (top to bottom)" is a load-bearing statement; the reorder must be amended in the same commit or the spec lies. No pinned STRING changes (all placeholders/labels/tags survive), so `format.test.ts` and the MainView string tests are untouched; the only test-count docs that move are TESTING.md's "header (2)" and the settings/bounds counts.
- `settings.rs::each_corrupt_field_falls_back_alone` has a fixed-size array type `[...; 8]` — adding the launchPlacement case must bump it to 9 or it fails to compile.
- The TS `Bridge` interface change is a compile error until `App.tsx`'s gate wrapper and `testUtils.FakeBridge` gain `dockToCamera` — good (nothing can silently forget), but it means the frontend lands as one change.
- `.main-view { height: 100vh }` replaces `min-height` for the main view: if a future block gets a large `min-height`, the view scrolls instead of the answer shrinking below `9em` — intended, but a visual regression to check at the 380x520 minimum with Interview prep expanded (the ask section then scrolls; that is acceptable).
- Windows invisible borders: centring uses the GetWindowRect outer width (the same rect `set_outer_position` positions), so the visible frame is centred; on a window with `decorations(false)` in the future the frame delta becomes 0 and the math still holds.
- `current_monitor()` before `show()`: works on Windows (MonitorFromWindow on a hidden HWND); the primary fallback covers a `None` anyway, and a total enumeration failure returns an error envelope on the button path / is swallowed on the launch path (the window is still centred by `restore_geometry`).
- Sync command thread: Tauri 2 runs non-async commands on the main thread; if that ever changes, the pre-measured frame delta keeps the position correct and `set_size`/`set_position` still dispatch safely through the event-loop proxy.
- Docking with `alwaysOnTop: false` puts the window under the call app the moment it is focused; not a bug, but worth a help-text sentence in Settings (already in the design's help copy is the eye-contact line; add "works best with always-on-top").
- Saved-bounds creep: docking saves the inner size (existing `current_bounds` uses `inner_size`), so the "monotonically larger window" bug documented in window.rs:74-77 does not return.

=== dont_build
- A frameless/custom title bar to reclaim the 32 px caption (loses native move/resize/close affordances; `data-tauri-drag-region` plus content protection is untested here; §9 says decorations on). Revisit only if users ask for the last 32 px.
- A second GLOBAL hotkey for dock: `hotkey.rs::apply_hotkey` calls `unregister_all()` and the app deliberately owns exactly one shortcut; a header button (and the launch setting) covers the need without a registry refactor.
- Auto-docking when an answer starts streaming: a window that jumps mid-call is worse than a stable one; the user docks once and it persists.
- Per-monitor / per-arrangement remembered bounds (IDEAS.md #12): orthogonal, still valid, not needed for eye-level placement.
- Click-through / translucent overlay mode, opacity sliders, "compact mode" that hides controls: the answer-first layout plus the 45 %-height dock already keeps the call window mostly visible.
- Frontend-side monitor math via `@tauri-apps/api/window` `currentMonitor()`/`setPosition()`: it works, but it would split geometry logic across two languages, bypass the pure/testable core seam the sanitizer established, and require enabling window-API capabilities in the ACL for the webview. Keep geometry in Rust.
- Per-tech-stack profiles: out of this design study's scope (the other finder covers it); nothing here couples to the prompt.

=== findings
{
 "id": "DOCK-1",
 "title": "Answer panel is the 9th block and capped at 240px — the primary output is far below the camera",
 "area": "ui/layout",
 "severity": "high",
 "evidence": "src/views/MainView.tsx renders header → local banner → StatusLine → RecordButton → hotkey notice → recording row → AskForm (+Interview prep toggle) → StyleChips → TranscriptPanel → AnswerPanel; src/styles.css `.answer-body { min-height: 7em; max-height: 240px; }` at 14px; `.history-bar { margin-top: auto }` takes the column slack instead of the answer.",
 "problem": "Even with the window placed under the webcam, the first answer line sits ~400px below the top edge, and a taller window only adds dead space above the history bar rather than answer area.",
 "proposal": "Move <AnswerPanel> to directly under the header (then ErrorBox, StatusLine, Record, controls, TranscriptPanel demoted to a 72px strip, HistoryBar); `.main-view { height: 100vh; overflow-y: auto }`, `.answer-panel { flex: 1 1 auto; min-height: 9em }`, `.answer-body { flex: 1; max-height: none; font-size: 15.5px; line-height: 1.5 }`, `.history-bar { margin-top: 0 }`. Amend SPEC §9 'Main view (top to bottom)' in the same change.",
 "effort": "M",
 "files": [
  "src/views/MainView.tsx",
  "src/styles.css",
  "docs/SPEC.md",
  "docs/TESTING.md"
 ],
 "risk": "SPEC §9 order statement changes (no pinned strings do); verify the 380x520 minimum with Interview prep expanded scrolls acceptably; MainView tests query by role/text so the reorder should not break them."
}
{
 "id": "DOCK-2",
 "title": "No way to place the window at eye level; launch centres and the user drags every session",
 "area": "window/shell",
 "severity": "high",
 "evidence": "src-tauri/src/lib.rs:84 `.center()`; src-tauri/src/window.rs:29-41 `restore_geometry` centres when no provable position; no command touches position beyond restore. Tauri 2.11.5 exposes `WebviewWindow::current_monitor/primary_monitor/outer_size/inner_size/set_size/set_position` and `Monitor::work_area()/scale_factor()`; tao raises `Moved` on WM_WINDOWPOSCHANGED for programmatic moves (event_loop.rs:1210-1213), so the existing debounced saver persists a dock for free.",
 "problem": "Eye-contact reading needs the window top-centred on the display under the webcam; today that is a manual drag after every launch/dock/undock.",
 "proposal": "Pure `dock_top_center(work, outer_width)` + `dock_preset_size(work, scale)` in core/store/bounds.rs; `window::dock_to_camera(&win, DockSize)` adapter measuring the frame delta before resizing; sync `#[tauri::command] dock_to_camera`; `Bridge.dockToCamera()`; header icon button 'Dock to camera' (enabled before settings load). Six pure tests (odd widths, left-of-primary, top taskbar, wider-than-display, DPI/clamp).",
 "effort": "M",
 "files": [
  "src-tauri/core/src/store/bounds.rs",
  "src-tauri/core/src/store/mod.rs",
  "src-tauri/src/window.rs",
  "src-tauri/src/commands.rs",
  "src-tauri/src/lib.rs",
  "src/bridge.ts",
  "src/App.tsx",
  "src/views/MainView.tsx",
  "src/views/testUtils.tsx",
  "src/state/bridge.test.ts",
  "src/views/MainView.test.tsx",
  "src/views/App.test.tsx"
 ],
 "risk": "Bridge interface change forces FakeBridge/gate wrapper updates (compile-time). Monitor enumeration failure returns an error envelope shown in the error box. DEVELOPMENT.md IPC table and TESTING.md need rows."
}
{
 "id": "DOCK-3",
 "title": "Add `launchPlacement` ('remembered' | 'camera') so the app opens docked every launch",
 "area": "settings/persistence",
 "severity": "medium",
 "evidence": "src-tauri/core/src/store/mod.rs Settings has no placement policy; settings.rs `settings_from_disk`/`to_disk_json` per-field pattern (llmProvider/answerStyle) is directly reusable; lib.rs applies `restore_geometry` before `show()` so a post-restore dock is invisible to the user.",
 "problem": "With 'remembered' only, a laptop dock/undock or a mid-call drag means the next launch is not at the camera; the sanitizer rightly recentres when the display set changes.",
 "proposal": "`LaunchPlacement` enum in store/mod.rs (as_str/parse_or_default like AnswerStyle), Settings/SettingsView/SettingsPatch fields, disk key `launchPlacement` with fallback to Remembered, lib.rs `if Camera { dock_to_camera(&win, DockSize::Keep) }` after restore_geometry (keeps saved size, overrides position), set_settings hop docks immediately when switching to Camera, SettingsView select 'Window position at launch'. Amend settings.rs tests (good_value/good_settings/corrupt-field array to 9/round-trip) and SPEC §8/§9.",
 "effort": "S",
 "files": [
  "src-tauri/core/src/store/mod.rs",
  "src-tauri/core/src/store/settings.rs",
  "src-tauri/src/lib.rs",
  "src-tauri/src/commands.rs",
  "src/types.ts",
  "src/views/SettingsView.tsx",
  "src/views/SettingsView.test.tsx",
  "src/views/testUtils.tsx",
  "docs/SPEC.md"
 ],
 "risk": "`each_corrupt_field_falls_back_alone` has a fixed `[...; 8]` array type — must become 9. Additive on disk; no migration; downgrade ignores the key."
}
{
 "id": "DOCK-4",
 "title": "Sanitizer clamps saved PHYSICAL size to a LOGICAL minimum",
 "area": "window/geometry",
 "severity": "low",
 "evidence": "src-tauri/core/src/store/bounds.rs:14-15 `MIN_WIDTH: u32 = 380; MIN_HEIGHT: u32 = 520` applied to physical saved bounds at :102-103, while src-tauri/src/lib.rs:77 `.min_inner_size(380.0, 520.0)` is logical (tao scales it by DPI).",
 "problem": "At 150% DPI the OS minimum is 570x780 physical, so the sanitizer's 380x520 clamp is below what the window can actually be; the 40px visibility judgement then runs at a size the OS will never produce (a corrupt tiny save is judged visible at 380 wide but restores at 570). Harmless today because the OS clamps again, but it is a silent DPI inconsistency the dock code should not copy.",
 "proposal": "Either document MIN_WIDTH/MIN_HEIGHT as 'physical floor at 100%' in bounds.rs, or pass the current monitor's scale into `sanitize_bounds` (scaling the floor) — the dock preset already scales by `scale_factor()`; doing the same in the sanitizer makes the two consistent. Add a test `minimum_is_scaled_by_dpi` if the second option is taken.",
 "effort": "S",
 "files": [
  "src-tauri/core/src/store/bounds.rs",
  "src-tauri/src/window.rs"
 ],
 "risk": "Changes `size_is_clamped_up_to_the_window_minimum` and `visibility_is_judged_at_the_clamped_size` expectations if the scale parameter is added; SPEC §8 'clamp size up to the window minimum' would need '(DPI-scaled)'."
}
{
 "id": "DOCK-5",
 "title": "Dock button should hint when always-on-top is off",
 "area": "ux",
 "severity": "low",
 "evidence": "src-tauri/core/src/store/mod.rs Settings.always_on_top default true; the SettingsView checkbox can turn it off; the docked window then drops behind the call app on focus.",
 "problem": "A docked window that vanishes behind Zoom/Teams the moment the user clicks the call looks like the dock 'did not work'.",
 "proposal": "In the Settings help text for launch placement add 'Works best with \"Keep this window always on top\"'; optionally, in App.tsx after a successful dock with `settings.alwaysOnTop === false`, set a one-line status hint. No core change.",
 "effort": "S",
 "files": [
  "src/views/SettingsView.tsx",
  "src/App.tsx"
 ],
 "risk": "None beyond copy; keep the string out of SPEC unless pinned."
}
{
 "id": "DOCK-6",
 "title": "Local-mode banner and hotkey-taken notice sit above the output",
 "area": "ui/layout",
 "severity": "low",
 "evidence": "src/views/MainView.tsx renders the `local-mode-banner` aside immediately after the header (~60px) and the hotkey notice under the Record button; both push everything below them down.",
 "problem": "Static, informational blocks occupy the eye-level rows the answer should own.",
 "proposal": "As part of DOCK-1, move the local-mode banner below the style chips (it is a persistent aside, not a status) and keep the hotkey notice next to the Record button it explains. Both are covered by existing tests (`LocalMode.test.tsx`, MainView 'hotkey taken notice') that query by text/role, not position.",
 "effort": "S",
 "files": [
  "src/views/MainView.tsx",
  "docs/SPEC.md"
 ],
 "risk": "SPEC §9 main-view order statement must list the banner's new position."
}

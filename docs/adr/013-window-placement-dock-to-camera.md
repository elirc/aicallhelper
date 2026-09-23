# ADR 013 — Window placement: dock to the camera by default, remember on request

**Status**: accepted

## Context

The app exists to be read *while the user looks at the other person*. On a
laptop the webcam sits at the top-centre of the display, so the closer the
answer text is to that spot, the more reading it looks like eye contact. v3
did the opposite twice over: the window centred itself on first launch
(`lib.rs` builder `.center()`, and `restore_geometry` centred again whenever
the saved position could not be proven visible), and inside the window the
answer panel was the ninth block from the top, capped at 240 px. Even a
perfectly placed window put the first line of the answer ~400 px below the
camera (audit DOCK-1/DOCK-2, UX-01/UX-03).

The placement half of that problem has three constraints of its own:

- Geometry math must stay **pure and testable without a monitor** — the
  sanitizer in `core/src/store/bounds.rs` set that precedent (SPEC §8), and a
  second geometry path written against live `WebviewWindow` calls would be
  untestable.
- The saved bounds are **physical pixels**; a design size ("~75 characters
  of 16 px text per line") is a **logical** size that must look the same at
  100 % and 150 % DPI. The two must not be confused — v3 already had one such
  confusion: the first-run fallback re-applied the builder's logical 460×700
  as a `PhysicalSize`, shrinking the window by the scale factor on every
  125–150 % laptop and then persisting that as the user's "choice" (RS-7).
- A window that jumps mid-call is worse than one that is slightly misplaced.

## Decision

- **The math lives in the core, pure and physical**
  (`src-tauri/core/src/store/bounds.rs:147-195`): `dock_top_center(work,
  outer_width)` returns the outer top-left for a window horizontally centred
  in a display's work area with an 8 px top margin (a window wider than the
  display hugs the left edge so the title bar stays grabbable; height is
  deliberately not an input — a top-anchored dock never moves on it), and
  `dock_preset_size(work, scale)` returns the wide reading preset as an
  INNER size: 600 logical px wide, 45 % of the work-area height clamped to
  520–720 logical px, never wider or taller than the display. The preset is
  the one place logical units enter, scaled once by the monitor's factor at
  the call site.
- **The shell is a thin adapter** (`src-tauri/src/window.rs:91-135`):
  `dock_to_camera(window, DockSize::{Preset, Keep})` resolves the current
  monitor (else the primary, else the error `Could not find the display this
  window is on.`), measures the frame delta `outer − inner` **before** any
  resize (the decoration width is a property of the window style, and reading
  `outer_size` right after a dispatched `set_size` would race the event loop
  when the caller is not on the main thread), applies the preset size only
  for `Preset`, then positions the outer rect. The `Moved`/`Resized` events
  this raises go through the existing debounced bounds save, so a dock
  persists exactly like a drag — zero new persistence code.
- **Three call sites, one mechanism**:
  - the header button → sync command `dock_to_camera` → `DockSize::Preset`
    (`src-tauri/src/commands.rs:342-348`);
  - launch with `launchPlacement: "camera"` (the default;
    `core/src/store/mod.rs:110-120`) → after `restore_geometry`, before
    `show()`, `DockSize::Keep` — the saved SIZE is the user's, only the
    position is replaced, and the first visible frame is already docked
    (`src-tauri/src/lib.rs:110-117`). Choosing the option in Settings docks
    immediately, also with `Keep` (`commands.rs:117-124`);
  - the sanitizer's fallback: when the saved position cannot be proven
    visible, `restore_geometry` docks with `Keep` instead of centring, and
    centres only if docking itself fails — centre is the one spot guaranteed
    reachable when even the monitor list cannot be trusted
    (`window.rs:42-58`).
- **First run keeps the builder's logical size** (RS-7): `SanitizedBounds`
  carries `from_saved`, and the shell applies `set_size` only when the bounds
  came from the file (`bounds.rs:59-76`, `window.rs:45-47`). The fallback
  460×700 is never re-applied as physical pixels.
- **The sanitizer's minimum stays logical-at-100 %** (DOCK-4, option A):
  `MIN_WIDTH`/`MIN_HEIGHT` are documented as the `min_inner_size` floor
  expressed in physical pixels at 100 % scale (`bounds.rs:18-25`). On a
  hi-DPI display the OS enforces a larger floor; the sanitizer's smaller
  floor is safe because visibility is monotonic in size for a fixed top-left,
  so judging at the smaller size never accepts a position the larger window
  would fail. No scale parameter was added to `sanitize_bounds`.
- **`launchPlacement` is a closed enum on the wire and on disk**:
  `"remembered" | "camera"`, unknown file values fall back to `camera`
  (SPEC §8 per-field rule), an invalid patch value fails deserialization.

## Consequences

- The window opens under the webcam on every launch by default, at whatever
  size the user last had, and a mid-call drag is undone next launch — which
  is the point for laptop/dock cycles and for "I moved it out of the way
  during the call". `remembered` restores the v3 behaviour for anyone who
  prefers it, with the sanitizer's fallback now docking rather than centring.
- Together with the answer-first layout (SPEC §9), the first answer line
  sits roughly 60 px below the top edge of the display when docked.
- Pure functions carry the test weight (the six dock cases plus the
  `from_saved` pair in `bounds.rs`); `window::dock_to_camera` itself has no
  shell test because it needs a live window — the same treatment
  `restore_geometry` already had.

Costs, honestly:

- **The saved position is ignored on every launch** while `camera` is
  selected — the setting says so, and the position is still saved (a flip to
  `remembered` brings it back), but a user who did not read the help text
  will see the window "forget" where they put it.
- **Always-on-top off + docked = hidden.** A docked window drops behind the
  call app the moment the call is focused; not a bug, but it reads like the
  dock "did not work". The Settings help text carries the hint.
- On the button path a display that cannot be resolved surfaces as an error
  envelope in the error box; on the launch path the same failure is
  swallowed and the sanitizer's placement stands. Honest where a person is
  watching, silent where nobody could act.
- One more persisted field means one more corrupt-field case in the settings
  matrix and one more fixed-size array to keep in sync.

## If revisited

Per-arrangement remembered bounds (IDEAS #12) must key the placement too, or
a docked laptop arrangement would restore a remembered desktop position. A
frameless title bar to reclaim the 32 px caption was refused (native
move/resize/close affordances, untested with content protection); so were a
second global hotkey for docking (`hotkey.rs` owns exactly one shortcut) and
auto-docking when an answer starts streaming (a window that jumps mid-call
is worse than a stable one). If Tauri ever runs sync commands off the main
thread, the pre-measured frame delta keeps the position correct and
`set_size`/`set_position` still dispatch through the event-loop proxy.

# AI Call Assistant v3.1 — change report (2026-09-18)

Everything below is in the working tree, **uncommitted**, so you can review
it with `git diff` and `git status` before deciding what to keep. Nothing was
pushed. The companion documents in this folder:

| File | What it is |
|---|---|
| `AUDIT.md` | The audit that started this: findings, verifier verdicts, and the build / defer / reject decision for each |
| `DESIGN.md` | The implementation contract the work followed (IPC shapes, disk format, prompt changes, layout, work packages, verification matrix, what was deliberately not built) |
| `USER-GUIDE.md` | Install, build and deploy, first-time setup, using it on a call, profiles, window placement, troubleshooting |
| `design/` | The two design studies (eye-level answer, call profiles) and the raw audit output |

## 1. Summary

The app now puts the answer where your eyes are, knows what kind of call it
is on, and does less work per token and per click.

- **Answer at eye level.** The suggested answer is the first block under the
  header, fills the window, and reads at 16 px (18 px in focus mode). A
  "Dock to camera" button and a launch-placement setting put the window
  top-centre on the display, directly under the webcam; the sanitizer's
  off-screen fallback docks instead of centering. The transcript collapses to
  a caption once the recording ends, errors land directly under the answer,
  history navigation lives in the answer head, and a focus mode hides
  everything but the answer, status, error and Record.
- **Call profiles.** Up to eight named profiles, each with a call type
  (interview, sales, support, meeting, other), a focus line (the tech-stack
  lever), resume or "about you", job description or call context, and extra
  instructions. One is active; chips on the main view switch it. A v3
  settings file migrates into a "Default" profile, and an interview profile's
  prompt is byte-identical to v3, so nobody pays a cache miss for upgrading.
- **Faster and lighter.** LLM tokens are coalesced into at most one React
  reconcile per 16 ms; settings writes left the event-loop thread; the audio
  device opens on the blocking pool so the speech socket dials ~100 ms
  earlier; the redundant Ask-path pre-warm that raced the answer request is
  gone; the HTTP client keeps pooled sockets alive; first run no longer
  mis-sizes the window on hi-DPI displays; lazy chunks are preloaded and open
  without a fallback flash; a dead polyfill and an unused dependency are gone.
- **Refactors.** One provider-capability table instead of seven `local`
  special cases, typed enums on the settings patch, a single transient-state
  hook instead of four hand-rolled timers, a testable frame-coalescing hook,
  table-driven key fields, the hotkey gate as a hook option, `HotkeyStatus`
  declared once, the dead `open_external` command removed, and the
  local-voice shell module split into pure, tested functions.
- **Docs.** The spec, README, testing catalogue and every other document were
  brought back in line with the code (they had drifted at the local-voice
  commit) and extended for the new features, with two new ADRs.

Test inventory before and after:

| Suite | Before | After |
|---|---|---|
| Rust core | 248 | 302 unit + 1 integration |
| Tauri shell | 36 | 50 (compiled, not executed here; see §6) |
| Frontend (runtime cases) | 300, 1 flaky | 375, 0 flaky |

## 2. The two explicit requests

### 2.1 "Answers in the center-middle-top of the screen at eye/camera level"

Two independent problems, both solved:

1. **Inside the window** (`src/views/MainView.tsx`, `src/styles.css`,
   `src/components/{AnswerPanel,TranscriptPanel,HistoryBar,RecordButton}.tsx`):
   the column is now header · profile chips · answer panel (flex-grow, no
   height cap, 16 px / 1.5) · error box · status line · Record + style chips ·
   Ask form + hints · local-mode banner · transcript strip · live region.
   The recording row keeps its height reserved so the meter mounting never
   shifts the text being read. Every pinned string from the spec survives.
2. **On the display** (`src-tauri/core/src/store/bounds.rs`,
   `src-tauri/src/{window,lib,commands}.rs`): pure, unit-tested dock math
   (`dock_top_center`, `dock_preset_size`), a thin `dock_to_camera` adapter
   that measures the frame delta before resizing and uses the current monitor
   (primary as fallback), a `dock_to_camera` command behind the ⬆ header
   button (wide reading preset: 600 logical px, 45 % of the work-area height),
   and a `launchPlacement` setting whose default `camera` re-docks on every
   launch keeping the saved size. The saved-bounds sanitizer's "cannot prove
   the position is on a display" fallback now docks instead of centering.

Also for reading while speaking: `streamFollow` (`tail` default, unchanged
behaviour; `top` keeps the panel at the opening sentence), and focus mode
(header ◎ button or Ctrl+Shift+F inside the window; not persisted).

### 2.2 "Separate profiles for different tech stacks / call types"

Verdict from the design study (`design/study-call-profiles.md`): yes, build
the lean version, because the single resume/JD forces a re-paste before
every call in a parallel search and the failure mode when you forget is an
answer confidently grounded in the wrong job description. The heavy version
(free-text personas, auto-detection from the transcript, per-question
switching) was rejected: it puts work inside the latency window and breaks
the byte-stable cached prefix on every question.

What was built:

- `CallProfile { id, name, callType, resume, jobDescription, focus, extraInstructions }`, up to 8, one active (`activeProfileId`).
- Prompt (`core/src/llm/prompt.rs`): additive only. Interview keeps the v3
  headers verbatim; other call types get a one-line call framing plus
  "ABOUT THE USER" / "CONTEXT FOR THIS CALL" headers and their own grounding
  note; focus and extra instructions get their own headers. All inside the
  cached prefix, in a fixed order, byte-stable; the style suffix is untouched.
- Store (`core/src/store/{mod,settings}.rs`): one pure, deterministic
  `normalize_profiles` enforces every invariant (count, caps, id repair,
  active resolution); v3 files migrate on read and are never written back in
  the old shape; per-field fallback applies inside each profile.
- IPC: `SettingsView`/`SettingsPatch` carry `profiles` + `activeProfileId`;
  a profile switch from the main view sends only `activeProfileId`.
- UI: Settings gets a Profile fieldset (select, New / Duplicate / Delete,
  per-type field labels, character and local-mode byte counters); the main
  view gets `ProfileChips` (hidden with one profile) and an "ungrounded
  profile" hint; Interview prep is hidden for non-interview profiles.
- Local mode's 7 KB cap message now names the whole profile.

Not built, on purpose: auto-detected call type, per-question profile
switching, per-profile default answer style, free-text call types, cloud
sync, JD-from-URL, per-profile keys or hotkeys (reasons in `DESIGN.md` §10).

## 3. Changes by area

### Rust core (`src-tauri/core`)

| File | Change |
|---|---|
| `llm/prompt.rs` | `CallType` enum; `Profile` gains `call_type`, `focus`, `extra_instructions`; nine new constants; `sections_for`; new prefix order; 8 new tests incl. the v3 byte-identity pin |
| `llm/mod.rs` | `CallType` re-export; `needs_cloud_keys()` / `uses_deepgram()`; `answer_limits()` default trait method |
| `llm/local.rs` | `answer_limits()` override (90 s / 300 s); oversize message names the profile |
| `llm/http.rs` | Shared client: 3 s connect timeout, TCP keepalive 20 s with 1 s interval; no whole-request timeout |
| `session/mod.rs` | `AnswerLimits { first_token, total }` with `CLOUD` / `LOCAL` |
| `session/machine.rs` | Ask path no longer pre-warms (the warm raced the answer POST for the one pooled HTTP/1.1 connection); limits come from the provider |
| `store/mod.rs` | `CallProfile`, `CallProfilePatch`, `LaunchPlacement` (default camera), `StreamFollow` (default tail), new `Settings`/`SettingsView`/`SettingsPatch` shapes with typed enum patch fields, `active_profile()` that never panics |
| `store/settings.rs` | `normalize_profiles`; migration; new disk shape; per-field fallback inside profiles; 21 new tests |
| `store/bounds.rs` | `dock_top_center`, `dock_preset_size`, `SanitizedBounds.from_saved`; DPI doc comment; 8 new tests |
| `tests/local_settings.rs`, provider tests | Adapted to the new shapes |

### Tauri shell (`src-tauri/src`)

| File | Change |
|---|---|
| `window.rs` | `DockSize`, `work_area_of`, `dock_to_camera`; `restore_geometry` skips the physical resize on first run and docks when no position can be proven |
| `lib.rs` | No builder `.center()`; pre-show dock when launch placement is camera; `dock_to_camera` registered; `open_external` removed; debug-only stage timings |
| `commands.rs` | `set_settings` is async with the fsync'd write on the blocking pool and the hotkey re-registration hopped to the main thread; `dock_to_camera`; `start_session` opens the capture on the blocking pool; deps built from the active profile and the capability helpers; 4 new tests |
| `local_voice.rs` | Pure `parse_tags`, `parse_health`, `parse_config`, `launch_plan`, `launch_spec` with 10 tests; I/O confined to thin wrappers; user-facing strings unchanged |
| `Cargo.toml`, `tauri.conf.json` | Version 3.1.0 (also `core/Cargo.toml`, `package.json`) |

### Frontend (`src`)

| File | Change |
|---|---|
| `types.ts` | The contract: `CallType`, `LaunchPlacement`, `StreamFollow`, `CallProfile`, `HotkeyStatus`, new settings shapes, limits, `PROVIDERS` / `CALL_TYPES` catalogues, `DEFAULT_HOTKEY` mirroring the core |
| `bridge.ts` | `dockToCamera`, `localVoiceStatus`, `prepareLocalVoice`; local-voice helpers moved behind the bridge seam |
| `App.tsx` | Hotkey gate is now `useSession({ hotkeyEnabled })`; profile switch, dock, focus mode; Settings opens in a transition; lazy chunks preloaded 1.5 s after mount |
| `state/useSession.ts` | Leading+trailing delta coalescer (16 ms window, per session, flushed before every ordering-sensitive event and before a supersede) |
| `views/MainView.tsx` | Answer-first order, header buttons, profile chips, hints, focus mode, Ctrl+Shift+F, transcript auto-collapse wiring, history in the answer head |
| `views/SettingsView.tsx` + `SettingsView.css` (new) | Three fieldsets, profile editor, table-driven key fields, counters, launch placement and stream-follow selects, corrected hotkey placeholder and help, sticky actions, dirty guard with an accessible confirm row, re-seed from the saved view |
| `components/AnswerPanel.tsx` | History nav props, `streamFollow` seeding, `useFrameCoalesced`, `useTransient` |
| `components/TranscriptPanel.tsx` | Collapsible disclosure (`<h2><button aria-expanded>`), caption when collapsed |
| `components/ProfileChips.tsx` (new) | Persisted-value chips, hidden with one profile |
| `components/{useTransient,useFrameCoalesced}.ts` (new) | Shared hooks replacing four timer sites and the dependency-less layout effect |
| `components/{AskForm,RecordButton,StatusLine,HistoryBar,LocalVoicePanel}.tsx` | `showPrep`, compact Record, empty status line until settings load, bridge-driven local panel, chunk preload on hover |
| `styles.css`, `components/LocalVoicePanel.css` | Answer-first layout, control rows, reserved recording row, global `[hidden]` reset, tokens instead of hex, Settings-only rules moved out |
| `vite.config.ts`, `package.json` | chrome120 target, no modulepreload polyfill, no gzip reporting; unused `@tauri-apps/plugin-global-shortcut` removed |
| Tests | 75 new runtime cases; the flaky interview-prep test now waits up to 10 s for the lazy chunk |

### Documentation (`docs`, `README.md`)

SPEC §1–§4, §6.4, new §6.5 (free local voice), §7–§9, §11–§13; README
(profiles setup, dock/focus, 21-step manual QA); TESTING.md rebuilt with one
bullet per test and corrected totals; ARCHITECTURE, DEVELOPMENT, SECURITY,
TROUBLESHOOTING, IDEAS, FREE_VOICE_MODE, docs/README; ADR 005 and 007
amended; ADR 013 (window placement) and 014 (call profiles) added; stale test
counts in ADR 001/003 replaced with a pointer.

## 4. Review and fixes

After implementation, four independent read-only reviewers went over the
diff (Rust correctness and invariants; frontend correctness and
accessibility; IPC contract consistency; security and privacy). Results:

- Rust: no defects. Byte-identity, determinism, migration, atomic-write
  discipline, no-panic `active_profile`, serde shapes, dock edge cases,
  no lock held across an await, no key or profile text logged — all verified.
- Frontend: three low findings, all fixed and covered by new tests: the
  Record-button and typed-ask supersede paths now flush the delta buffer
  (only the hotkey path did); the transcript toggle uses the standard
  disclosure pattern so the heading stays in the accessibility tree; the
  unsaved-changes dialog references its question.
- Contract: no code mismatch; six spec sentences flagged and rewritten.
- Security: only a stale SECURITY.md paragraph about the removed command;
  rewritten.

## 5. Verification (run by me, after all agents finished)

| Gate | Command | Result |
|---|---|---|
| Rust core tests | `cargo test -p app-core` (src-tauri/) | 302 passed, 0 failed; integration 1 passed |
| Rust core lint | `cargo clippy -p app-core -- -D warnings` | clean |
| Shell compile incl. tests | `cargo check -p aicallhelper --tests` | clean |
| TypeScript | `npm run typecheck` | clean |
| Frontend tests | `npx vitest run --maxWorkers=2` | 16 files, 375 passed, 0 failed (109 s) |
| Bundle | `npm run build` | index 178.3 kB + 8.8 kB CSS; SettingsView 14.6 kB + 2.0 kB CSS (lazy); PracticeLibrary 6.0 kB + 1.5 kB CSS (lazy); modulepreload polyfill gone |

The agents' own runs agree (core 302/302, shell check clean, frontend
375/375 twice, build green), and one mutation check confirmed the new
coalescer tests fail without the fix.

## 6. What could not be verified here, and why

- **The 50 shell unit tests were compiled but not executed.** Running them
  needs a full debug link of the Tauri tree, which needs several GB; the C:
  drive had between 0.4 and 5 GB free during this session. The same limit
  blocks `npm run tauri dev`, `npm run tauri build`, and therefore a live
  smoke test of the built app.
- **Manual QA on hardware** (README steps): docking on 125 % / 150 % DPI
  displays and with a top-docked taskbar, the first-run window size on
  hi-DPI, always-on-top interplay, the real Deepgram/Anthropic/Groq paths.
  Nothing in the test suites touches a device, a window or the network.
- **Three gate runs were killed by the harness** mid-session because the
  machine was critically low on memory (about 1.6 GB free of 15.8 GB, most of
  it held by VS Code, Chrome and other apps). They were re-run one at a time
  with limited parallelism and all passed; no code was changed in between.

To finish verification once disk is available:

```powershell
cd src-tauri; cargo test -p aicallhelper      # the 50 shell tests
cd ..; npm run tauri dev                       # then walk README's manual QA script
```

## 7. Machine housekeeping done during the session

- `cargo clean` of `src-tauri/target` (3.5 GB of regenerable build output) when the disk hit 92 MB free.
- `npm cache clean --force` (3.8 GB, regenerable) and removal of `src-tauri/target/debug/incremental` (0.9 GB) when the disk hit 975 MB free mid-implementation.
- Nothing outside the project's caches was touched. The drive's real consumers are user data (Desktop ≈57 GB, AppData ≈49 GB) and Windows (≈38 GB).

## 8. Deviations from the design contract (all deliberate, all documented in the agent reports)

- The delta coalescer re-arms its window while tokens keep arriving, so a steady stream renders exactly once per 16 ms rather than alternating.
- App's chunk preload uses its own `import()` site rather than an export from a component module (keeps Fast Refresh working); Rollup still emits one chunk.
- The local-voice speech port constant lives in the shell and is pinned against the core's URL by a test, because the shell package could not edit the core crate.
- The main-thread hotkey hop falls back to in-thread registration if the hop is refused during shutdown.
- Two new user-facing strings were worded by the implementers: "The audio capture thread failed to start. Try restarting the app." and "Could not save settings. Try again."
- Version bumped to 3.1.0 in `package.json`, `package-lock.json`, `tauri.conf.json` and both `Cargo.toml` files (Cargo.lock refreshed by the compile check).

## 9. Suggested follow-ups

1. Free disk, run the shell tests and the app, walk the manual QA script, then commit (`git add -A && git commit`); the tree is clean of stray files apart from `dist/` (gitignored) and this folder.
2. Deferred, still worth doing when measured: Rust boot payload so the first frame carries settings (FE-2 thorough form), overlapping the settings load with WebView2 creation (RS-5), a lighter settings summary for chip clicks if profiles get large (PROF-07), preact/compat only after measuring startup (FE-8).
3. If Groq or Anthropic ever ship a different SSE cadence, re-measure the 16 ms coalescing window against first-word latency; the first delta is always synchronous, so the metric itself is unaffected.

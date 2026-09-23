# Design contract — v3.1 "eye-level answers, call profiles, faster app"

> **Historical (2026-09-18).** This was the contract for the v3.1 implementation pass. Its work packages (§3–§8) are a record of that pass, not a standing change request; current behavior is specified in `docs/SPEC.md`. Corrected 2026-09-22: the audit-verification count below (see `PROJECT-REVIEW-2026-09-19.md`).

This file is the single source of truth for the implementation work packages.
Every implementer reads it before touching code. `src/types.ts` is already
rewritten and is the authoritative TypeScript side of the IPC contract; the
Rust side must serialize to exactly those shapes.

Inputs: the audit in `fabledocs/design/audit-findings-raw.md` (each finding
was handed to an independent verifier; 32 of the 42 verifications completed,
and the remaining ten — six Rust-startup items and four low-priority frontend
items — were judged by hand from the finder evidence, as recorded in
`AUDIT.md`; the completed verdicts are at the end of `audit-findings-raw.md`) and the two
design studies `fabledocs/design/study-eye-level-answer.md` and
`fabledocs/design/study-call-profiles.md`. Where this file and a study differ,
this file wins.

## 0. Non-negotiables (from docs/SPEC.md, docs/DEVELOPMENT.md, docs/adr/)

- Pinned prompt strings in `core/src/llm/prompt.rs` (ROLE_INSTRUCTIONS, RESUME_HEADER, JD_HEADER, GROUNDING_NOTE, STYLE_*, user wrapper) do NOT change. New sections are ADDED. An interview profile with empty focus/extra must build a cached prefix byte-identical to v3.
- The cached prefix must stay byte-stable and deterministic (ADR 007): no clocks, no HashMap iteration order, no environment reads anywhere on the path from settings to prompt.
- Every UI string that docs/SPEC.md §9 pins verbatim (status lines, placeholders, tags, "X.Xs to first word", the hotkey-taken notice, "History cleared", "Answer copied to clipboard", "Saved ✓", button labels Record/Starting…/Stop & Answer/Regenerate/Copy/Ask/Save/Back) stays byte-identical. New strings are new elements.
- Accessibility contract (UX-12): live regions (`role="status"` StatusLine, the two `sr-only` `role="status"` spans, `aria-live="polite"` + `aria-busy` on `.answer-body`) stay permanently mounted with only their TEXT changing; `role="alert"` ErrorBox; focus returns (Clear → Record, Settings close → gear, Settings open → heading); Escape closes a CLEAN Settings form; `aria-pressed` on chips mirrors the PERSISTED value never the click; `prefers-reduced-motion`; AA contrast.
- Settings file: per-field fallback, atomic write, disk-then-cache, keys write-only across IPC, empty hotkey = disabled and never springs back.
- Key material never reaches the webview or any log. Profile text is never logged.
- No `dangerouslySetInnerHTML`; markdown renderer untouched.
- Tests: every new behavior gets a test; existing tests may be amended only where this design changes behavior on purpose, and each amendment is listed in the work package's report. Nothing touches the network or an audio device.

## 1. IPC contract

TypeScript: `src/types.ts` (final). Rust must produce/accept these wire shapes (serde `rename_all = "camelCase"`).

```
SettingsView {
  profiles: CallProfile[]           // never empty, <= 8
  activeProfileId: string           // always names an entry of profiles
  alwaysOnTop: bool
  llmProvider: "anthropic"|"groq"|"local"
  answerStyle: "brief"|"balanced"|"detailed"
  hotkey: string
  launchPlacement: "remembered"|"camera"     // default "camera"
  streamFollow: "tail"|"top"                 // default "tail"
  hasDeepgramKey, hasAnthropicKey, hasGroqKey: bool
}
CallProfile { id, name, callType: "interview"|"sales"|"support"|"meeting"|"other", resume, jobDescription, focus, extraInstructions }   // all strings
SettingsPatch { profiles?, activeProfileId?, alwaysOnTop?, llmProvider?, answerStyle?, hotkey?, launchPlacement?, streamFollow?, deepgramKey?, anthropicKey?, groqKey? }
```

- `SettingsPatch.llmProvider/answerStyle/launchPlacement/streamFollow` are typed ENUMS on the Rust side (`Option<LlmProviderKind>` etc.). An invalid wire value fails deserialization → the invoke rejects → `bridge.call()` folds it into `{code:'internal'}`. `parse_or_default` stays for the untrusted settings FILE only.
- `SettingsPatch.profiles` deserializes through a lenient `CallProfilePatch` (every field `#[serde(default)]`, `callType` as String → `CallType::parse_or_default`) so a partial object never fails the whole patch.
- Commands (all return `Envelope`): `get_settings`, `set_settings(patch)` [async], `start_session`, `stop_session(sessionId)`, `ask(text)`, `cancel_session(sessionId)`, `hotkey_status` → HotkeyStatus, `dock_to_camera` → null, `local_voice_status` → LocalVoiceStatus, `prepare_local_voice` → LocalVoiceStatus. `open_external` is REMOVED (dead; https navigation is already bounced by `on_navigation`).
- `Bridge` (src/bridge.ts) gains `dockToCamera()`, `localVoiceStatus()`, `prepareLocalVoice()`; the module-level `checkLocalVoice/prepareLocalVoice` functions are removed and LocalVoicePanel uses `getBridge()`.
- Events unchanged.

## 2. Disk JSON (only shape ever written)

```
{ "profiles": [ {id,name,callType,resume,jobDescription,focus,extraInstructions}, ... ],
  "activeProfileId": "default",
  "alwaysOnTop": true, "llmProvider": "anthropic", "answerStyle": "balanced", "hotkey": "Ctrl+Shift+Space",
  "launchPlacement": "camera", "streamFollow": "tail",
  "deepgramKey": "enc:...", "anthropicKey": "enc:...", "groqKey": "enc:...",   // only when set
  "windowBounds": {x,y,width,height} }
```

Migration: if `profiles` is not a JSON array (v3 flat file, missing, or corrupt) → ONE profile `{id:"default", name:"Default", callType:"interview", resume: <top-level "resume" via profile_field>, jobDescription: <top-level "jobDescription">}`; `activeProfileId` = "default". Top-level `resume`/`jobDescription` are never written again. Keys, hotkey, bounds untouched by migration.

## 3. WP-CORE-1 — profiles, prompt, new settings fields, dock math (crate `app-core` only)

Files: `core/src/llm/prompt.rs`, `core/src/llm/mod.rs`, `core/src/store/mod.rs`, `core/src/store/settings.rs`, `core/src/store/bounds.rs`, `core/tests/local_settings.rs`, plus the `Profile {..}` literals in `core/src/llm/{anthropic,groq,local}.rs` tests.

### 3.1 prompt.rs (additive)
- `pub enum CallType { #[default] Interview, Sales, Support, Meeting, Other }` with `as_str()` / `parse_or_default()` (unknown → Interview), serde lowercase. Re-export from `llm`.
- `Profile<'a>` gains `call_type: CallType, focus: &'a str, extra_instructions: &'a str` (still `Copy + Default`).
- New constants, EXACT text:
  - `CALL_TYPE_SALES = "\n\nThis is a sales call: the user is selling to the other person. Answer as the user speaking to a prospect or customer — specific, helpful, and never pushy."`
  - `CALL_TYPE_SUPPORT = "\n\nThis is a customer support call: the user is helping the other person. Answer as the user speaking to a customer — calm, clear, and focused on resolving their issue."`
  - `CALL_TYPE_MEETING = "\n\nThis is a work meeting: the user is a participant, not a candidate. Answer as the user speaking to colleagues — direct and to the point."`
  - `CALL_TYPE_OTHER = "\n\nThis is a general call, not a job interview. Answer as the user speaking to the other person."`
  - `BACKGROUND_HEADER = "\n\n--- ABOUT THE USER ---\n"`
  - `CONTEXT_HEADER = "\n\n--- CONTEXT FOR THIS CALL ---\n"`
  - `GROUNDING_NOTE_CALL = "\n\nGround every answer in the background and call context above. Never invent experience or facts the background does not support."`
  - `FOCUS_HEADER = "\n\n--- WHAT TO EMPHASIZE ---\n"`
  - `EXTRA_INSTRUCTIONS_HEADER = "\n\n--- ADDITIONAL INSTRUCTIONS FROM THE USER ---\n"`
- `sections_for(call_type)` → Interview: `("", RESUME_HEADER, JD_HEADER, GROUNDING_NOTE)`; every other type: `(its CALL_TYPE_* line, BACKGROUND_HEADER, CONTEXT_HEADER, GROUNDING_NOTE_CALL)`.
- cached_prefix order: ROLE_INSTRUCTIONS · call line (interview: nothing) · [resume header + trimmed resume] · [jd header + trimmed JD] · [grounding note iff resume or JD non-empty — focus alone does NOT trigger it] · [FOCUS_HEADER + trimmed focus] · [EXTRA_INSTRUCTIONS_HEADER + trimmed extra]. style_suffix and user message unchanged.
- Tests to add (names as given in the profiles study §8): `migrated_v3_profile_yields_a_byte_identical_prefix`, `call_type_lines_and_new_headers_are_verbatim`, `interview_emits_no_call_type_line`, `non_interview_uses_background_context_headers_and_call_grounding_note`, `sections_appear_in_call_resume_jd_grounding_focus_extra_order`, `whitespace_only_focus_and_extra_count_as_absent`, `focus_alone_does_not_trigger_the_grounding_note`, `unknown_call_type_falls_back_to_interview`; extend `style_lives_outside_the_cached_prefix` and `prompt_is_byte_stable_across_repeated_builds` with a full Sales profile.

### 3.2 store/mod.rs + store/settings.rs
- Constants: `MAX_PROFILES = 8`, `MAX_PROFILE_NAME_CHARS = 60`, `MAX_PROFILE_ID_CHARS = 40`, `MAX_FOCUS_CHARS = 2_000`, `MAX_EXTRA_INSTRUCTIONS_CHARS = 2_000`, `DEFAULT_PROFILE_ID = "default"`, `DEFAULT_PROFILE_NAME = "Default"`, `UNNAMED_PROFILE_NAME = "Untitled"`. `MAX_PROFILE_CHARS` unchanged (per resume / JD).
- `pub struct CallProfile { id, name, call_type: CallType, resume, job_description, focus, extra_instructions }` (Serialize+Deserialize camelCase), `CallProfile::empty(id, name)`, `CallProfile::as_prompt(&self) -> Profile<'_>` (borrows).
- `pub enum LaunchPlacement { Remembered, #[default] Camera }` and `pub enum StreamFollow { #[default] Tail, Top }`, both with `as_str()`/`parse_or_default()`, serde lowercase, in `store/mod.rs`.
- `Settings`: remove `resume`/`job_description`; add `profiles: Vec<CallProfile>`, `active_profile_id: String`, `launch_placement`, `stream_follow`. Default = one empty Default profile, active "default", Camera, Tail. `Settings::active_profile(&self) -> &CallProfile` (by id → first → a `static EMPTY_PROFILE`, never panics).
- `SettingsView` mirrors §1. `SettingsPatch`: `profiles: Option<Vec<CallProfilePatch>>`, `active_profile_id: Option<String>`, `llm_provider: Option<LlmProviderKind>`, `answer_style: Option<AnswerStyle>`, `launch_placement: Option<LaunchPlacement>`, `stream_follow: Option<StreamFollow>`, rest unchanged.
- `pub fn normalize_profiles(profiles, requested: Option<&str>, previous: Option<&str>) -> (Vec<CallProfile>, String)` — the ONLY writer of the invariant: truncate to 8; empty → push Default; name trimmed+capped, "" → "Untitled"; resume/JD capped VERBATIM (chars); focus/extra capped; invalid (`[A-Za-z0-9_-]{1,40}`) or duplicate id → smallest unused `p<n>`; active = requested if known, else previous if known, else profiles[0].id. Pure and deterministic (pin with a run-twice equality test).
- `settings_from_disk`: profiles per §2 (per-field fallback INSIDE a profile: bad callType → interview, bad string → ""; a non-object entry is dropped), `launchPlacement`/`streamFollow` via parse_or_default with defaults Camera/Tail. `to_disk_json` writes the §2 shape. `apply_patch`: profiles/active handling per the study §3 (a switch to an unknown id keeps the current active); enum fields assigned directly (no trim/parse).
- Tests: the list in the profiles study §8 (store) — migration, rewrite-in-new-shape-only, round trip, per-field fallback inside a profile, dropped non-object entries, corrupt `profiles` value → default profile with keys intact, empty array, >8 truncation, id repair determinism, unknown active on load → first, unknown active on patch → unchanged, switch patch changes only active id, caps on load and patch (chars not bytes), `active_profile` fallbacks; plus `unknown_launch_placement_falls_back_to_camera`, `unknown_stream_follow_falls_back_to_tail`, round trips for both, and the corrupt-field matrix extended to the new fields. Adapt existing tests that referenced `settings.resume` to `active_profile().resume`. `core/tests/local_settings.rs` uses enum patch values.

### 3.3 store/bounds.rs — dock math (pure, physical pixels)
- `pub const DOCK_TOP_MARGIN: u32 = 8; DOCK_WIDTH_LOGICAL: u32 = 600; DOCK_MAX_HEIGHT_LOGICAL: u32 = 720; DOCK_HEIGHT_FRACTION: f64 = 0.45;`
- `pub fn dock_top_center(work: WorkArea, outer_width: u32) -> (i32, i32)`: x = work.x + max(0, work.width − outer_width)/2 (i64 math, clamp to i32), y = work.y + DOCK_TOP_MARGIN. A window wider than the display hugs its left edge.
- `pub fn dock_preset_size(work: WorkArea, scale: f64) -> (u32, u32)` (INNER size): width = min(round(600·scale), work.width) ≥ 1; height = round(work.height·0.45) clamped to [round(MIN_HEIGHT·scale), round(720·scale)] then min(work.height − margin) ≥ 1; non-finite/≤0 scale → 1.0.
- `SanitizedBounds` gains `pub from_saved: bool` (false for the fallback) so the shell can skip the physical `set_size` on first run (RS-7: the builder's logical 460×700 is right; a physical 460×700 is wrong on hi-DPI). Existing tests updated for the new field.
- Doc-comment on MIN_WIDTH/MIN_HEIGHT explaining they are the logical minimum at 100% scale (DOCK-4, option A; do NOT add a scale parameter to `sanitize_bounds`).
- Tests: the six in the eye-level study §F plus `fallback_is_not_from_saved` / `saved_bounds_are_from_saved`.

## 4. WP-CORE-2 — small core latency/robustness fixes (after CORE-1)

- RS-1: remove `deps.llm.prewarm()` from `SessionManager::ask` in `core/src/session/machine.rs` (keep Record/Stop/auto-stop warms); update the pinned test `prewarm_fires_on_start_stop_and_ask` (count reaches 2, not 3, at ask; rename to `prewarm_fires_on_start_and_stop_not_ask`).
- RS-6 (as amended by its verifier): `core/src/llm/http.rs` shared client gains `.tcp_keepalive(Some(Duration::from_secs(20)))` AND `.tcp_keepalive_interval(Some(Duration::from_secs(1)))` (socket2 passes an unset interval as 0 on Windows, so set it explicitly) plus `.connect_timeout(Duration::from_secs(3))` (bounds only the TCP/TLS connect). Leave `POOL_IDLE_TIMEOUT` at 120 s. No whole-request timeout (the body streams). Keep singleton tests green.
- R5 (core half): `impl LlmProviderKind { pub fn needs_cloud_keys(self) -> bool; pub fn uses_deepgram(self) -> bool }` (local → false/false). Add `pub struct AnswerLimits { first_token: Duration, total: Duration }` in `session/mod.rs` with `pub const CLOUD` / `pub const LOCAL` built from the existing `limits::*` constants, and a default trait method `fn answer_limits(&self) -> AnswerLimits { AnswerLimits::CLOUD }` on `LlmProvider` that `LocalProvider` overrides; `machine.rs` uses `llm.answer_limits()` instead of `kind() == Local`.
- NOT in scope (verifier refuted RS-4): no `default_origin()` helper and no launch-time pre-warm — the pooled socket is reaped after the idle timeout long before a typical first question, and an unauthenticated request at every launch is a privacy cost with no measured gain.
- PROF-08: `core/src/llm/local.rs` oversize message becomes exactly: `"Free local mode supports about 7 KB of combined instructions, profile (resume, job description, focus, extra instructions) and question. Shorten the active profile in Settings or use a cloud model."` and pin it in the existing local.rs test.
- Gate: `cargo test -p app-core` and `cargo clippy -p app-core -- -D warnings` green (run from `src-tauri/`).

## 5. WP-SHELL — Tauri shell (`src-tauri/src/**`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`), after CORE-2

- window.rs: `pub enum DockSize { Preset, Keep }`; `fn work_area_of(&Monitor) -> WorkArea` (reuse in `current_work_areas`); `pub fn dock_to_camera(window: &WebviewWindow, size: DockSize) -> Result<(), AppError>` exactly as in the eye-level study code sketch (measure the frame delta BEFORE resizing; current monitor else primary else error "Could not find the display this window is on."). `restore_geometry`: apply `set_size` only when `sanitized.from_saved`; when the sanitizer yields no position, call `dock_to_camera(window, DockSize::Keep)` instead of `window.center()` (fallback = camera level; centre only if docking itself fails).
- lib.rs: remove the builder `.center()` (positioning is owned by `restore_geometry`, which runs before `show()`); after `restore_geometry`, `if startup.launch_placement == LaunchPlacement::Camera { let _ = window::dock_to_camera(&win, DockSize::Keep); }` (pre-show); register `commands::dock_to_camera`; remove `commands::open_external`; group `local_voice::*` handlers after `commands::*`. NO launch-time pre-warm (RS-4 was refuted — see §4). RS-9: `#[cfg(debug_assertions)]` stage timings via `eprintln!` in setup (settings load, window build, restore, protect, show) — never into crash.log.
- commands.rs: `dock_to_camera` command (sync, `DockSize::Preset`); `set_settings` becomes `async`, runs lock+`apply_patch` inside `tokio::task::spawn_blocking` so the fsync'd write leaves the event-loop thread; the hotkey re-registration after a hotkey change is done via `app.run_on_main_thread` + a oneshot (belt and braces: the pinned global-shortcut plugin does not strictly require the main thread, but registering from the thread that owns the plugin's hidden window is the documented-safe path on Windows) before writing `state.hotkey`; `set_always_on_top` can be called from any thread; if `launch_placement` changed to Camera, dock with `DockSize::Keep`. RS-2: `start_session_inner` runs `WasapiLoopbackCapture.start(sink)` inside `tokio::task::spawn_blocking`; a JoinError folds into the existing cancel path as `AppError::internal("The audio capture thread failed to start. Try restarting the app.")` so every failure mode still cancels the machine session; keep the `is_active` re-check after the await. `build_deps` uses `settings.active_profile().as_prompt()`; `required_llm_key`/deepgram checks use `needs_cloud_keys()`/`uses_deepgram()`; replace `.get().clone()` with `.get()`. Remove `open_external`.
- local_voice.rs (R11): split into pure functions with unit tests — `parse_tags(&Value) -> (bool, bool)`, `parse_health(&Value) -> bool`, `parse_config(&str) -> Result<Config, AppError>` (BOM strip, relative-path rejection), and a launch-plan function — keep I/O in thin async wrappers; keep every user-facing string verbatim. Name the speech port and the config path as constants shared with `core/src/stt/local.rs` where practical.
- Gate: `cargo test -p app-core` still green; for the shell run `cargo check -p aicallhelper` from `src-tauri/` (the full shell build may not fit on this disk — C: has ~3 GB free; if `cargo check` fails with "not enough space", report exactly that and stop; do not delete anything). Note: cargo serializes on the target-dir lock, so never run two cargo commands concurrently.

## 6. WP-FE-1 — frontend features (`src/**` except the perf items in §7)

Read `src/types.ts` first. Then implement, keeping `npm run typecheck` green at the end and every test file you touch green (`npx vitest run <file>`); run the full `npm test` once at the end (it takes ~6 minutes on this machine — that is normal).

### 6.1 Bridge / App
- `src/bridge.ts`: add `dockToCamera`, `localVoiceStatus`, `prepareLocalVoice` to `Bridge` and the Tauri impl; delete the module-level `checkLocalVoice/prepareLocalVoice`; `hotkeyStatus()` returns `Envelope<HotkeyStatus>` from types.ts. Delete `src/components/hotkey.ts`; import `HotkeyStatus` from `../types` everywhere.
- `src/App.tsx`: (R4) delete `useHotkeyGate`/`gatedBridges`/`setBridge` usage; `useSession({ hotkeyEnabled: () => !settingsOpenRef.current })` — implement the option in `useSession` (default enabled). Add `selectProfile = (id) => applySettings({ activeProfileId: id })`, `dockToCamera` (calls the bridge; a refusal goes to `session.setError`), `focusMode` state (boolean, default false, not persisted) with `toggleFocus`. Open Settings inside `startTransition` (FE-3). Pass `onSelectProfile`, `onDock`, `focusMode`, `onToggleFocus` to MainView.
- StatusLine gets `firstRun: boolean | null`; while `settings == null` MainView passes `null` and StatusLine renders an empty string (no wrong first frame; the `min-height` keeps the row). All pinned strings unchanged once settings load.

### 6.2 MainView — answer-first layout (DOM order, top to bottom)
1. `<header className="app-header">`: status dot · `<h1>` title · `<button aria-label="Focus mode" aria-pressed={focusMode} title="Show only the answer (Ctrl+Shift+F)">` · `<button aria-label="Dock to camera" title="Move this window to the top of the screen, under the webcam">` (enabled even while settings are null) · gear (unchanged, disabled until settings load).
2. `<ProfileChips>` (new component, StyleChips pattern; renders nothing with < 2 profiles; `role="group" aria-label="Call profile"`; `aria-pressed` mirrors `settings.activeProfileId`; click on the active chip is a no-op; failed save → `session.setError`). Hidden in focus mode.
3. `<AnswerPanel>` — flex-grow reading surface. New props: `historyLength`, `viewIndex`, `idle`, `onPrev`, `onNext`, `onClear` (primitives + stable callbacks; keep `memo`) so `<HistoryBar>` renders INSIDE the panel head (right slot, before Regenerate/Copy). `HistoryBar` component and its tests stay as-is (same roles/names: `nav aria-label="Answer history"`, "Previous answer", "Next answer", "n/m", "Clear"; hidden until 2 entries; Clear enabled only when idle). MainView's `clearHistory` wrapper becomes a `useCallback`, still announces "History cleared" via the sr-only span and focuses the Record button. Also pass `streamFollow` (from settings, default 'tail'); implement 'top' by SEEDING `nearBottomRef` (mount: `follow === 'tail'`; `resetScrollForNewEntry`: `follow === 'tail' && fits`) — the stick branch itself is unchanged, so every existing autoscroll test stays green; add tests for 'top'. Font: `.answer-body` 16px / line-height 1.5 (18px in focus mode).
4. `<ErrorBox>` (first child of the reading area, directly under the answer).
5. `<StatusLine>`.
6. Control row A (`.control-row`): compact `<RecordButton>` (auto width, still the accent fill, same label cycle and kbd chip) + `<StyleChips>` as a segmented control. The hotkey-taken notice stays right after the Record button. A `.recording-row` container is ALWAYS rendered with a reserved `min-height` so nothing shifts; `<LevelMeter>` and the timer mount only while recording (tests check the meter is absent when idle).
7. Row B: `<AskForm>` (gets `showPrep` prop; MainView passes `activeProfile.callType === 'interview'`; when false AskForm hides the Interview-prep toggle and closes an open library). Under it, a one-line tip `<p className="field-help hint">` "Tip: press {formatHotkey(hotkey.accelerator)} from the meeting window — the answer streams here." shown only while `history.length === 0 && hotkey?.registered` — and an "ungrounded profile" hint `No resume or job description saved for {name} — answers won't be grounded. Add them in Settings.` shown when the active profile has neither and `hasRequiredKeys(settings)`.
8. Local-mode banner (only when `llmProvider === 'local'`; exact strings unchanged; `aria-label="Current mode"`).
9. `<TranscriptPanel>` — compact strip that auto-collapses: expanded while `starting|recording|finalizing`, or when idle with an empty question (so the pinned idle placeholder stays visible), or when the user toggled it open; collapsed otherwise showing a one-line ellipsised caption of the question. The head is a `<div className="panel-head">` holding `<h2 className="panel-title"><button aria-expanded aria-controls="transcript-body">Question heard</button></h2>`, with the `live` tag and the ellipsised caption as siblings of the h2 (standard disclosure pattern: a heading nested inside a button leaves the accessibility heading list, and the caption would become the button's name); body `id="transcript-body"` `hidden` when collapsed. Placeholders/"Listening…"/"live" strings unchanged.
10. sr-only `role="status"` announcement span (permanently mounted).
Focus mode (`.main-view--focus`): hides ProfileChips, control rows, AskForm + hints, local banner, TranscriptPanel via the `hidden` attribute (elements stay mounted); keeps header, AnswerPanel (18px), ErrorBox, StatusLine, and the Record button (rendered compact in the header's left of the icons? — no: keep it simple, keep control row A visible in focus mode too, only hide row B, hints, chips, banner, transcript). In-window `Ctrl+Shift+F` toggles focus (window keydown, ignored when the target is an input/textarea/select); it is NOT a global shortcut.
- Tests (MainView.test.tsx, App.test.tsx, AnswerPanel.test.tsx, TranscriptPanel cases): dock button calls `onDock` and works with `settings === null`; focus toggle hides Ask/transcript (`queryByRole` null) and keeps Record/status/answer; profile chips hidden with one profile, shown with two, pressed mirrors `activeProfileId`, click sends ONLY `{ activeProfileId }` through the bridge (App test), failed save shows in the error box; transcript collapses after recording ends and expands on toggle; history nav inside the answer panel still navigates and Clear focuses Record; ungrounded hint and hotkey tip appear/disappear; StatusLine empty while settings are null. Existing string/role assertions stay.

### 6.3 SettingsView
- Draft state: `profiles` (copied from props), `selectedId` (seeded from `activeProfileId`). Layout in three `<fieldset className="settings-section"><legend>`: **Keys & model** (provider select derived from `PROVIDER_ORDER`/`PROVIDERS` labels; then only the key fields the provider needs, hidden via the `hidden` attribute; `<LocalVoicePanel/>` for local), **Profile** (`<select id="set-profile">` label "Profile" + New / Duplicate / Delete buttons; New disabled at MAX_PROFILES, Delete disabled with one profile; fields for the selected profile: "Profile name" (maxLength 60), "Call type" select from `CALL_TYPES`, "Focus (optional)" (placeholder "What to emphasize, e.g. Rust, tokio, async"), resume textarea labelled "Resume" for interview / "About you (optional)" otherwise, JD textarea labelled "Job description" for interview / "Call context (account, product, agenda)" otherwise, "Extra instructions (optional)" textarea rows=2; textareas rows=10 with a `field-help` character counter `n / 200,000 characters`; when provider is local also a UTF-8 byte counter of the profile text with a warning above 6,500 bytes referencing LOCAL_MAX_INPUT_BYTES), **Shortcut & window** (hotkey with placeholder `DEFAULT_HOTKEY` and help "Modifier+key combination, e.g. Ctrl+Shift+Space. Works while any app is focused. Leave empty to turn the shortcut off. Applied when you save."; always-on-top checkbox with help "Keeps the assistant above your call window so you can read answers while the meeting is focused."; `Window position at launch` select `remembered` → "Remember where I left it", `camera` → "Dock under the camera (top centre)" with help "Docked windows sit at the top of the screen, so reading the answer looks like eye contact with the webcam. The ⬆ button on the main screen docks at any time."; `While an answer streams` select `tail` → "Follow the newest text", `top` → "Stay at the opening sentence (teleprompter)").
- Key fields rendered from a `KEY_FIELDS` table (same ids/labels as today: `set-deepgram` "Deepgram API key", `set-anthropic` "Anthropic API key", `set-groq` "Groq API key (only for the Groq preset)"), state `Partial<Record<'deepgramKey'|'anthropicKey'|'groqKey', string>>`; untouched = omitted, '' = clear (unchanged rule).
- Save sends `{ profiles, activeProfileId: selectedId, alwaysOnTop, llmProvider, answerStyle, hotkey, launchPlacement, streamFollow }` + touched keys; on success re-seed every field from the returned view (ids may be repaired, hotkey trimmed), reset key drafts, show "Saved ✓".
- Sticky actions: `.settings-view { height: 100vh }`, `.settings-form { flex: 1; min-height: 0; overflow-y: auto }`, `.settings-actions { position: sticky; bottom: 0; ... }`.
- Dirty guard: `dirty` = any field differs from the seeded props or a key was typed; Escape/Back while dirty shows an inline `<div role="alertdialog" aria-label="Unsaved changes">` "Discard unsaved changes?" with "Discard" / "Keep editing" instead of closing; a clean form still closes on the first Escape (existing test stays green).
- New profile ids: `crypto.randomUUID()` with a `p-<base36 time><random>` fallback; the core repairs anything invalid.
- Tests: edits only the selected profile and sends the whole array + selected id; New/Duplicate/Delete behaviors and disabled states; labels switch with call type; launch placement and stream follow sent; hotkey placeholder equals DEFAULT_HOTKEY; local mode hides the three key `.field` wrappers (assert the `hidden` attribute) and shows them again; dirty guard blocks Escape then Discard closes; the existing "omits untouched key fields but always sends the rest of the form" asserts the new fields.
- Move Settings-only CSS out of `styles.css` into a new `src/views/SettingsView.css` imported only by SettingsView (so it code-splits); move `.local-mode-banner*` rules into `styles.css`; `LocalVoicePanel.tsx` imports its own CSS; add a global `[hidden] { display: none !important }` reset in styles.css; delete the CSS imports from MainView/SettingsView; replace hex literals in LocalVoicePanel.css with tokens (add `--ok`, `--ok-bg`, `--ok-border`).
- `src/views/testUtils.tsx`: `baseSettings` gets `profiles: [{ id: 'default', name: 'Default', callType: 'interview', resume: 'Senior engineer, 8 years.', jobDescription: 'Backend role at Initech.', focus: '', extraInstructions: '' }], activeProfileId: 'default', launchPlacement: 'camera', streamFollow: 'tail', hotkey: 'Ctrl+Shift+Space'`; FakeBridge gains `dockToCamera`, `localVoiceStatus`, `prepareLocalVoice` (vi.fn returning ok). `LocalMode.test.tsx` drives LocalVoicePanel through FakeBridge, not `vi.spyOn`.

## 7. WP-FE-2 — frontend performance and refactors (after FE-1)

- FE-1 (delta coalescing) in `src/state/useSession.ts`: leading+trailing coalescer for `llm:delta` — first delta dispatches synchronously, later deltas within 16 ms are merged (per sessionId) and flushed by `setTimeout` (not rAF: WebView2 throttles rAF when minimized); flush synchronously at the top of `stt:partial`, `llm:done`, `session:error`, `hotkey:toggle` handlers and in the effect cleanup so event ORDER is preserved. Tests: second delta within 16 ms merged, first paints immediately, error after a buffered delta keeps the text, done after buffered delta yields the full answer; adjust existing tests that emit two deltas without a terminal event (use fake timers or the flush).
- FE-3: `AskForm` preloads the PracticeLibrary chunk on pointerenter/focus of the toggle and opens it inside `startTransition`; App preloads both lazy chunks 1500 ms after mount (cleared on unmount).
- NOT in scope (verifier refuted FE-4): do not wrap StatusLine/RecordButton/StyleChips/HistoryBar/ErrorBox in `React.memo` or extract a RecordingRow just to dodge the 12 Hz recording re-render — several of their props are not referentially stable, so the memo checks would be net churn for a tree this small. Keep the existing memo on AnswerPanel/TranscriptPanel/AskForm.
- FE-6/FE-7: `vite.config.ts` build `{ target: 'chrome120', modulePreload: { polyfill: false }, reportCompressedSize: false, sourcemap: false }`; keep cssCodeSplit; verify `npm run build` still emits the SettingsView and PracticeLibrary chunks plus a SettingsView CSS chunk.
- R7: `src/components/useTransient.ts` — `useTransient<T>(idle, ms)` and `useAnnouncer(ms = 1500)`; replace the four hand-rolled timer sites (MainView announcement, AnswerPanel copied + announcement, SettingsView saved note).
- R9: extract `useFrameCoalesced(source, streaming, entryKey)` from AnswerPanel; give the entry-switch effect `[entryKey]` deps; hoist `resetScrollForNewEntry` into a `useCallback` declared above both effects. All AnswerPanel and markdown streaming tests stay green.
- R10: `npm uninstall @tauri-apps/plugin-global-shortcut` (unused in src/; the Rust plugin is what the app uses). Keep package-lock consistent.
- Flaky test: `src/components/AskForm.test.tsx` — the lazy chunk can take > 1 s to resolve on a cold jsdom; give every `findBy*` that waits for the PracticeLibrary a `{ timeout: 10_000 }` (or preload the chunk in a `beforeAll`).
- Gate: `npm run typecheck`, `npm test` (full), `npm run build` all green.

## 8. WP-DOCS (after everything compiles and tests pass)

- docs/SPEC.md: §1 providers incl. local; §3 timeouts note for local (90 s / 300 s) and prewarm-is-a-no-op for local; §4 full command list (incl. `hotkey_status`, `dock_to_camera`, `local_voice_status`, `prepare_local_voice`; `open_external` removed); §6.4 prewarm sites (Record, Stop, auto-stop, launch; NOT Ask) + connect timeout/keepalive; new §6.5 Local (Ollama + Moonshine) per DOC-02; §7 new optional sections + call-type header set, byte-identity of the interview profile, per-profile cache note; §8 fields (profiles, activeProfileId, launchPlacement, streamFollow, llmProvider incl. local), caps, migration, normalize rules, "a switch never rewrites text", window bounds + dock rule (RS-7 first-run logical size; fallback docks); §9 window paragraph (dock, launch placement, default 460×700 logical), main view order (the §6.2 list), focus mode, transcript auto-collapse, history nav in the answer head, settings view sections, profile chips, stream-follow option (default tail); keep every pinned string.
- docs/TESTING.md: bullets for every new test (Rust + TS) in the existing "— what; **why**: failure mode" shape, corrected counts (compute them: `grep -rcE "^\s*#\[(tokio::)?test" src-tauri/core/src` etc., and the vitest total from a real run), sections for the previously undocumented files (llm/local.rs, stt/local.rs, AskForm.test, LocalMode.test, commands.rs count), hotkey.rs/local_voice.rs status.
- docs/ARCHITECTURE.md, docs/DEVELOPMENT.md (IPC table, threading row for async set_settings + main-thread hotkey hop, "Adding another LLM provider" keyless note, prompt volatility rule per profile, StrictMode dev-doubling note), docs/SECURITY.md (assets: profiles plaintext; local origins; open_external removed), docs/TROUBLESHOOTING.md (local-mode section; dock/placement note; timeouts), docs/IDEAS.md (privacy line: three cloud origins + two loopback origins in local mode; anti-goal clarification "profiles are grounding data, not personas"; Don't build: auto-detected call type, per-question profile switching), docs/FREE_VOICE_MODE.md (7 KB cap applies to the active profile incl. focus/extra), docs/adr/README.md + new `013-window-placement-dock-to-camera.md` and `014-call-profiles.md` (ADR 007 gets a Consequences sentence about profile switch = cache write; ADR 005 gets a Scope paragraph about the local client), README.md (setup step 4 → profiles; architecture tree incl. local files; manual QA steps for dock/profiles/focus; test counts pointer).
- Test counts live only in TESTING.md; the other docs say "(count in TESTING.md)".

## 9. Verification matrix (what "done" means)

| Gate | Command (from) | Expectation |
|---|---|---|
| core tests | `cargo test -p app-core` (src-tauri/) | all green |
| core lint | `cargo clippy -p app-core -- -D warnings` (src-tauri/) | clean |
| shell compile | `cargo check -p aicallhelper` (src-tauri/) | clean, or an honest "disk full" report |
| typecheck | `npm run typecheck` | clean |
| frontend tests | `npm test` | all green (≈6 min) |
| bundle | `npm run build` | index + SettingsView(+css) + PracticeLibrary(+css) chunks |

## 10. Explicitly not built (and why)

Auto-detecting call type from the transcript (a wrong guess grounds the answer in the wrong JD, and it works inside the latency window); per-question profile switching (a cache write per question); per-profile default answer style (second source of truth for the chips); free-text call types (unpinnable prompt text); cloud sync / CRM / JD-from-URL; a frameless title bar; a second global hotkey; auto-docking mid-call; preact/compat (timing-sensitive layout effects, unmeasured gain); eager per-event listener fan-out in the bridge (FE-5: tiny window, semantic change to the tested dispose race); dropping the outer settings mutex (R1 part 2) and reordering setup to overlap settings load with window creation (RS-5): small, risky, unmeasured; a launch-time LLM pre-warm (RS-4, refuted: the pooled socket is reaped before a typical first question, and it is an unauthenticated request at every launch); memo-wrapping the small static main-view components (FE-4, refuted: their props are not referentially stable, so the memo checks are net churn).

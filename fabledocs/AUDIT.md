# Audit — AI Call Assistant v3 (2026-09-18)

> **Historical material (2026-09-18).** The audit that preceded v3.1, kept as decision history. "Built" below means implemented and covered by tests at that time, not validated on hardware; hardware validation is recorded only in `docs/RELEASE_CHECKLIST.md`. Current status is in `README.md` in this folder.

This is the audit that preceded the v3.1 changes. `REPORT.md` lists what was
actually changed and how it was verified; `DESIGN.md` is the contract the
implementation followed. The raw finder output, including every verifier
verdict that completed, is in `design/audit-findings-raw.md`.

## Method

Seven read-only finders ran in parallel, each with one lens: frontend loading
time, Rust startup and pipeline latency, live-call usability, code quality,
documentation drift, and two design studies (the "answer at eye level"
request, and the "profiles per tech stack / call type" question). Every
finding was then handed to an independent skeptic told to refute it by reading
the cited code. 32 of 42 verifications completed before the session's usage
limit paused the run; the remaining ten (six Rust-startup items and four
low-priority frontend items) were judged by hand from the finder evidence.

Baseline before any change:

| Suite | Result |
|---|---|
| Rust core (`cargo test -p app-core`) | 248 passed |
| Tauri shell (`cargo test`) | could not build: disk full (see below) |
| Frontend (`npm test`) | 299 passed, 1 failed (timing flake in `AskForm.test.tsx`: the lazy practice-library chunk took longer than the 1 s default query timeout on a cold jsdom; the suite took 325 s) |
| Bundle (`npm run build`) | main chunk 173.9 kB (56.5 kB gzip, 83 % React), SettingsView 7.7 kB lazy, PracticeLibrary 6.0 kB + 1.5 kB CSS lazy, main CSS 8.2 kB |

## The machine, not the app: the disk is full

The C: drive held 235 GB of 238 GB when the audit started, with 92 MB free at
one point. The Tauri shell test build failed with "There is not enough space
on the disk". The repo's own regenerable Rust build output (3.5 GB) was
removed with `cargo clean` to keep working; the real consumers are outside
this project (Desktop ≈57 GB, AppData ≈49 GB, Windows ≈38 GB). Until tens of
GB are freed, `npm run tauri build` and the full shell test build cannot run
on this machine. Everything else was verified.

## Findings and decisions

Severity is the verified severity. "Built" means it is in v3.1; "deferred"
means it is worth doing but was left out on purpose; "rejected" means the
verifier refuted it or the trade-off is wrong for this app.

### Eye-level answer (the explicit request)

| # | Finding | Decision |
|---|---|---|
| UX-01 / DOCK-1 | The answer panel is the ninth block in the column; its first line lands ~415 px into a 700 px window and ~600 px down a 1080p display when centered. | Built: answer-first layout, error box directly under it, controls below. |
| UX-02 | The answer body is capped at 240 px with 14 px text while ~160 px of the column is dead space above the history bar. | Built: the panel fills the column, 16 px / 1.5 line height (18 px in focus mode), no cap. |
| UX-03 / DOCK-2 | The window launches centered; no way to put it under the webcam. | Built: pure dock math in the core, `dock_to_camera` command, header button, and the sanitizer's fallback now docks instead of centering. |
| DOCK-3 | Nothing re-docks after a monitor change or a mid-call drag. | Built: `launchPlacement` setting, default `camera` (saved size kept, saved position ignored). |
| UX-04 | The transcript panel takes prime space above the answer even after recording ends. | Built: auto-collapses to a one-line caption; expands while recording or on click. |
| UX-05 | Five rows of controls (~190–250 px) crowd the live view; the recording row mounting shifts the layout. | Built: compact Record button + segmented style chips on one row, Ask on the next, reserved recording-row height. |
| UX-06 | No teleprompter mode. | Built (frontend only): focus mode toggle in the header and Ctrl+Shift+F; hides everything but the answer, status, error and Record. |
| UX-08 | Errors render at the bottom of the column. | Built: error box is the first child of the reading area. |
| UX-10 | History navigation is at the opposite end of the window from the answer it changes. | Built: the history bar renders inside the answer panel head. |
| UX-11 | Stick-to-bottom scrolls the opening sentence away within seconds while the user is reading it aloud. | Built as an opt-in `streamFollow: top` setting; the pinned `tail` default is unchanged. |
| DOCK-4 | The bounds sanitizer clamps physical sizes to a logical minimum. | Documented only (verifier: safe because the OS clamps again). |
| DOCK-5 | Hint that docking works best with always-on-top. | Rejected by the verifier as describing UI that did not exist; the help text landed anyway with the settings redesign. |
| DOCK-6 | The local-mode banner (~95 px) sits above the output. | Built: moved below the controls. |
| UX-12 | Accessibility contract checklist. | Applied as constraints in `DESIGN.md` §0. |

### Profiles per tech stack / call type (the exploration)

Verdict: build the lean version. The single resume + job description forces a
re-paste before every call in a parallel job search, and forgetting produces
an answer confidently grounded in the wrong job description in about a
second. The heavy version (free-text personas, per-question switching,
auto-detection) would betray the product's own anti-goals and put work
inside the latency window. Full reasoning: `design/study-call-profiles.md`.

| # | Finding | Decision |
|---|---|---|
| PROF-01 | One resume/JD for every call. | Built: up to 8 named profiles with call type, focus (tech stack), resume, context/JD and extra instructions; one active; quick-switch chips on the main view; legacy file migrates into a "Default" profile; the interview profile's prompt is byte-identical to v3. |
| PROF-02 | Local mode's 7 KB cap is only enforced after Stop. | Built: byte counter and warning in Settings when the local provider is selected. |
| PROF-03 | Escape discards unsaved Settings edits silently. | Built: dirty guard with an inline Discard / Keep editing row; a clean form still closes on Escape. |
| PROF-04 / UX-07 / R2 / DOC-05 | The Settings hotkey placeholder claims to mirror the core default but spells it differently. | Built: one constant, `Ctrl+Shift+Space`, mirrored in `types.ts`. |
| PROF-05 | Interview prep shows for sales/support profiles. | Built: hidden unless the active profile is an interview. |
| PROF-06 | No hint when the active profile is empty. | Built: one-line hint under the chips. |
| PROF-07 | Every save round-trips every profile over IPC. | Deferred: local IPC, tens of KB in practice; recorded in ADR 014. |
| PROF-08 | The local-mode size error names only resume and JD. | Built: message now names the whole profile. |

### Loading time and responsiveness

| # | Finding | Decision |
|---|---|---|
| FE-1 | One reducer dispatch and one App-tree reconcile per LLM token; fast providers are punished most. | Built: deltas coalesced in the session hook (first delta immediate, later ones merged within 16 ms, order preserved against done/error). |
| FE-2 | First frame shows "Ready…" then flips to "First run…" once settings load. | Built (cheap form): the status line stays empty until settings arrive. The Rust boot-payload variant is deferred. |
| FE-3 | Suspense fallback flashes on the first open of Settings and Interview prep. | Built: `startTransition` on open, chunk preload on hover/focus and 1.5 s after launch. |
| FE-4 | Whole main view re-renders at ~12 Hz while recording. | Rejected by the verifier: props are not referentially stable, memo would be net churn for a tree this small. |
| FE-5 | Tauri `listen()` registrations happen after first paint. | Deferred: tiny window, changes the tested dispose race. |
| FE-6 / R3 | Settings-only CSS ships in the main stylesheet; MainView imports a Settings panel's CSS. | Built: Settings CSS split into its own lazy chunk; banner rules moved; global `[hidden]` reset. |
| FE-7 | Vite ships a modulepreload polyfill WebView2 never needs. | Built: `modulePreload.polyfill = false`, target chrome120, no gzip reporting. |
| FE-8 | React + ReactDOM are 83 % of the main chunk. | Deferred: preact/compat is the only lever and its timing differences are unmeasured. |
| FE-9 | StrictMode doubles dev-mode IPC. | Documented in DEVELOPMENT.md. |
| RS-1 | The Ask-path pre-warm races the answer POST for the one pooled HTTP/1.1 connection. | Built: pre-warm removed from Ask; Record/Stop warms kept. |
| RS-2 | The blocking WASAPI open parks the just-spawned Deepgram dial for ~100 ms. | Built: capture opened on the blocking pool. |
| RS-3 / R1 | `set_settings` fsyncs on the event-loop thread on every style-chip click. | Built as an async command with the write on the blocking pool (the verifier judged the severity overstated but the change is low-risk). |
| RS-4 | Pre-warm the LLM origin at launch. | Rejected by the verifier: the pooled socket is reaped before a typical first question, and it is an unauthenticated request at every launch. |
| RS-5 | Overlap settings load with WebView2 creation. | Deferred: small gain, main-thread ordering subtleties. |
| RS-6 | The shared HTTP client has no TCP keepalive or connect timeout. | Built as amended: keepalive 20 s / interval 1 s, connect timeout 3 s, idle timeout unchanged. |
| RS-7 | First run applies the logical 460×700 default as a physical size (wrong on hi-DPI). | Built: the sanitizer reports whether bounds came from disk; the physical resize is skipped otherwise. |
| RS-8 | Release profile. | No change (already right). |
| RS-9 | Redundant builder `.center()`. | Built: removed; debug-only stage timings added. |

### Code quality

| # | Finding | Decision |
|---|---|---|
| R4 | The hotkey gate rewires the global bridge from a component effect and hand-copies every method. | Built: `useSession({ hotkeyEnabled })` option. |
| R5 | The local provider is special-cased in seven places. | Built: `PROVIDERS` table in TS; `needs_cloud_keys()` / `uses_deepgram()` / `answer_limits()` in Rust. |
| R6 | Patch enums travel as loose strings with silent fallback. | Built: typed enums on the patch; parse-or-default only for the file. |
| R7 | The timer-then-clear announcement pattern is hand-rolled four times. | Built: `useTransient` / `useAnnouncer`. |
| R8 | Three identical key fields; the form never re-syncs with coerced values. | Built: table-driven key fields; the form re-seeds from the saved view. |
| R9 | AnswerPanel's dependency-less layout effect calling setState. | Built: `useFrameCoalesced` extraction with proper deps. |
| R10 | Dead `open_external` command, unused npm plugin, `HotkeyStatus` declared three times, local-voice calls bypass the bridge seam. | Built: all four. |
| R11 | `local_voice.rs` has no tests and TESTING.md is stale for the local-voice commit. | Built: pure parsing/decision functions with unit tests; TESTING.md rewritten. |
| R12 | Flat resume/JD threaded through eight files. | Superseded by profiles. |

### Documentation drift

All twelve DOC findings (local mode absent from SPEC §1/§3/§4/§6/§8/§9;
four registered commands undocumented; stale repo trees; four different test
counts; fourteen undocumented tests; "three origins" and ADR 005's singleton
claim contradicted by the local provider; the provider recipe assuming a key
and SSE) were applied in the docs pass, together with the sections the new
features required (SPEC §7/§8/§9, ADR 013 window placement, ADR 014 call
profiles).

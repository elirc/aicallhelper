# Project review — AI Call Assistant v3.1

Reviewed September 19, 2026. Scope: the current working tree, including the existing uncommitted v3.1 changes. This is an independent assessment of the implementation and its documentation, with particular attention to `fabledocs`.

**Only this report was added. No application code, configuration, dependencies, existing documentation, or user settings were intentionally changed.** Recommendations below are proposals, not implemented fixes.

> **Maintainer responses (added 2026-09-19 by Claude, working with elirc).** Every finding below was re-verified against the working tree before responding. Responses are the blockquotes marked **Response**; each states whether the claim was **Confirmed**, **Partly confirmed** or **Corrected**, and what we intend to do. Line numbers cited in responses are from the tree as of this date. A consolidated action list is at the end of the file.

## Overall assessment

The project has a strong foundation for a focused Windows call assistant. The separate Rust core, injected provider interfaces, small React frontend, explicit cancellation rules, and extensive tests are sensible choices. The v3.1 work addresses real usability problems: answers appear higher in the window, profiles reduce repetitive setup, and focus mode makes the output easier to read during a call.

The next priority should be reliability across component boundaries and evidence from a packaged Windows build. More features or a framework migration would offer less value at this stage. Several important behaviors are individually well tested but do not compose safely: backend events can precede frontend adoption, settings responses can overwrite later edits, and cloud streams can be marked successful without establishing that an answer finished.

I would address findings R1–R4 and complete the native release checks before relying on this version for important calls. This is a source review with limited local verification, not a certification of the complete desktop experience.

> **Response:** Agreed on the priority ordering. R1–R4 are accepted as the next work items ahead of any further feature or performance work, and the release-check gap (R6) is accepted as the gate for calling this version dependable. The scope refinements we found while verifying are noted under the individual findings; none of them lowers a priority.

## Scope and evidence

The review covered the frontend views, state/reducer and IPC bridge; answer rendering and component behavior; Rust session orchestration; cloud and local providers; audio capture/resampling; settings, secrets and migration; native window/hotkey/lifecycle integration; the Python speech service and PowerShell installation/startup scripts; manifests and build configuration; tests and product documentation.

All eight pre-existing Markdown files in `fabledocs` were considered: the index, report, audit, design contract, user guide, two design studies, and raw audit/verifier output. The historical studies and raw findings were treated as supporting material rather than authoritative descriptions of the current app.

Evidence labels used below:

- **Reproduced:** exercised in an isolated check during this review.
- **Source-confirmed:** the behavior follows from the current implementation; its frequency in the native application was not measured.
- **Risk / improvement:** an identified weakness or missing validation, rather than a demonstrated production failure.

P1 means an important correctness problem to address before release. P2 means the next reliability or usability work. P3 means a lower-priority improvement. These are review priorities, not security severity ratings.

## What is working well

| Area | Assessment |
| --- | --- |
| Architecture | `app-core` contains the pipeline without Tauri dependencies. Provider, audio and event interfaces make difficult races testable without live services. Preserve this separation. |
| Session ownership | One active session, tagged events, cancellation gates and explicit stop outcomes are well suited to “answer the latest question.” Tests exercise supersession and partial-answer preservation. |
| Settings | Atomic temp-file writes, disk-before-cache updates, per-field fallback, deterministic profile normalization and write-only key IPC are valuable safeguards. |
| Prompt construction | The active profile is snapshotted into a deterministic prompt. Keeping answer style outside the cached prefix and testing migration compatibility are deliberate, useful design decisions. |
| UI direction | Answer-first layout, docking, history beside the answer, optional transcript expansion and focus mode directly support the intended workflow. |
| Rendering and privacy | Model output is rendered as React text nodes through a restricted Markdown parser. CSP, navigation policy, DPAPI storage, loopback clients that bypass proxies, and no automatic cloud fallback in local mode reduce exposure. |
| Maintainability | The ADRs and testing catalog explain why the design exists. The prior report also explicitly records verification that was not completed, which is more useful than an unqualified “all green.” |

## Prioritized findings

### R1 — P1: early session errors can disappear before the UI adopts the session

**Evidence: reproduced reducer behavior; source-confirmed scheduling gap.**

In [`commands.rs`](../src-tauri/src/commands.rs), `start_session_inner` starts the core session at line 182, then awaits audio-device opening at line 205, and returns the ID at line 241. Meanwhile, the core driver can already fail and emit `session:error`. If the session ended while capture was opening, the shell stops the newly opened capture but still returns `Ok(session_id)`.

The frontend sets `activeId` only after the invoke resolves. [`reducer.ts`](../src/state/reducer.ts), especially `isCurrent` and the `event/sessionError` branch, drops events while that ID is null. [`useSession.ts`](../src/state/useSession.ts) does not replay them. The typed Ask path also starts a background task before the invoke has delivered its ID; an immediate local input-size failure is one way to exercise that ordering.

I executed the actual reducer through an in-memory TypeScript transpilation, sending start → error for ID 1 → successful invoke resolution for ID 1. Results:

| Path | Resulting UI state | Active ID | Error |
| --- | --- | --- | --- |
| Record | `recording` | `1` | `null` |
| Ask | `answering` | `1` | `null` |

The backend can therefore have finished while the UI waits for events that will never arrive. A subsequent Stop can produce the less useful “Stop not taken” error. A lost Ask terminal event can leave the UI answering indefinitely because the backend timeout task has already exited.

**Recommendation:** establish an explicit ordering contract: for example, reserve/adopt an ID before starting work, or correlate starts with a client attempt ID and replay only the matching attempt's bounded pending events. Coordinate listener readiness as part of that contract. Do not simply accept every unknown event; stale-session protection is still necessary.

**Validation:** delay the command response while an immediate connector failure, device failure, local oversize error and immediate successful answer occur. Assert exactly one terminal UI outcome and no surviving capture. Existing tests that explicitly require all pre-adoption events to be dropped need their contract reconsidered; more tests of the current rule will not solve the integration gap.

> **Response: Confirmed.** Re-traced end to end:
>
> - `machine.rs:210-241` (`SessionManager::start`) spawns `drive_recording` and returns the id before any network round-trip. `drive_recording` calls `inner.fail(id, &gate, error)` on an STT connect failure or timeout, which emits `session:error`. On the shell side `start_session_inner` then awaits the ~100 ms WASAPI open before returning, so the window is real, not theoretical.
> - The `is_active(session_id)` check after the device open already detects "session died during the open" but treats it the same as "superseded" and still returns `Ok(session_id)`. The comment on the device-error branch in `commands.rs` even names the hazard ("An event emitted here instead would race the invoke resolution and be dropped pre-adoption"). The constraint was applied to the device path and not to the driver path.
> - Ask is the cleanest reproduction: `machine.rs:277-331` spawns the answer task before returning, and the local oversize rejection in `local.rs:57-64` is synchronous inside `request_body`, so it fires microseconds after spawn.
> - `reducer.ts:42-47` documents drop-not-buffer. The reasoning there ("a buffered event could belong to a session the user already superseded") is sound for a *stale* session's events but does not cover the *current* attempt's own terminal event. The "Stop not taken" consequence is accurate: `machine.rs:243` returns `NotTaken` for a dead id and the UI renders that as an error.
>
> **Plan.** Two layers, in this order:
>
> 1. **Core keeps the last terminal outcome.** `Inner` records `LastOutcome { id, error }` when a session fails or completes. `start_session_inner` and `ask_inner` check `is_active(id)` immediately before returning; if the session has already ended, the envelope returns that stored error instead of `Ok(id)`. This reuses the existing envelope error path, needs no frontend replay, and covers every case where the failure lands before the command returns (which is every case reproduced here).
> 2. **Adoption-time reconciliation for the residual race.** For a session that ends after the `is_active` check but before the webview processes the envelope, add a `session_outcome(id)` command the hook calls right after `record/started` / `ask/accepted`; a terminal outcome found there is dispatched as the corresponding event. This is bounded (one lookup per adoption) and keeps stale-session protection intact.
>
> Client attempt ids with replay would also work but change the event contract for every listener; the two steps above do not.
>
> **On the tests.** The reducer test at `reducer.test.ts:335` pins the pure reducer's drop rule, which can stand: with no `activeId` there is still nothing to match against. The contract that changes is the hook/shell one, so the new tests belong at the command/event boundary as you suggest (delay the envelope; immediate connector failure, device failure, local oversize, immediate success; assert exactly one terminal UI outcome and no surviving capture).

### R2 — P1: cloud streams can report success for incomplete or empty answers

**Evidence: source-confirmed.**

Both [`anthropic.rs`](../src-tauri/core/src/llm/anthropic.rs) (`stream_once`, lines 155–197) and [`groq.rs`](../src-tauri/core/src/llm/groq.rs) (lines 117–159) use `saw_event` to decide whether a response succeeded. Any decoded SSE event sets it, even if the event contains no answer text or malformed JSON. At clean HTTP EOF, both return `Ok(answer)` without requiring meaningful text or a provider completion event.

Consequences include a ping-only Anthropic response becoming an empty successful answer, a Groq error-shaped SSE frame being ignored by `apply_event`, and partial text being labeled complete if HTTP ends cleanly before model completion. This finding concerns a protocol-incomplete response with clean HTTP EOF; transport errors already take a separate error path.

Anthropic's documented event flow ends with `message_stop`; merely receiving an SSE event does not prove that point was reached. See [Anthropic streaming documentation](https://platform.claude.com/docs/en/build-with-claude/streaming). Groq's current implementation also explicitly ignores `finish_reason` and treats `[DONE]` only as a skipped frame.

**Recommendation:** track protocol completion, nonempty answer text, structured error frames and stop reasons separately. Preserve already displayed text on failure, but mark it incomplete. Keep the existing rule against retrying after a visible delta. Handle usage/trailing frames deliberately instead of confusing HTTP EOF with model success.

**Validation:** use scripted HTTP servers for ping-only/role-only responses, malformed-event-only responses, an error object after partial output, and clean EOF before completion. Include normal completion and token-limit termination so a capped answer is not mistaken for a network failure.

> **Response: Confirmed, with one scope note.** Both providers set `saw_event` on any decoded SSE event, including one whose JSON fails to parse (`apply_event` returns `Ok(())` on a parse failure after `saw_event` was already set at `anthropic.rs:177/184`, `groq.rs:139/146`). Anthropic never tracks `message_stop`; it falls into the `_ => {}` arm at `anthropic.rs:253-254`. Groq ignores `finish_reason` and skips `[DONE]`. A ping-only 200 returns `Ok("")` from Anthropic; a role-only chunk does the same from Groq.
>
> Scope note: Anthropic's `apply_event` **does** handle a `type: "error"` frame mid-stream (`anthropic.rs:229-236`) and returns an error, so the "error-shaped frame ignored" case is Groq-only, as your text says. Noting it here so the fix is not over-scoped.
>
> **Plan.** Track four things separately per stream: protocol completion (Anthropic `message_stop`, or `message_delta` carrying `stop_reason`; Groq a `finish_reason` chunk or `[DONE]`), non-empty answer text, a structured error frame (add the OpenAI-style `error` object to Groq), and the stop reason.
>
> - Clean EOF without completion becomes `LlmHttp` "The answer ended early." The sink has already delivered the deltas, and `teardown` keeps a captured entry, so the partial text stays on screen and the entry is marked incomplete. The `retry::Attempt` rule against retrying after a visible delta is unchanged.
> - Completion with zero text deltas becomes an error ("The model returned no text."), never `Ok("")`.
> - Token-limit stop (`max_tokens` / `length`) returns `Ok(text)`, with the stop reason carried in metrics so the UI can add a small "cut short" note. Product wording to be settled; default is not to treat a capped answer as a failure.
>
> The existing scripted-body harness in both provider test modules (for example the ping + `message_delta` + `message_stop` fixture at `anthropic.rs:527-530`) fits the validation list directly: ping-only, role-only, malformed-only, error-after-partial, EOF-before-stop, normal completion, and token-limit termination.

### R3 — P2: saving Settings can discard edits made while the save is pending

**Evidence: source-confirmed.**

[`SettingsView.tsx`](../src/views/SettingsView.tsx), `save` at line 275, captures a draft and awaits `onSave`. Only the Save button is disabled. Profile text, provider controls, profile creation/deletion and key inputs remain editable. When the earlier request succeeds, line 298 replaces the entire draft with the returned snapshot and clears all key drafts.

Example: change a resume, click Save, then type another sentence before the disk operation finishes. The old response replaces the newer sentence. Changing or typing a key during the same interval can also be lost. The dirty-form guard does not protect against this replacement.

**Recommendation:** either disable the editable form while saving, or retain a draft revision and apply server normalization only where no newer edit exists. Make Back/Escape behavior during an in-flight save explicit as well.

**Validation:** hold a fake save promise open, edit text and a key, then resolve it. The UI must prevent those edits or preserve them as unsaved; it must never silently remove them.

> **Response: Confirmed.** Only the submit button is disabled during the await (`SettingsView.tsx:633`); the profile textareas, chip controls, add/duplicate/delete and the key inputs stay live. On success the whole draft is replaced (`setDraft(seedDraft(env.value))`, line 298) and key drafts are cleared (`setKeys({})`), so anything typed in that interval is lost without notice.
>
> **Plan.** Wrap the editable body in `<fieldset disabled={saving}>`. It is the smallest change, matches the stated contract ("main is the source of truth, the form is a proposal", `commands.rs`), and the save is normally well under 100 ms so the lock is invisible. `onBack` and the Escape handler will also refuse while `saving` is true, so the view cannot unmount mid-save and drop the response. The draft-revision-and-merge alternative is more code for a case the lock removes; we will revisit only if the lock proves visible in practice.
>
> Test as you describe: hold the fake promise open, attempt to edit text and a key, resolve, and assert the edits were prevented (disabled) rather than silently discarded.

### R4 — P2: the local prompt-size warning misses requests already over the hard limit

**Evidence: reproduced byte calculation from current prompt constants.**

[`SettingsView.tsx`](../src/views/SettingsView.tsx) warns only when raw profile fields exceed 6,500 bytes. [`local.rs`](../src-tauri/core/src/llm/local.rs), `request_body`, rejects the complete system and user messages above 7,000 bytes. The latter also includes role text, section headers, grounding instructions, style instructions, call framing and the question wrapper.

For an interview profile containing only a 6,500-byte ASCII resume, Balanced style and the question `Hi?`, the current construction totals **7,274 bytes**. Settings displays no warning, but the backend rejects the request. A user can therefore discover the problem only after recording a question.

**Recommendation:** expose the actual fixed/profile prompt budget from the core, show the remaining question allowance, and reject a profile that cannot fit even a minimal question before recording starts. Keep the final backend check because recorded question length is not known in advance. Avoid duplicating prompt strings in TypeScript just to reproduce the calculation.

**Validation:** cover every call type/style, boundary sizes, emoji/multibyte input, empty fields and typed versus transcribed questions.

> **Response: Reproduced independently.** Interview profile, 6,500-byte ASCII resume, Balanced, question `Hi?`: system 7,203 bytes + user 71 bytes = **7,274**, over the 7,000-byte cap in `local.rs:16`. The fixed overhead for that shape is 771 bytes (role instructions 457, resume header 28, grounding note 111, the blank-line join 2, Balanced style 105, user wrapper 68), so a 6,500-byte threshold was never derivable from the budget. The comment at `SettingsView.tsx:37-40` describes it as a profile that "already leaves no room", which is a heuristic rather than a calculation. Detailed style adds roughly 170 bytes; focus and extra-instruction headers add 27 and 47 bytes plus their text; non-interview call types add a call line of 100-170 bytes and a longer grounding note.
>
> Two smaller discrepancies the same fix removes: the warning sums the raw fields while the prompt builder trims their edges, and the UI counts UTF-8 bytes with its own `utf8Bytes` helper (`SettingsView.tsx:152`) instead of asking the core.
>
> **Plan.** Add a core function `local_prompt_budget(profile, style)` returning `{ fixedBytes, profileBytes, remainingForQuestion }`, exposed either on the settings view or through a small `local_budget` command. Settings shows "N bytes left for the question" for the local provider and turns the warning into a blocking error when the remainder is below a small minimum (about 200 bytes). The backend check in `request_body` stays as the final gate because the transcribed question's length is unknown until Stop. No prompt strings are duplicated in TypeScript. Validation list accepted as written.

### R5 — P2: settings saves are serialized per control, not across the application

**Evidence: source-confirmed opportunity for concurrency; response reordering not reproduced in the native app.**

[`ProfileChips.tsx`](../src/components/ProfileChips.tsx) and [`StyleChips.tsx`](../src/components/StyleChips.tsx) each have their own `saving` flag. They can issue overlapping saves, and Settings can be opened while a chip save is pending. [`App.tsx`](../src/App.tsx), `applySettings`, unconditionally installs each response's entire settings snapshot.

The Rust store lock protects each persisted update, but [`commands.rs`](../src-tauri/src/commands.rs), `set_settings_inner`, releases it before applying hotkey/window side effects and returning the snapshot. Store consistency does not establish response or side-effect ordering. A delayed older response can replace a newer UI snapshot; a form opened from an old snapshot can later submit stale fields.

**Recommendation:** coordinate writes centrally and attach a monotonically increasing persisted revision to settings views. Apply side effects in the same ordered workflow. A frontend “latest request wins” counter alone is insufficient unless backend commit order is also defined.

**Validation:** overlap style and profile changes, open Settings before a chip save completes, and deliberately reverse response delivery. Check the disk, visible selection, next prompt, hotkey and window flag all agree.

> **Response: Partly confirmed; the exposure is narrower than the paragraph implies.**
>
> Confirmed: independent `saving` flags (`ProfileChips.tsx:15`, `StyleChips.tsx:17`); `applySettings` (`App.tsx:82-93`) installs every response snapshot unconditionally; side effects in `set_settings_inner` run after the store lock is released.
>
> Narrowing: the chips send single-field patches (`{ activeProfileId }` and `{ answerStyle }`, `App.tsx:96-105`) and the store applies each patch under one lock, so the **on-disk** state is always a correct merge regardless of response order. What can go wrong is (1) the in-memory snapshot in `App` briefly showing an older response's value if two responses reorder over IPC, and (2) a Settings form seeded from a stale snapshot later submitting the full `profiles` array, which is a whole-object overwrite. Case 2 is the one that can lose data.
>
> **Plan.** Add a monotonically increasing `revision: u64` to `Settings`, bumped on every committed patch. `applySettings` ignores a response whose revision is below the one already held. `SettingsView` submits the revision it was seeded from, and the core rejects a patch with a stale revision with a clear message ("Settings changed while this form was open. Reload it."). That defines commit order at the backend without a central write queue, and the same revision lets side effects skip themselves when a newer revision has already applied. Validation as you list it, including deliberately reversed response delivery. Scheduled after R1–R4.

### R6 — P2: release verification is still incomplete

**Evidence: documented gap; no full native smoke test performed in this review.**

[`REPORT.md`](REPORT.md), sections 5–6, records passing core/frontend checks but only compilation of the 50 shell tests. It explicitly excludes live desktop verification and installer building. [`FREE_VOICE_TEST_RESULTS.md`](../docs/FREE_VOICE_TEST_RESULTS.md) records a successful standalone speech sample, but failed local-model warm-up under memory pressure and no complete native voice-to-answer run.

Those are useful results, but they do not establish packaged-app behavior, real capture cancellation, local answer quality, DPI geometry or screen-share exclusion. No checked-in `.github/workflows` directory was present in the reviewed tree.

**Recommendation:** add repeatable Windows automation for core and shell tests, frontend tests/build, and a packaged-build smoke test. Keep live-provider/device checks separate from deterministic tests. Record commit, artifact, toolchain, machine, date and pass/fail outcome, instead of relying on a historical test count.

**Validation:** first-run installer flow, upgrade with existing profiles/keys, Record → Stop → answer, immediate failures, rapid supersession, hotkeys from another app, 100/125/150% scaling, monitor removal, actual sharing applications, local offline operation after setup, and cancellation followed by another local request.

> **Response: Confirmed.** There is no `.github/workflows` directory. `REPORT.md` §5-6 records the 50 shell tests as compiled, not executed, and `FREE_VOICE_TEST_RESULTS.md` records the memory-pressure warm-up failure. The blocker is the build machine: free disk has been near 3 GB and physical memory was under 1 GB during your run, which is why the shell tests and packaged build have not been executed here.
>
> **Plan.**
>
> - Add `docs/RELEASE_CHECKLIST.md` carrying your validation list verbatim (installer first run, upgrade with existing profiles and keys, Record then Stop then answer, immediate failures, rapid supersession, hotkeys from another app, 100/125/150% scaling, monitor removal, actual sharing applications, local offline operation, cancel then another local request) and a results table with commit, artifact, toolchain, machine, date and outcome per run. Historical counts are not carried forward.
> - Add a workflow for the deterministic gates (core tests, clippy, frontend typecheck/tests/build, Python unit tests) and a manually triggered job for the packaged NSIS build plus shell tests. Live-provider and device checks stay manual and are recorded in the same table.
> - The first full run happens once disk is freed on this machine or on a second Windows box.

### R7 — P2: the screen-sharing promise is stronger than the evidence

**Evidence: documentation mismatch with the platform guarantee.**

[`USER-GUIDE.md`](USER-GUIDE.md), line 92 and the troubleshooting row at line 200, describes invisibility as unconditional. [`lib.rs`](../src-tauri/src/lib.rs) does request content protection before showing the window and fails setup when that call returns an error. That is good ordering, but an API success is not evidence for every capture method or conferencing application.

Microsoft describes display affinity as protection through a specific set of public capture mechanisms and does not guarantee complete protection. `WDA_EXCLUDEFROMCAPTURE` is supported starting with Windows 10 version 2004. See [Microsoft's SetWindowDisplayAffinity documentation](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity).

**Recommendation:** describe the protection accurately, state the supported Windows baseline, and publish the actual capture configurations tested. Align the user guide and README with the more qualified threat model in `docs/SECURITY.md`. This review did not demonstrate a capture bypass.

> **Response: Confirmed.** `lib.rs:125` sets content protection before `show()` at line 133, and a failure aborts setup, so the ordering claim is accurate. `docs/SECURITY.md:150-151` already carries the qualified statement ("anti-embarrassment, not anti-forensics"); `USER-GUIDE.md:92` and `:200` and `README.md:20` and `:275` are unconditional.
> **Correction (2026-09-22):** "a failure aborts setup" was wrong. tao 0.35.3 (`platform_impl/windows/window.rs:1092`) discards the result of `SetWindowDisplayAffinity` with `let _ =`, and tauri-runtime-wry's `set_content_protected` only reports a message-send failure, so the `?` at `lib.rs:125` can never see a Windows refusal. The exclusion is requested, not verified. The fix (read back `GetWindowDisplayAffinity` after the call and surface a refusal) is tracked in the 2026-09-22 readiness report.
>
> **Plan.** Reword the user guide §3, the troubleshooting row and both README passages to: the window is excluded from screen capture through Windows display affinity (`WDA_EXCLUDEFROMCAPTURE`, Windows 10 version 2004 or later); this keeps it out of Zoom, Teams and Meet screen sharing and most screen-recording tools, and is not a guarantee against every capture method. The troubleshooting row becomes "the app refuses to launch if Windows denies content protection; if a specific application still shows the window, report the application and capture mode." The list of tested capture configurations is filled from the R6 results table and stays empty until a run has been recorded.

## Further improvements

These are worthwhile follow-ups, but should not distract from R1–R4.

| Priority | Improvement | Reason and suggested boundary |
| --- | --- | --- |
| P2 | Recover corrupt settings without overwriting the evidence | `SettingsStore::load_from` falls back to defaults on unreadable/unparseable data; later geometry saves persist that state. Preserve a recoverable backup or quarantine copy before replacing a damaged file, and distinguish missing from corrupt data. Use disposable files to test this, never the user's real settings. |
| P2 | Bound cloud decoder and error-body memory | `SseDecoder` grows its line/event buffers without a byte ceiling; cloud providers read full HTTP error bodies before truncating their displayed snippets. The local NDJSON decoder already has a frame cap. Add explicit byte limits and test missing delimiters/oversized bodies. This is robustness work, not evidence of an observed exploit. |
| P2 | Supervise session driver failures | The core starts detached tasks without inspecting their join results. Panic unwinding preserves the process, but does not by itself emit a terminal session outcome or release every resource. Add a guarded cleanup path and a fake provider that panics to establish the recovery contract. |
| P2 | Make local installation usable outside a source checkout | The local setup UI tells users to run repository scripts, while the NSIS configuration does not bundle those scripts as resources. Provide a supported companion setup package or explicit source-checkout instructions for installer users; include stop/uninstall and service-ownership guidance. |
| P3 | Preserve the identity of the profile used for an answer | Requests snapshot a profile at session start, but chips remain switchable and history stores no profile/provider metadata. Show which profile generated the viewed answer, or make “applies to the next question” explicit during a live session. Keep metadata in memory unless persistence is separately requested. |
| P3 | Align Unicode counters and truncation feedback | UI `.length` counts UTF-16 code units; Rust limits count Unicode scalar values. Several profile fields accept over-limit text and are silently capped on Save. Count consistently and explain truncation before committing it. |
| P3 | Improve error specificity | Invalid hotkey syntax and a shortcut already owned by another app both become `registered: false`, but the UI always blames another app. Distinguish syntax, conflict and registration failure. Make the empty-profile hint use sales/support/meeting labels where appropriate. |
| P3 | Measure before optimizing further | Record local, nonsensitive stage timings and distributions for startup, first word and completion. Do not infer user-visible speed from bundle size or a single sample. Measure the existing two layers of text coalescing before changing frameworks or rendering strategy. |
| P3 | Add answer-quality evaluation fixtures | Byte-stable prompts and parser tests do not measure usefulness or factual grounding. Use synthetic profiles/questions for each call type, ambiguous transcripts and missing background. Assess unsupported claims, spoken length and usefulness separately from latency. |
| P3 | Reduce build and deployment ambiguity | Pin/document tested toolchains, use lockfile-based installs in repeatable builds, include Python and PowerShell checks, and document installer provenance/signing and update expectations. Require a usable checksum when installing a pinned runtime instead of silently treating a missing upstream digest as verified. |

> **Response to the table, row by row:**
>
> - **Recover corrupt settings.** Confirmed (`settings.rs:52-62`: an unreadable file and an unparseable one both load as defaults, and `Err(_)` does not distinguish missing from unreadable; the next geometry save persists the defaults). Accepted: rename a damaged file to `settings.json.corrupt-<timestamp>` before the first write, treat `NotFound` as a clean first run and every other read error as damage, keep per-field fallback for partially valid files. Small; will land with R3.
> - **Bound decoder and error-body memory.** Confirmed (`sse.rs:40-52`: `line: Vec<u8>` and `data: String` have no ceiling; both cloud providers read the whole error body with `response.text()` before truncating). Accepted: cap line and event buffers at 1 MiB (the local NDJSON decoder uses 256 KiB), read error bodies through a bounded stream, and test missing delimiters and oversized bodies. Small.
> - **Supervise driver failures.** Confirmed (no join handle is retained and there is no `catch_unwind` in `machine.rs`). A panicked driver leaves `Active` in the slot until the next start supersedes it, so the user sees a hang rather than an error. Accepted: a drop guard on the driver task that calls `inner.fail(id, gate, internal_error)` if the task exits without settling handles panics too, since unwinding runs `Drop`. Tested with a panicking fake provider as suggested.
> - **Local installation outside a checkout.** Confirmed (`tauri.conf.json` has `resources: null`; `LocalVoicePanel.tsx:70` tells the user to run `scripts\setup-free-voice.ps1`). Accepted: bundle `scripts/` and `local-voice/` as NSIS resources and have the panel show the resolved path, plus stop and uninstall guidance and a note on which account owns the services. Medium; after R1–R4.
> - **Profile identity per answer.** Confirmed (`HistoryEntry` in `reducer.ts:26-32` carries no profile or provider). Accepted at P3: record `profileName` and `provider` on the entry at `record/start` and `ask/start` from the current settings snapshot, show them as a caption on the answer. In memory only.
> - **Unicode counters.** Confirmed (`.length` at `SettingsView.tsx:471-519` versus `chars()` in `cap_chars`, `settings.rs:263`; only the name field has `maxLength`). Accepted: count code points in the UI, show "will be shortened on save" when over the limit instead of capping silently, and keep the Rust cap as the safety net.
> - **Error specificity.** Confirmed (`hotkey.rs:51-56` folds parse failure and OS rejection into `registered: false`; `MainView.tsx:240` says "already taken by another app"; `settings.rs:116-120` stores the accelerator without syntax validation). Accepted: validate accelerator syntax in the core at save time and reject with a message, so a `registered: false` after a successful save can only mean a conflict. The empty-profile hint at `MainView.tsx:269` hard-codes "resume or job description"; accepted, switch the wording on `callType`.
> - **Measure before optimizing.** Agreed. The startup stage timer in `lib.rs` already exists; extend it to first word and completion using the existing `Metrics`, log locally, never over the network. Deferred behind the correctness work.
> - **Answer-quality fixtures.** Agreed in principle. Needs a rubric and is a separate piece of work; deferred.
> - **Build and deployment ambiguity.** Confirmed for the checksum point: `local-voice/install_ollama.py:43-46` verifies the digest only when the upstream asset carries one, so a missing digest is treated as verified. Accepted: fail closed with a clear message, or pin the expected SHA-256 next to `VERSION`. Toolchain pinning (`rust-toolchain.toml`, `.nvmrc`, `npm ci` in repeatable builds) and Python/PowerShell checks are cheap and accepted.

## Specific assessment of `fabledocs`

The folder is a valuable record of why v3.1 was built. Its strongest quality is the connection between a user problem, an implementation contract and a verification statement. It should remain a historical record; current product behavior should have one clear home in `docs/SPEC.md` and the user-facing guide.

| Document | Assessment and proposed improvement |
| --- | --- |
| [`README.md`](README.md) | Useful reading order. Add an explicit distinction between the historical implementation pass, current product instructions, and later reviews such as this one. The “uncommitted” statement is a snapshot of that session, not a permanent repository property. |
| [`REPORT.md`](REPORT.md) | Good implementation inventory and unusually useful disclosure of checks that could not run. Treat “Rust: no defects” as the result of that review, not an assurance about the whole app. Attach future release claims to actual build artifacts and verification records. |
| [`AUDIT.md`](AUDIT.md) | The built/deferred/rejected tables are actionable. Retain the baseline and decision history, but separate “implemented” from “validated on hardware.” For example, launch placement does not implement continuous re-docking after every monitor change or drag. |
| [`DESIGN.md`](DESIGN.md) | Clear constraints, IPC contracts, migration rules and non-goals. Its statement that every finding was adversarially verified should be qualified: `AUDIT.md` says 32 of 42 independent verifications completed and ten were judged manually. Mark the work-package instructions as historical rather than a standing change request. |
| [`USER-GUIDE.md`](USER-GUIDE.md) | Good task-oriented explanation of profiles and reading modes. Qualify the screen-sharing claim; distinguish recommended free disk space from model download volume (“about 8 GB of models” conflates the two); separate installer prerequisites from source-build prerequisites; include shell and packaged-app checks in the release checklist. |
| [`design/study-eye-level-answer.md`](design/study-eye-level-answer.md) | Useful rationale and alternative exploration. Keep it archived as a proposal; its code sketches and measurements should not be copied as if they describe the final implementation. |
| [`design/study-call-profiles.md`](design/study-call-profiles.md) | Sensible restraint: self-contained profiles and closed call types avoid unnecessary persona/configuration complexity. Preserve that direction. Runtime profile budget validation remains unfinished despite the Settings counter. |
| [`design/audit-findings-raw.md`](design/audit-findings-raw.md) | Useful provenance, but verbose and partly truncated verifier/amendment text makes it unsuitable as the only evidence for a decision. Link final decisions to complete evidence and mark incomplete historical entries clearly. |

> **Response to the folder assessment:** Accepted in full.
>
> - `README.md`: add a status line dating the "uncommitted" statement to 2026-09-18, and a "Reviews" section that distinguishes the implementation pass, the current product instructions and later reviews such as this one.
> - `REPORT.md`: annotate "Rust: no defects" as the result of that review's scope, and attach any future release claim to the R6 results table.
> - `AUDIT.md`: add an "implemented / validated on hardware" split; the launch-placement example is correct, docking is applied at launch and on the button, not continuously.
> - `DESIGN.md`: confirmed mismatch. `DESIGN.md:8-9` says every finding was adversarially verified; `AUDIT.md:15-16` says 32 of 42 verifications completed and ten were judged manually. `DESIGN.md` will be corrected to match, and the work-package section marked historical.
> - `USER-GUIDE.md`: all four points accepted. On "about 8 GB of models": the setup script itself describes the Qwen pull as about 2.7 GB, so the figure conflates download volume with recommended free space; both will be stated separately once measured.
> - The two design studies get an "archived proposal" banner; `audit-findings-raw.md` gets a banner noting the truncated verifier text and pointing to `AUDIT.md` for the decisions.

A related correction is needed in [`docs/SECURITY.md`](../docs/SECURITY.md), around line 110: removal of `open_external` did **not** leave the IPC handler list without process-launching behavior. `prepare_local_voice` remains registered and can launch the configured local services. Describe that intentionally exposed, constrained capability accurately instead of claiming it does not exist.

> **Response: Confirmed.** `lib.rs:150` registers `prepare_local_voice`, and `local_voice.rs:241-262` launches `Command::new(spec.executable)` with `CREATE_NO_WINDOW`. The sentence in `docs/SECURITY.md` is wrong as written and will be replaced with an accurate description: the one process-launching command is `prepare_local_voice`; it takes no arguments from the webview, launches only the executables named in the local-voice configuration under `%LOCALAPPDATA%`, and is serialized by a mutex. Since that configuration file is writable by the local user, the trust boundary is the user's own account, which is the same boundary the DPAPI key storage relies on; the document will say so rather than imply a stronger one.

The earlier audit's decisions to defer a framework replacement, richer boot payload, and larger persona features still look reasonable. The new findings concern correctness and verification and deserve attention before those optimizations.

## Verification performed for this review

Verification results are recorded below. Historical counts from `REPORT.md` are not substituted for fresh execution.

| Check | Result |
| --- | --- |
| Python service unit tests: `python -B -m unittest discover -s local-voice -p test_server.py` | **Passed: 3 tests.** No model download or live inference was performed. |
| Actual frontend reducer, early-error ordering | **Reproduced R1:** both Record and Ask lost the early error and retained an active UI session. In-memory execution only; no test/source file added. |
| Prompt-size calculation using current Rust prompt constants | **Reproduced R4:** 6,500 profile bytes, no Settings warning, 7,274 actual input bytes against a 7,000-byte cap. |
| Frontend suite: `node node_modules/vitest/vitest.mjs run --maxWorkers=2` | **Passed: 375 tests across 16 files**, zero failures; 596.29 seconds on this resource-constrained run. These tests do not cover all integration gaps identified above. |
| Frontend typecheck and production build: `npm run build` | **Passed.** TypeScript checking completed, and Vite built 58 modules. Main JS: 178.33 kB; lazy Settings JS: 14.64 kB; lazy PracticeLibrary JS: 6.04 kB. This refreshed ignored `dist/` output. |
| Rust core tests | **Incomplete, not a test failure.** Attempted with `cargo test -p app-core --locked --offline -j 2`; stopped the review's Cargo/compiler process tree during compilation to reduce memory pressure. No fresh Rust test result is claimed. |
| Native shell tests, installer, live audio/providers, screen sharing, real display geometry and assistive technology | **Not executed in this review.** Source/component inspection does not validate these behaviors. |
| Report and change-scope checks | All local report links resolve; no trailing whitespace found. Hash comparison of existing tracked/nonignored project files found no changes; the only added file was this report. |

The first frontend run did not receive the requested worker limit through the local npm invocation. It was stopped and restarted directly with `node node_modules/vitest/vitest.mjs run --maxWorkers=2`. Available physical memory was approximately 780 MiB at one observation. Only processes started for this review were targeted for cancellation; no user applications or caches were removed.

> **Response:** Noted, and thank you for stating what was not run. For these responses the byte calculation in R4 was re-derived independently and every source claim was re-read in the working tree; the frontend suite and the Rust tests were not re-executed today because the machine constraints are unchanged. Fresh results will be recorded in the R6 table, not carried over from `REPORT.md`.

## Suggested implementation order

1. Fix the start/event delivery contract and add tests that cross the command/event boundary (R1).
2. Enforce cloud stream completion and honest incomplete-answer outcomes (R2).
3. Protect in-flight Settings edits, coordinate saves, and calculate the real local prompt budget (R3–R5).
4. Complete Windows shell, package, device and local-model validation; correct documentation claims to match the results (R6–R7).
5. Then address recovery, bounded resource use, installation usability and measured quality/performance improvements.

The existing architecture supports this work. The most useful next release would make the current feature set dependable and its guarantees verifiable.

> **Response:** Order accepted as written, with one fold: the corrupt-settings backup and the SSE/error-body bounds are small and touch the same files as R3 and R2, so they ride along in steps 2 and 3 rather than waiting for step 5.

## Agreed actions (maintainer, 2026-09-19)

Derived from the responses above. Order is the implementation order; the checkbox is the state as of this date.

- [ ] **R1** Core records the last terminal outcome; `start_session` and `ask` return it instead of `Ok(id)` when the session has already ended; `session_outcome(id)` reconciliation on adoption; boundary tests with a delayed envelope.
- [ ] **R2** Providers track completion, non-empty text, error frames and stop reason separately; early EOF and empty completion become errors that keep displayed text; token-limit stop is reported, not failed. Bounded SSE buffers and error bodies land here.
- [ ] **R3** Settings form locked with a disabled fieldset while saving; Back and Escape refused during a save. Corrupt-settings quarantine lands here.
- [ ] **R4** Core-computed local prompt budget exposed to Settings; blocking error when a minimal question cannot fit.
- [ ] **R5** `revision` on `Settings`; stale responses ignored in the frontend, stale form submissions rejected by the core.
- [ ] **R6** `docs/RELEASE_CHECKLIST.md` with the validation list and results table; workflow for deterministic gates; manual job for the packaged build. First run once disk allows.
- [ ] **R7** Screen-sharing wording corrected in the user guide, README and troubleshooting; tested configurations listed only from recorded runs.
- [ ] **Docs** `docs/SECURITY.md` `prepare_local_voice` correction; `DESIGN.md` verification-count correction; `fabledocs/README.md` status and reviews section; archived banners on the studies and raw findings; `USER-GUIDE.md` download versus disk figures.
- [ ] **Later** Driver drop guard; bundled local setup resources; profile and provider on history entries; code-point counters and pre-save truncation notice; accelerator syntax validation and call-type-aware empty-profile hint; fail-closed Ollama checksum; toolchain pins; local stage timing; answer-quality fixtures.

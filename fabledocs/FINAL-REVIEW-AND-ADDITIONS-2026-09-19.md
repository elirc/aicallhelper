# Final review of Fable's response and recommended additions

**Date:** 2026-09-19  
**Scope:** Review of the maintainer responses added to [PROJECT-REVIEW-2026-09-19.md](PROJECT-REVIEW-2026-09-19.md), checked against the current application and [existing roadmap](../docs/IDEAS.md).  
**Change boundary:** This report is the only deliverable. No application fixes, dependency changes, or edits to existing documentation were made.

## Final assessment

Fable's response is broadly sound. It confirms the important defects, appropriately narrows the settings concurrency finding, and accepts the need for stronger release evidence. The existing Rust core, Tauri shell, provider interfaces, and React state separation support the proposed work; a rewrite would not address the main problems.

However, **agreement and implementation plans do not close the findings**. All items in Fable's action list remain unchecked. Comparing the current tracked/nonignored project files with the previous review's final hashes found only the annotated review had changed; application code was unchanged.

The next release should make session completion, saved settings, and local-mode limits dependable. Three proposed fixes need particular refinement before implementation: recovery must represent successful completion as well as errors; stream completion must follow the provider's actual protocol; and settings revisions must coordinate operating-system effects as well as UI responses.

## Decisions on Fable's response

| Finding | Final decision | Required refinement |
| --- | --- | --- |
| **R1: events arrive before session adoption** | Confirmed; highest priority | Define a complete session outcome and subscription-readiness contract. A stored error alone cannot recover an early successful answer. |
| **R2: incomplete cloud streams appear successful** | Confirmed; highest priority | Require the documented terminal event, preserve stop reasons, and retain incomplete status on the history entry. |
| **R3: edits lost during Settings save** | Accept the disabled-form approach | Cover every editing/navigation path and recovery after a failed save. Do not assume saves always finish quickly. |
| **R4: inaccurate local prompt warning** | Accept one authoritative Rust budget calculation | Distinguish actual size rejection from a suggested reserve for future questions; calculate unsaved drafts too. |
| **R5: settings response/effect ordering** | Accept revision-based concurrency control | Compare revisions atomically during persistence and order/reconcile OS effects. Address alongside R3. |
| **R6: incomplete release verification** | Accept checklist and automation | Execute deterministic shell tests automatically on Windows; a manually available build job alone is not release evidence. |
| **R7: overstated screen-share protection** | Accept correction, revise proposed wording further | Named conferencing-app claims require a recorded capture matrix. API success alone does not establish them. |

Fable correctly notes that distinct-field patches already merge under the settings lock. R5 is about stale full views, stale form replacements, and effects performed after that lock is released; it is not evidence that every overlapping chip click loses persisted data.

## Refinements required before implementing the fixes

### 1. Recover the complete session, including early success

The relevant boundary is [commands.rs](../src-tauri/src/commands.rs), [machine.rs](../src-tauri/core/src/session/machine.rs), [bridge.ts](../src/bridge.ts), and [useSession.ts](../src/state/useSession.ts).

Fable proposes `LastOutcome { id, error }`, a pre-return activity check, and an outcome lookup after adoption. That is a useful direction, with these requirements:

- Represent **active, completed, failed, and cancelled/superseded** outcomes explicitly. Completed results need the transcript, answer, and metrics. Never turn a successful early completion into an error because its session is no longer active.
- Read activity and outcome consistently. Document when an outcome can be retired. A single global “last outcome” needs proof that another completion cannot replace an outcome still needed by a relevant pending adoption; bounded retention by session ID is an alternative.
- Make reconciliation idempotent and retain the existing attempt/session guards. A live event and a lookup may both report completion; an older lookup must never settle a newer session.
- Account for text emitted before adoption while the session is still running. A terminal-only lookup does not recover that prefix. Use a snapshot with an event sequence boundary, bounded replay, or a reserve/adopt/activate protocol; avoid appending replayed text twice.
- Establish listener readiness before starting a session. Today `bridge.on` starts asynchronous `listen()` calls and silently catches registration failures (lines 71–83). A one-time lookup can report “active,” then miss completion if subscriptions are still unavailable. Surface subscription failure and make readiness part of the contract.

Move driver supervision into this work package. Fable's proposed task guard should settle once and clean up owned resources without panicking itself, including during unwinding. It must preserve cancellation and supersession semantics.

**Acceptance:** exercise immediate failure, immediate success, early deltas, delayed command responses, delayed/failed listener registration, duplicate terminal delivery, cancellation before adoption, rapid supersession, and a panicking fake provider. The UI must return to an actionable state, preserve the right text, and release capture/streams exactly once.

### 2. Separate protocol completion from why generation stopped

Fable proposes treating either Anthropic `message_stop` or a `message_delta` with a stop reason as completion. The documented flow ends with `message_stop`; a stop reason is useful metadata but does not establish receipt of the final protocol event. Require that final event. [Anthropic streaming documentation](https://platform.claude.com/docs/en/build-with-claude/streaming)

Likewise, Groq documents `data: [DONE]` as the stream terminator. Retain `finish_reason` separately rather than treating it as interchangeable proof of protocol completion. [Groq API reference](https://console.groq.com/docs/api-reference)

This intentionally changes current behavior in [groq.rs](../src-tauri/core/src/llm/groq.rs), including the test that expects content after `[DONE]` to be appended. Update that test and its explanatory comments as part of the fix. Do not consume additional answer text after the terminal marker.

The result contract should distinguish:

| Outcome | User-visible behavior |
| --- | --- |
| Normal terminal event with usable text | Completed answer |
| Token-limit termination | Keep the text and label it as cut short |
| Error or EOF before terminal event | Keep received text and label the entry incomplete |
| Terminal event without usable answer text | Explicit failure or an appropriate non-answer outcome, never a blank success |

[HistoryEntry](../src/state/reducer.ts) currently has no completion-status field; the error is held on overall session state. Add per-entry status and relevant reason so an interrupted answer remains recognizable after another question clears the global error. Keep timing measurements separate from outcome classification.

Accept the proposed SSE/error-body bounds, enforced while reading rather than after allocating the whole body. Preserve existing deadlines and the prohibition on retrying after visible answer text. Tests should include malformed known frames, harmless unknown events, metadata-only streams, missing terminators, structured errors, token limits, and oversized input.

### 3. Treat Settings edits, persistence, and OS effects as one contract

The disabled fieldset is a reasonable fix for R3. Include profile add/delete/selection, key inputs, Save, Back, and Escape; show a clear saving state. Failed saves must restore interactivity and retain the draft. Test with a deliberately delayed response rather than relying on the response's unmeasured “under 100 ms” assumption.

For R5, accept backend revisions instead of requiring a central frontend queue, subject to:

1. Compare the form's expected revision and commit its patch under the same store synchronization. A failed disk write must not advance the committed revision.
2. Apply only current views in [App.tsx](../src/App.tsx), using a comparison safe against concurrent response handlers. Preserve drafts on conflict and offer a clear reload/recovery path.
3. Define whether geometry-only writes affect the editable-settings revision; moving the window should not needlessly invalidate a form.
4. Order or reconcile effects in [set_settings_inner](../src-tauri/src/commands.rs). A revision check alone has a check-then-act race. Skipping older work can also omit a necessary effect: revision A changes the hotkey, revision B changes only style, and discarding A does not make B register the new hotkey. Reconcile the latest desired OS state or maintain an ordered effect workflow.

**Acceptance:** reverse response delivery and effect scheduling, overlap a full form with chip changes, inject persistence failure, and verify disk, UI, hotkey registration, and window behavior agree. Do this in the same reliability phase as R3.

### 4. Make the local budget accurate without introducing an arbitrary hard limit

The independently confirmed example remains valid: 6,500 bytes of profile text can produce a 7,274-byte request against the 7,000-byte limit. [Local request construction](../src-tauri/core/src/llm/local.rs)

Use the same Rust prompt construction, trimming, style suffix, and question wrapper for both preview and enforcement. Preview the **unsaved draft**, not only the last persisted settings. If previews are asynchronous, discard obsolete results when the user changes profile or style again.

Fable's proposed 200-byte question reserve is a possible usability warning, not the actual technical boundary. Show the remaining capacity and distinguish “little room for a question” from “this request exceeds the limit.” Saving a profile for cloud use should remain possible. For voice, check known overhead before recording and validate the actual transcript after Stop; for typed Ask, validate the actual question. Keep the backend gate authoritative.

**Acceptance:** cover all call types/styles, optional-field combinations, whitespace trimming, emoji/non-Latin text, profile switching, and exact boundary sizes.

### 5. Narrow several secondary proposals

| Proposal | Final recommendation |
| --- | --- |
| Rename every unreadable settings file as corrupt | Distinguish missing, invalid readable content, and permission/sharing/transient I/O errors. An unreadable file is not necessarily corrupt. Preserve a recoverable copy before replacement; do not overwrite the original when preservation fails. Test using disposable files. |
| Put profile/provider labels on entries from current frontend settings | Obtain metadata from the authoritative backend request snapshot. A stale frontend view can otherwise mislabel the answer. Include profile ID/name, provider/model, and effective style without copying private profile text into diagnostic records. |
| Syntax validation makes every remaining hotkey failure a conflict | Validate syntax before persisting, but retain a general registration-failure outcome. Classify conflict only when the actual error supports it. Report desired and successfully registered state accurately. |
| Bundle local setup scripts | Accept, but preserve their relative resource layout and install writable runtimes/models in the configured user data directory. Verify a fresh installed app with no source checkout, paths containing spaces, missing Python, interrupted setup, and existing local services. Stop/uninstall must respect process ownership. |
| Fail closed when a runtime checksum is absent | Accept. A pinned digest beside the pinned version is also reasonable. Verify downloads before use and give a recoverable error. |
| Local timing logs | Accept bounded, opt-in diagnostics with useful timing/error metadata. Exclude prompts, transcripts, answers, credentials, and private profile names. “Local” alone does not make a log insensitive. |

## What to add next

These additions build on the current interaction model and the existing [ideas document](../docs/IDEAS.md). The order below favors recovery and call readiness before extra generation options. Effort is relative: **S** is localized work, **M** spans several components, and neither is a delivery estimate.

| Order | Addition and value | Minimum useful scope and acceptance | Dependencies / effort |
| --- | --- | --- | --- |
| **1** | **Answer status and context labels.** Users should know whether text is complete and which profile produced it. | In-memory entry caption with completed/incomplete/cancelled/limited status, profile, provider, and style. Labels remain correct after switching settings and browsing history. | Part of R1/R2 contracts; S–M |
| **2** | **Edit & re-ask.** Correct one misheard name without retyping the whole question. | Prefill the existing Ask form from the viewed entry; focus it; submit only on the user's action. Create a new history entry and preserve the original. Show that the current profile/provider will be used. Do not add a confirmation gate to every voice answer. | Existing Ask path; S |
| **3** | **Stop generating.** Particularly valuable during a slow local answer. | A direct cancel control during generation/finalization. Flush displayed text, label it cancelled, clear pending adoption safely, release resources, and allow the next request immediately. Cancellation must work before the ID response and during streaming. | R1 and outcome labels; S–M |
| **4** | **Pre-call readiness check.** Find configuration problems before the conversation starts. | Show selected profile/provider, capture output device, credential presence, local service/model readiness, and local prompt capacity. Separate “configured” from “tested”; timestamp checks. Extend the existing LocalVoicePanel rather than duplicating it. Any audio/provider test is explicitly started by the user. | R4 and installed-app validation; M |
| **5** | **Reliable local setup from the installed app.** Make local mode usable without locating the repository. | Bundled setup resources or a supported companion installer, resolved setup location, progress/failure guidance, and ownership-aware stop/remove instructions. Complete the first-install-to-answer journey on a clean Windows account. | Packaging, checksum work; M. Required before claiming turnkey installer support. |
| **6** | **Explicit Markdown debrief export.** Retain useful questions after a call. | User-initiated save with question, answer, context, and outcome labels; no automatic transcript persistence. Initially export retained history and clearly state its scope. A later full-call buffer needs an explicit size limit and Clear behavior; current history retains six entries. | Entry metadata; S for retained-history export, M for bounded full-call capture |
| **7** | **Local performance and diagnostic view.** Make provider and startup decisions measurable. | Bounded timing samples, provider/model, sample count, cold/warm distinction, success/failure counts, and p50/p95 only with clear sample context. Offer a previewable copy of diagnostic metadata. Keep recording optional and off the answer-rendering path. | Stable outcomes/metadata; M |
| **8** | **Output-device selection and reading preferences.** Improve daily use across headsets and displays. | Device picker with a visible fallback when unavailable; text-size/line-spacing controls that preserve copy, focus, and scrolling behavior. Test at supported DPI scales and narrow widths. | Device lifecycle tests and UI validation; M |

The existing roadmap's style modifiers and speaking-length presets remain reasonable later additions. They should follow the above recovery work and measured answer-quality checks. The current style suffix offers an appropriate location for such preferences, but “cache-friendly” should not be described as guaranteeing zero end-to-end latency cost.

Also add a **small synthetic answer-quality evaluation set** as a development asset: interview, sales, support, and meeting questions; empty and conflicting context; short follow-ups; and requests for facts absent from the profile. Score grounding, unsupported claims, relevance, brevity, and speakability. Record the model, prompt revision, and generation settings. Keep these slower, variable evaluations separate from deterministic unit-test gates.

Continue deferring automatic provider fallback, always-listening question detection, dual microphone capture, and multi-turn memory until their user value justifies the additional state and privacy behavior. The response gives no new evidence requiring a framework replacement or broader persona system.

## Documentation and release additions

Fable's documentation corrections are appropriate. The next documentation pass should add:

- **A release checklist and results ledger** with commit, artifact identity, toolchain, machine/OS, date, test procedure, result, and evidence. Keep implementation status separate from hardware validation.
- **A short contract note or ADR** for adoption/reconciliation, terminal answer outcomes, and settings revisions/effect ordering. These decisions cross Rust, IPC, and React and need one agreed specification.
- **A current index in fabledocs** linking the implementation report, annotated review, this final report, and release evidence. Date historical claims and mark the design studies/raw audit as historical material.
- **An installed-user local-mode guide** covering prerequisites, download size versus recommended free space, configured data location, startup, cancellation, and uninstall ownership.

Revise the proposed screen-sharing sentence further. Saying it “keeps it out of Zoom, Teams and Meet” still claims behavior before the test matrix exists. Suggested wording:

> The app requests Windows capture exclusion. Its effect depends on the Windows version and capture method. Check the recorded compatibility results and test your intended sharing setup before relying on it.

Microsoft describes protection through a specific set of public capture APIs, not a universal guarantee; `WDA_EXCLUDEFROMCAPTURE` is supported starting with Windows 10 version 2004. Record conferencing-app version, OS build, and capture mode for each tested configuration. [Microsoft display-affinity documentation](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity)

The planned [SECURITY.md](../docs/SECURITY.md) correction should distinguish the configuration file's location from executable locations. [setup-free-voice.ps1](../scripts/setup-free-voice.ps1) accepts a custom absolute data directory; executables need not live under the default LocalAppData directory. Describe `prepare_local_voice` and the actual same-user configuration trust boundary precisely.

## Recommended delivery sequence

1. **Session and streaming reliability:** R1/R2, subscription readiness, task supervision, bounded reads, and per-entry outcomes.
2. **Settings and local budget:** R3–R5, careful recovery of damaged settings, accurate hotkey failure messages, and Unicode-consistent limits.
3. **Release proof:** automatic frontend/core/Python checks and deterministic Windows shell tests; build the installer and record real audio, local inference, cancellation, hotkey, DPI/monitor, and screen-sharing checks.
4. **First usability additions:** Edit & re-ask, Stop generating, authoritative context captions, and pre-call readiness.
5. **Measured expansion:** installed local setup where still outstanding, explicit export, bounded diagnostics, device/reading preferences, and answer-quality evaluation.

Steps 1–3 establish the release baseline. If local mode is advertised as supported in the installer, its setup and complete offline voice-to-answer path must also pass before that release.

## Evidence and limits of this final review

This follow-up re-read Fable's responses, checked the affected source paths and roadmap, compared project file hashes, and checked the primary provider/Windows documentation cited above. Proposed acceptance tests in this report are recommendations, not completed test results.

The earlier review recorded **375 frontend tests passed, a successful frontend production build, and 3 Python tests passed**. Those results are documented in the [original review](PROJECT-REVIEW-2026-09-19.md#verification-performed-for-this-review); they were not rerun for this report. The earlier Rust attempt remained incomplete during compilation. No new native shell, installer, live-provider, audio-device, screen-sharing, or accessibility result is claimed.

**Final recommendation:** accept Fable's response as the basis for implementation, with the refinements above. Keep the findings open until the amended behavior and its acceptance checks are demonstrated.


# Reference review — sealed answer

Do not open until you've done `training/review/EXERCISE.md` and filled in `COMPARE.md`.

## Where this comes from

This repo already contains a staff-level, evidence-cited review at `docs/REVIEW.md`,
written by six reviewers against commit `d18cd33` (2026-08-20). Two feature commits have
landed since (`bfd0955` free local voice, `f1cf524` v3.1 call profiles/dock-to-camera). This
answer key **re-verifies each MEDIUM+ finding against current HEAD (`f1cf524`)** rather than
assuming the old review still holds — because "does this document still describe the code"
is itself the finding that matters most here (see §3 of `COMPARE.md`).

If your own list found something not listed below, that's a legitimate independent finding
— log it, don't discard it because it isn't "in the answer."

---

## HIGH

### H1 — Screen-share invisibility fails open, silently, and the docs claim it can't
`src-tauri/src/lib.rs` (the `set_content_protected(true)?` call in the window setup) ·
`docs/SECURITY.md:107`

**Not independently re-verified against HEAD for this pass** (it requires reading the
vendored `tao`/`tauri-runtime-wry` dependency source, which `docs/REVIEW.md` did). The
claim: `set_content_protected`'s `Result` reflects only whether the message reached the
event loop, never whether Windows honored `WDA_EXCLUDEFROMCAPTURE`. On pre-2004 Windows 10
the call "succeeds" while the window stays fully visible in a screen share — the opposite of
what the comment and SECURITY.md promise. There is no `GetWindowDisplayAffinity` readback
anywhere in the repo (confirmed: `grep -rn GetWindowDisplayAffinity src-tauri` finds
nothing at HEAD).

**Why it matters:** this app's entire value proposition is "invisible during a screen
share." A user who is wrongly confident of invisibility is worse off than one who knows the
feature doesn't exist — they behave differently. This is the single highest-leverage finding
in the repo precisely because it's silent.

**Old-AI-smell:** yes — docs asserting a guarantee the code's own type signature (`Result`
from a call with no OS-level failure channel) cannot possibly deliver. The `?` operator
looks like it's handling an error case that structurally cannot occur through this call.

---

## MEDIUM — re-verified against HEAD f1cf524

| # | Finding | Status at HEAD | Evidence |
|---|---|---|---|
| M1 | Empty-answer guard tests "any SSE *event* seen" instead of "any *text* delivered" | **Still present, both providers** | `src-tauri/core/src/llm/anthropic.rs` `StreamProgress::conclude()` (`if !self.saw_event`) and the identical shape in `groq.rs`. A 200 stream of only `message_start`/`ping`/`message_stop` (or Groq's role-priming + `[DONE]`) resolves `Ok("")` — an `llm:done` with an empty answer and "successful" metrics, not a reported failure. |
| M2 | STT drain-timeout silently truncates the transcript tail | **Still present** | `src-tauri/core/src/stt/deepgram.rs:414`: `let _ = tokio::time::timeout(limits::STT_FINALIZE, drain).await;` — a send failure, socket error, and server error frame are all reported (`:357`, `:376`, `:389-397`), but a plain timeout (Deepgram takes >5s to flush+close after `CloseStream`) is swallowed. `finalize()` returns whatever text was accumulated before the timeout, silently missing `smart_format`'s held-back tail (numbers, dates) — contradicting the module's own stated invariant at `:366-369`. |
| M3 | `finalizing` UI state has no timeout/escape | **Still present** | `src/state/reducer.ts` — `record/stop` → `ui: 'finalizing'`; the only ways out are `record/stopRejected` (an explicit rejection envelope) or a terminal session event. If `stopSession` resolves `ok` and the core then emits nothing (e.g. the STT drain in M2 hangs past its own timeout and something upstream never calls back), the reducer has no `tick`-driven or timer-driven fallback for this state — unlike `recording`, which has the 120s cap. `useSession.ts:350` documents the no-op explicitly: "finalizing: the stop is already committed; toggling again is a no-op." |
| M4 | Groq mid-stream error frames swallowed as a silent empty answer | **FIXED since the review** | `src-tauri/core/src/llm/groq.rs` `apply_event()` now has `if let Some(error) = value.get("error") { ... return Err(...) }` with a comment reading "ignoring it used to let the stream end 'successfully'" — the fix references the exact bug the old review found. **This is the finding to flag in COMPARE.md §3**: trusting the dated doc here would have cost you a false positive. |
| M5 | `ask()` emits its first event before the caller can adopt the session id (a race the paused-clock test harness can't see) | **FIXED since the review, by a new subsystem** | The old review cited `machine.rs:313-322` racing the IPC response against the frontend's blind "drop events for an unadopted id" rule. HEAD now has an explicit adoption-reconciliation mechanism: `src/state/useSession.ts`'s `pendingRef`/`PendingBuffer` ("R1: events that arrive while a start/ask call is still in flight, kept for the attempt that is waiting to adopt its id") plus `adopt()` replaying them in order once the id lands. This is ADR 015 territory — read it *after* this exercise, not before. |
| M6 | A panic in the driver task strands the session (occupied slot, no event, UI stuck in "Generating…" forever) | **FIXED since the review** | `src-tauri/core/src/session/machine.rs` module doc now states: "Supervised driver (R1): a drop guard settles the session and aborts its STT stream if the driver task exits any other way — including a panic unwinding out of a provider — so a crash is an error, not a hang." Verify the guard's actual `Drop` impl before fully trusting this restatement — that's the M4/M5 lesson applied to this very answer key. |
| M7 | `open_external` is live IPC surface with no caller | **FIXED (removed)** | `grep -n open_external src-tauri/src/*.rs` at HEAD returns nothing — the command no longer exists. |
| M8 | No error boundary anywhere in the React tree | **FIXED** | `src/components/DeferredView.tsx:9` now has `static getDerivedStateFromError() { return { failed: true }; }`. Confirm at review time whether it wraps `<Markdown>` specifically (the old review's stated blast radius) or something narrower. |
| M9 | CommonMark paragraph-interruption guard missing on list continuation lines | **Not re-verified this pass** | `src/markdown/parse.ts` around `:274-281` (guard defined `:152`) per the old review. Cosmetic (rendering mangling), no security impact. Worth a 5-minute spot check if you're grading yourself on area B. |

## LOW (carried from `docs/REVIEW.md`, not independently re-verified this pass — treat as leads)

- `max_tokens` truncation is invisible to the user (neither provider surfaces `stop_reason`/
  `finish_reason` as "cut short" in the UI beyond the `limited` status already wired in
  `reducer.ts`'s `event/llmDone` case — confirm whether that already covers it).
- Unbiased `tokio::select!` lets a first-token timeout report *after* a delta already
  painted (`machine.rs`, `biased` would close it for free).
- Corrupt `settings.json` plus any save permanently destroys the original (no
  `settings.json.corrupt-<ts>` rename before first overwrite).
- No mid-stream backpressure on the Deepgram audio channel (unbounded `mpsc`).
- Surround downmix / resample assumptions (`resample.rs`) — probably fine, undocumented.
- Input-validation errors miscoded as `internal` in `commands.rs`.
- Focus dropped at the stop transition (`RecordButton` disables while focused).
- No live region on the transcript panel for screen readers.
- CSP missing `base-uri`/`form-action`/`frame-ancestors` (defense-in-depth only).

---

## Grading yourself

- Every HIGH/MEDIUM you found with correct evidence: full credit.
- Found the *symptom* but attributed it to the wrong line/mechanism: half credit — go back
  and trace it to the exact guard.
- Missed something because you trusted a comment or a module doc without reading the code
  it describes: that's the pattern this whole file is built to teach. Write it in
  `COMPARE.md` §4.
- **Bonus finding**: if you independently flagged that M4/M5/M6/M7/M8 look fixed (or looked
  suspicious enough to re-check) without being told, that's exactly the "verify claims
  against code" instinct this exercise is testing for. Most reviewers, given a "here's an
  expert review of this codebase" document, would have copied its findings forward
  unchecked — which is itself indistinguishable from the doc-trusts-doc failure mode
  `docs/REVIEW.md` itself calls out about SECURITY.md.

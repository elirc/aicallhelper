# ADR 015 — Session outcomes, adoption reconciliation, and terminal answer outcomes

**Status**: accepted (2026-09-22; implements review actions R1 and R2)

## Context

Two contracts that each passed their own tests failed when composed.

- **R1, adoption.** `start_session` and `ask` start work in the core before
  the command response reaches the webview. The frontend only learns the id
  when the invoke resolves, and the reducer drops every event whose id it has
  not adopted (ADR 011). An error or a whole answer that fired in that window
  (an immediate STT connect failure during the ~100 ms device open, the local
  provider's synchronous oversize rejection, an instant answer) was dropped,
  and the UI waited on a session that had already ended. `bridge.on` also
  swallowed listener-registration failures, so a start could run with nobody
  listening. A panicking provider left the session Active in the slot: the UI
  hung and the next Record "superseded a ghost".
- **R2, completion.** Both cloud providers returned `Ok` at clean HTTP end of
  stream if any SSE event had been decoded. A ping-only stream was a blank
  success; a stream cut before the provider's terminal event was a complete
  answer; Groq ignored structured error frames and appended text that arrived
  after `data: [DONE]`. The SSE decoder and the error-body reads had no byte
  limit.

The review refinements (FINAL-REVIEW §1–2) require: complete outcomes
(including early success), per-id bounded retention, idempotent guarded
reconciliation, no lost or duplicated pre-adoption text, visible listener
readiness, a panic-safe driver guard, the documented terminal event as the
only proof of completion, and a per-entry outcome in the UI.

## Decision

**Outcome log in the core.** `SessionManager` keeps a `SessionOutcome` per id
(`active | completed{transcript, answer, metrics, stopReason} |
failed{error, transcript, partial} | cancelled | unknown`) in the SAME mutex as
the one-session slot (`core/src/session/machine.rs`, `Slot`). A claim records
`active`, and every slot release records the terminal outcome in the same
critical section, so `is_active` and `outcome` can never disagree. Settling is
once-only: only `active` may change. The log is bounded at
`OUTCOME_RETENTION` = 16 ids, oldest retired first. An outcome is retired only
when 16 newer sessions have been claimed; the UI runs one attempt at a time
and asks only about the attempt it is adopting, so retirement cannot hit a
pending adoption. Retired or never-issued ids read `unknown`. `failed.partial`
is exactly the text the gate let through (check, emit and record happen under
the gate's lock), so a reconciled failure keeps the text the user saw.

**Envelope and lookup.** `start_session` and `ask` resolve with
`SessionStart { sessionId, outcome }`, the outcome read after the device open.
An already-failed session is still an ok envelope, because the start itself
succeeded and the UI settles the adopted attempt from the outcome. A new
`session_outcome(sessionId)` command is the adoption-time lookup: it is
read-only and idempotent, and the hook calls it once when the envelope said
`active`.

**Pre-adoption text: bounded replay (chosen over snapshot/sequence numbers and
reserve/adopt/activate).** While a start/ask call is in flight, `useSession`
holds session events in a per-attempt buffer instead of dispatching them.
Consecutive deltas of one id merge, consecutive partials of one id replace, and
audio levels are not held, so the buffer is capped at 64 entries. At the cap
it evicts the oldest event of any id other than the newest one first (ids only
grow, so the newest is the one about to be adopted). If the adopted id itself
lost an event, its held deltas are not replayed, and the outcome settles the
text instead (a `failed` partial replaces any fragment it contains). On adoption it dispatches `record/started`/`ask/accepted` and then
replays only the events tagged with the adopted id, in order. Each event is
applied exactly once, either held and replayed or dispatched live, so nothing
is lost or duplicated. Events of any other id are discarded, which keeps
ADR 011's stale-drop. The reducer itself still drops everything before
adoption. Replay needs no change to the event wire shapes; sequence numbers
would have changed every listener.

**Reconciliation is guarded and idempotent.** The reducer action
`session/outcome {key, id, outcome}` applies only when both the attempt key and
the adopted id match the live attempt. A live terminal event and a lookup can
both land, and the first wins while the second is a no-op. An older lookup can
never settle a newer attempt. `unknown` for an adopted id settles with "Lost
track of that answer…" rather than waiting.

**Listener readiness is part of the start contract.** `bridge.on` returns
`{ ready, unsubscribe }`. `ready` resolves to an envelope, and a registration
failure is an `internal` error naming the event. The hook awaits the
session-event subscriptions before calling `start_session`/`ask`; on failure
the attempt fails visibly and the next attempt re-subscribes. The wait is
bounded: after `LISTEN_TIMEOUT_MS` (3 s) a registration that never settles
counts as failed ("The app could not start listening for session events.
Restart the app."), and the next attempt re-subscribes instead of awaiting
the same dead promise. Only the four session events gate a start; failures of
the hotkey and `audio:level` subscriptions are shown but do not block Record.

**Driver supervision.** Each driver task owns a `DriverGuard` whose `Drop` runs
on every exit, including a panic unwinding out of a provider or connector. It
calls `fail(id, internal "The answer stopped unexpectedly. Try again.")`,
which is a no-op unless the session still owns the slot, so cancellation and
supersession stay silent. It then cancels the session token and aborts the STT
stream if the driver still held it. The settle (gate, slot, outcome) is
synchronous and cannot panic; the `session:error` emit is not. During a panic
unwind the guard hands that emit to a fresh tokio task, because the sink is
outside the core's control (Tauri's emit path locks with `unwrap`), and a
second panic inside a destructor during unwinding aborts the process. The gate
records a delta or partial only after the sink accepted it, so a
sink-side panic never leaves `failed.partial` claiming text the UI never
received. Every lock it takes is poison-tolerant, so
the guard cannot panic during an unwind. `OwnedStream` makes every abort path
(teardown, cancel arm, device error, guard) tear the socket down exactly once.
The shell's capture release stays keyed off the terminal event, which the
guard now always produces.

**Terminal answer outcomes (R2).** `LlmProvider::stream_answer` returns
`Answer { text, stopReason }`. Completion is the protocol terminator only:
Anthropic `message_stop`, Groq `data: [DONE]`, Ollama `done: true`. Nothing
after it is read. `stop_reason` / `finish_reason` / `done_reason` are
metadata. `max_tokens` and `length` map to `token_limit`, and everything else
maps to `complete`.

| Stream | Result | UI entry status |
| --- | --- | --- |
| Terminator, usable text | `Ok(Answer)`; `llm:done` with `stopReason: "complete"` | `completed` |
| Terminator, token-limit reason | `Ok(Answer)` with `token_limit` | `limited` ("cut short") |
| Error frame, malformed frame, oversized frame, or clean EOF before the terminator | `Err(llm_http)`; streamed deltas stay on screen | `incomplete`, reason = the message |
| Terminator without usable text | `Err(llm_http)` "…finished without any answer text" | `incomplete` (no blank success) |
| Superseded / cancelled / `aborted` | silent | `cancelled` |

The SSE decoder caps a line and an event at 1 MiB each, checked per byte
while reading. An overflow returns the events already framed earlier in the
same feed, so the providers apply them before failing, and the kept partial
text does not depend on network chunking. Error bodies are read through `http::read_error_body`, capped at
16 KiB while streaming. The deadlines (§3) and the no-retry-after-a-visible-
delta rule (ADR 006) are unchanged.

**Per-entry status.** `HistoryEntry` gains `status` (`pending | completed |
incomplete | cancelled | limited`) and `reason`. The AnswerPanel head shows an
`incomplete` / `cut short` / `stopped` tag, and a caption under the answer
shows the reason. `metrics` stays timing only.

## Consequences

- Every start/ask costs one extra read-only IPC round trip (`session_outcome`)
  when the envelope says `active`. It is off the stop-to-first-word path.
- A malformed frame now fails the answer instead of silently dropping text.
  One bad frame from a provider becomes a visible "incomplete" answer: honest,
  but stricter than before.
- Groq text after `[DONE]` is no longer appended (the old test was reversed on
  purpose).
- The first Record/Ask after mount waits for listener registration, which
  normally finishes in microseconds before the user can click.
- The replay buffer lives in the hook, not the reducer. The reducer's
  drop-before-adoption rule is unchanged and still tested.

## What would change the call

- A second client of the IPC surface, or events that must survive a webview
  reload, would call for sequence numbers on events and a snapshot lookup
  instead of an in-memory replay buffer.
- More than one concurrent session would call for a real per-session registry
  instead of the one-slot + bounded log.

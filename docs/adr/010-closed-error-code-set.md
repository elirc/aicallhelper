# ADR 010 — A closed error-code set as the UI contract

**Status**: accepted

## Context

The frontend needs to *behave differently* per failure kind: `aborted` must
never render (it means the user superseded or cancelled on purpose), the
key-missing codes nudge toward Settings, everything else shows its message in
the error box. That means errors crossing the Rust→TS boundary must be
branchable — and v2 demonstrated the failure mode of not designing this:
free-form errors degraded into "show the raw exception text", leaking stack
noise at a user mid-interview
(`src-tauri/core/src/error.rs:1-6`).

String codes assembled ad hoc have a second, quieter failure: a typo'd code
doesn't error, it just falls through every `switch` arm and silently becomes
the default behavior.

## Decision

- One closed enum, fourteen codes, defined in exactly one place
  (`core/src/error.rs`): `no_stt_key · no_llm_key · stt_connect · stt_error ·
  stt_timeout · no_speech · llm_auth · llm_http · llm_rate_limit ·
  llm_first_token_timeout · llm_timeout · aborted · internal ·
  settings_conflict` (SPEC §4). The fourteenth, `settings_conflict`, was
  added in 2026-09 (ADR 016) because the UI must act on it differently from
  a failed save: keep the draft and offer a reload.
- The **wire strings are pinned by test on both sides**
  (`error.rs:111-120`; the frontend switches on the same literals), so a
  rename cannot silently change behavior across the IPC boundary.
- **Messages are for the person on the call**, not a log reader: they say what
  to do next wherever that is knowable ("Open Settings (gear icon) and add
  it." — canonical messages at `error.rs:94-104`), and never raw exception
  text when avoidable.
- `aborted` is structurally special: it is the pipeline's non-error way to
  unwind, constructed via `AppError::aborted()` and detectable via
  `is_aborted()` (`error.rs:69-81`); the state machine strips it before
  emission (`machine.rs:175-183`) and the frontend drops it again
  (`src/state/reducer.ts:140-154`) — belt and braces.
- Provider HTTP statuses map to codes through explicit matrices (SPEC §6.2,
  §6.3), including the honesty rules: report the *actual* status (a 403
  labelled 401 sends the user debugging the wrong thing), and a Groq 404 says
  "the model may have been retired" because that is the likely cause.

## Consequences

- Every new failure mode forces a deliberate decision — which code, what
  actionable message — instead of defaulting to leak-whatever-we-caught.
- The UI's behavior matrix is finite and testable: the frontend suite drives
  every code through the mocked bridge.
- Two codes exist purely because the *user's fix* differs: `no_speech` (fix
  your audio) vs `stt_error` (the socket died) — collapsing them was v2's way
  of sending users to debug the wrong thing (SPEC §5.5).

Costs, honestly:

- Adding a code is a cross-boundary change: Rust enum, wire-string test, TS
  type, UI branch, TESTING.md. That friction is mostly the point, but it is
  friction.
- The codes are deliberately coarse — `llm_http` covers everything from a 500
  to a connection drop — so the *message* carries the detail. That splits the
  contract in two: codes for behavior, messages for humans, and both have to
  be maintained.

## If revisited

Add a code only when the UI needs a genuinely different *behavior*, not for
taxonomy. If the set ever grows past what the UI branches on, that growth is
the signal the detail belonged in messages. If a second consumer appears
(logging, analytics), resist reusing these codes for it — they are a UI
contract, and overloading them re-creates the coupling this ADR removed.

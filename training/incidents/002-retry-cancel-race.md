# INC-002 — Pressing Stop mid-answer doesn't always stop the Anthropic request

**Branch:** `training/incidents/002-retry-cancel-race` (based on `f1cf524`)
**Reported by:** you, while investigating INC-001 (this is a related but *separate* bug in
a different layer — don't assume the same fix applies)
**Severity:** as filed — Sev2 (intermittent, timing-dependent, real API cost + a possible
garbled/duplicate answer on the rare case it "succeeds")

## What was observed

While auditing session cancellation after INC-001, a teammate stress-tested the Stop path
by yanking their network adapter mid-answer (to simulate a flaky connection) and hitting the
in-app Stop/supersede action at the same moment, repeatedly, in a loop. On roughly 1 in 20
tries, `cargo test -p app-core --lib llm::retry::` fails locally with:

```
thread '...cancellation_during_the_first_attempt_yields_aborted_not_the_http_error' panicked at core\src\llm\retry.rs:297:9:
assertion `left == right` failed
  left: [0, 1]
 right: [0]
```

That's a **deterministic unit test**, not a flaky network repro — it doesn't touch a real
socket. If it's failing at all, the bug is unconditional, not a rare race; the "1 in 20"
in manual testing was just how often a human's timing happened to line up with the failure
mode the test exercises directly every time.

## What you have

- The failing test's name and file (`src-tauri/core/src/llm/retry.rs`), and the assertion
  above.
- `cargo check -p app-core` — compiles clean, no type errors. This is a logic bug, not a
  type-system-catchable one.
- The module doc at the top of `retry.rs` (read it — it states the intended contract in
  plain language, including *why* each check exists).
- The full commit that this branch adds over `f1cf524` — again, try to localize from the
  test failure and the module doc before reading the diff.

## Your task

1. Read `with_retry_once` in `retry.rs` end to end. It has (or is supposed to have) **three**
   separate cancellation checks. Find them. What distinct moment does each one guard?
2. Explain in one sentence why `calls == [0, 1]` instead of `[0]` means a **second live HTTP
   request actually went out to Anthropic** after the user cancelled — not just "the test
   counted wrong."
3. Given the "never retry after a delta" and "never retry an HTTP status" guards are both
   still intact, name the exact narrow window this bug opens: what has to be true of the
   *first* attempt's failure for a cancelled call to retry anyway?
4. Write the fix (diff-shaped is fine) and say which of the three cancellation checks it
   restores.
5. This is the Rust counterpart to INC-001's frontend bug. Write one sentence on why a fix
   at *this* layer is more valuable than a fix at the frontend layer alone — think about
   what other callers of `stream_answer` (there's more than one provider) get for free.

Seal your answer, then open `training/_answers/incidents/002-retry-cancel-race.md`.

## Verification note for whoever built this incident

`cargo check -p app-core` passes clean on this branch (compiles). `cargo test -p app-core
--lib llm::retry::` reproduces the exact failure quoted above (7 passed, 1 failed,
`left: [0, 1]` vs `right: [0]`) — both commands were actually run against this branch, not
inferred.

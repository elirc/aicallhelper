# INC-002 — sealed answer

## Root cause

`src-tauri/core/src/llm/retry.rs`, `with_retry_once()`. The suspect commit deleted this
block:

```diff
     // Cancellation may be *why* the attempt failed (the provider aborts its
     // request on cancel). Either signal — the token or an aborted error —
     // means the user moved on, and no error may outrank that.
-    if cancel.is_cancelled() {
-        return Err(AppError::aborted());
-    }
     if first_err.is_aborted() {
         return Err(first_err);
     }
```

## The three cancellation checks, and what each one actually guards

`with_retry_once` is supposed to check cancellation at three distinct moments — they are
*not* redundant despite looking similar, which is exactly what makes "simplify away the
middle one" a plausible-looking but wrong refactor:

1. **Before attempt 0** (top of the function, untouched): don't spend a request on work
   already cancelled before it started.
2. **After attempt 0 fails, before deciding to retry** (the block deleted above): the token
   may have flipped *while attempt 0 was in flight*, and the resulting error may or may not
   have been mapped to `AppError::aborted()` by the provider — `AnthropicProvider::stream_once`
   races `cancel.cancelled()` against the network call with `tokio::select! { biased; ... }`
   (`anthropic.rs:140-146`), so cancellation *usually* wins and produces `aborted()`
   directly. But not always: a connection that was already mid-failure for an unrelated
   reason (a stream drop, a refused connect) can resolve to a plain `LlmHttp` /
   `MSG_STREAM_DROPPED`/`MSG_CONNECT_FAILED` error in the same instant the user's Stop
   click flips the token — `first_err.is_aborted()` is false for that error, so without
   this check the code falls through to the retry decision as if nothing had asked to stop.
3. **After attempt 1 fails** (bottom of the function, untouched): same idea, closing the
   loop if the *second* attempt also raced a very late cancel.

Removing check #2 leaves a real gap: check #1 doesn't cover it (it's checked before attempt
0 was ever an issue), and check #3 only fires if attempt 1 also *fails* — if attempt 1
*succeeds*, the function returns `Ok(answer)` with no cancellation check at all after that
point.

## Why `calls == [0, 1]` means a real second HTTP request, not a counting artifact

`run` (the closure passed into `with_retry_once`) is exactly `self.stream_once(...)` per
attempt index — see `anthropic.rs:366-373`. It isn't a mock in production; in the test it's
a stand-in, but the *shape* of what's being exercised is "does `with_retry_once` invoke this
closure a second time." In production that closure opens a fresh `POST /v1/messages` with
`stream: true` to Anthropic (`stream_once`, `anthropic.rs:130-146`). If `run(1)` executes at
all, a second real connection was opened and a second real generation started — regardless
of whether that generation is ever rendered, it is billed the moment Anthropic starts
producing `content_block_delta` events for it. The test's `calls` vector is a direct proxy
for "how many times `run()` — i.e., how many times a live request — was dispatched." `[0, 1]`
instead of `[0]` is not a bookkeeping detail; it's the unit test's only way to observe "a
second billed request fired" without actually hitting the network.

## The exact narrow window

For the bug to bite, all of these have to be true simultaneously:

1. The user cancels (Stop, or a supersede) **while attempt 0 is in flight**.
2. Attempt 0's resulting error is **not** literally `AppError::aborted()` — i.e., the
   `select!` race in `stream_once` didn't happen to land on the cancel arm, or the failure
   was a genuine connection-level fault that coincided with the cancel.
3. **No delta has reached the sink yet** (`attempt.delta_emitted()` is false) — if any text
   already streamed, the pre-existing concatenation guard (`if attempt.delta_emitted() {
   return Err(first_err); }`, untouched by this bug) already blocks the retry for an
   unrelated, correct reason.
4. The error is one `is_retryable` accepts — i.e., genuinely looks like a pre-response
   connection failure (the exact class the retry policy exists to recover from).

That's a real, if narrow, production window: "user cancels right as a flaky connection was
about to fail anyway, before any text arrived." Flaky networks and impatient users cancelling
early are not rare in combination — this isn't a one-in-a-million race.

## The fix

Restore the deleted block exactly where it was — between the `first_err` match and the
`first_err.is_aborted()` check, so a cancellation observed via the *token* (not just via the
error shape) short-circuits before the retryability decision is even consulted:

```diff
+    if cancel.is_cancelled() {
+        return Err(AppError::aborted());
+    }
     if first_err.is_aborted() {
         return Err(first_err);
     }
```

Verified: `cargo test -p app-core --lib llm::retry::` on the incident branch shows `7
passed; 1 failed` — `cancellation_during_the_first_attempt_yields_aborted_not_the_http_error`
fails with `left: [0, 1]  right: [0]`, i.e. it proves the extra attempt fires. Restoring the
three lines above should return it to `8 passed; 0 failed` (not independently re-run after
the fix as part of this exercise — that's your verification step).

## Why the Rust-layer fix matters more than a frontend-only fix

`with_retry_once` is the **shared** retry policy — both `AnthropicProvider` and
`GroqProvider` route every attempt through it (`anthropic.rs:373`, and the equivalent call
in `groq.rs`). A frontend fix (making sure `cancelSession` is always called, as in INC-001)
stops the *session* from being tracked as live, but it does not stop an in-flight retry
that's *already past* the frontend's cancel call from firing a second HTTP request on the
Rust side — the two bugs are independent and this one exists even when the frontend
behaves perfectly. Fixing it here protects every current and future LLM provider that goes
through this policy, not just the one code path a frontend fix happens to cover.

## Interview-ready one-liner

"A retry helper had three cancellation checks at three different moments, and a
well-intentioned 'this looks redundant' cleanup removed the middle one — the one covering
the exact window where a user's Stop races a connection failure that hadn't yet been mapped
to the `aborted` error variant. The unit test suite already had a targeted regression test
for precisely this race; it just wasn't run before merge. The fix is three lines; the real
lesson is that in a retry/cancel policy, checks that look like duplicates of each other are
usually guarding *different moments*, not the same one — read the comment that explains
*why* before you delete anything that looks redundant."

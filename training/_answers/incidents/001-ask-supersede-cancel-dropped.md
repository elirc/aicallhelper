# INC-001 — sealed answer

## Root cause

`src/state/useSession.ts`, `submitAsk()`. The suspect commit removed this line from the
supersede path:

```diff
     // Same supersede as startFlow: drain a buffered delta before ask/start
     // retires the streaming entry.
     flushRef.current();
-    if (s.ui === 'answering' && s.activeId !== null) bridge.cancelSession(s.activeId);
     keyCounter.current += 1;
```

The commit message on the incident branch (`git log -p -1`) is a plausible-looking
"consolidate the two call sites to look more alike" refactor. It isn't a refactor — it's a
silent behavior deletion. `startFlow()` (the Record/toggleRecord path) still has its own
identical guard a few lines up and was untouched, which is exactly why the symptom is
narrow: **only** "ask a follow-up while an answer is streaming" lost its cancel; "start
recording over a streaming answer" still cancels correctly. That asymmetry is the strongest
diagnostic signal in the whole incident — if only one of two structurally identical guards
misbehaves, suspect a diff that touched one call site and not its twin.

## Why this produces the Anthropic-console symptom

`bridge.cancelSession(id)` (`src/bridge.ts:90-93`) is the *only* thing that tells the Rust
core "stop the in-flight Anthropic request for this session." It's fire-and-forget by
design (comment: "cancellation legitimately races session teardown"), but it is not
optional — without it, nothing on the Rust side ever learns the user moved on.

Trace it through: `cancel_session` (`src-tauri/src/commands.rs:440`) calls
`state.sessions.cancel(session_id)`, which flips the `CancellationToken` the
`AnthropicProvider::stream_answer` call is racing against in `tokio::select!` (see
`anthropic.rs:140-146`, `:169-173`). With the frontend never issuing `cancelSession` for the
superseded `session_id`, that token is never cancelled, so the Rust-side SSE read loop just
... keeps reading. The HTTP connection stays open, Anthropic keeps generating and billing
`output_tokens`, and the stream runs to `message_stop` exactly as if nobody had asked a
follow-up — because from the API's point of view, nobody told it otherwise.

Meanwhile the **frontend is completely fine to look at**: the reducer's `isCurrent()` guard
(`reducer.ts:207-209`) drops every `llm:delta`/`llm:done` for the old `sessionId` once
`activeId` has moved on to the new session (`ask/start` → `ask/accepted`). So the user sees
exactly what they expect — the second answer, cleanly. The bug is entirely invisible from
inside the app. It only shows up as a billing anomaly, which is why support routed it as a
cost question, not a bug report.

## The regression test that already existed — and should have caught this

`src/state/useSession.test.ts:441` — `'asking during a streaming answer supersedes it: old
session cancelled, its events dropped'` — and a second one at `:874` in the delta-coalescing
block, both assert `expect(fake.bridge.cancelSession).toHaveBeenCalledWith(1)` after a
second `submitAsk` while the first is `answering`. Running
`npx vitest run src/state/useSession.test.ts` on this branch fails exactly these two, with
"Number of calls: 0" — the assertion that would have blocked this PR in CI. **This incident
should never have reached a branch that ran the test suite.** The real "how did this ship"
question: either CI wasn't run on this commit, the failing tests were skipped/quarantined,
or (most likely, given the plausible-looking commit message) someone reviewed the diff
without running `npm test` locally and CI hadn't caught up yet. Ask that question in the
retro, not just "what's the code fix."

## The fix

Restore the deleted line (or better: extract the shared guard into a helper both `startFlow`
and `submitAsk` call, closing off this exact "one call site drifts from its twin" failure
mode permanently):

```diff
     flushRef.current();
+    if (s.ui === 'answering' && s.activeId !== null) bridge.cancelSession(s.activeId);
     keyCounter.current += 1;
```

## Additional regression test worth adding

The existing tests prove `cancelSession` is *called*. Neither one proves the old session's
answer stops rendering *before* the new question exists in history, nor asserts anything
about "no further billing" (that's outside what a frontend unit test can observe — it can
only prove the IPC call happened, not that Rust honored it end to end). A useful addition:
an integration-shaped test (Rust side, `session/machine.rs` test harness) that asserts a
provider fake's `stream_answer` future is actually dropped/cancelled — not just that
`cancel()` was called on the manager — when a session is superseded via `ask`. That closes
the gap between "we called cancel" and "cancel actually stopped the billed work," which is
the real invariant INC-001 broke.

## Interview-ready one-liner

"A refactor that looked like deduplication silently deleted one of two symmetric
cancel-on-supersede calls. The bug was invisible in the UI because the frontend's stale-id
drop logic (`isCurrent()`) hides superseded output regardless of whether the backend
actually stopped generating it — so a passing-looking app was quietly leaving live LLM
requests running (and billing) in the background. Two existing unit tests already asserted
the missing call; the incident was a process failure (unrun/ignored tests) as much as a code
failure."

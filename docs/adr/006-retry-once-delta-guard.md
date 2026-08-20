# ADR 006 — Retry exactly once, vetoed by the delta-emitted concatenation guard

**Status**: accepted

## Context

ADR 005 creates the very failure it must then absorb: a pooled connection can
be reaped by the far end between warm and use, and the answer request finds it
dead at first byte. That specific failure — connection-level, before the
server ever answered — deserves one immediate retry, because a fresh
connection almost certainly fixes it.

Every other retry makes things worse:

- An **HTTP error status** means the server heard us and said no. A 401 will
  be a 401 again; a 429 asked us to back off; an instant repeat burns the
  10 s first-token budget on a request already known to fail.
- A retry **after any delta reached the UI** is catastrophic in a specific
  way: the UI *appends* deltas, so a second attempt streams a second full
  answer onto the end of the first's partial text. A truncated answer is
  recoverable by the user; a concatenated one is gibberish
  (`src-tauri/core/src/llm/retry.rs:14-18`).
- A retry **after abort** resurrects an answer nobody wants and races the one
  they do.

## Decision

One shared helper both providers route through:
`with_retry_once(cancel, attempt, is_retryable, run)`
(`retry.rs:76-128`), with the provider supplying only the "was this a
connection-level failure?" predicate.

- **The guard is a shared `Attempt` flag** flipped on the first delta and read
  after the attempt's future completes (`retry.rs:42-60`). It is the one bit
  of state that distinguishes "died before anything happened" (retryable) from
  "died mid-answer" (never).
- **Cancellation outranks everything**: checked before any attempt, and after
  any failure — an abort-provoked transport error must surface as `aborted`
  (which the UI never renders), not as a scary HTTP error for work the user
  cancelled (`retry.rs:88-104`).
- **The retried request is byte-identical**: the helper hands the closure only
  an attempt index; the body is built once and borrowed
  (`retry.rs:21-25`, `AnswerRequest` at `llm/mod.rs:61-69`). A rebuilt body
  can differ (map ordering), and on Anthropic a differing prefix silently
  misses the prompt cache (ADR 007) — turning the retry into the slowest
  request of the day. Pinned by test down to allocation identity
  (`retry.rs:331-364`).
- Predicates recognize connection failures by **equality against message
  constants** (`anthropic.rs:33-37`, `groq.rs:31-35`) rather than substring
  matching.

## Consequences

- The stale-pooled-connection failure self-heals invisibly; the user sees a
  slightly slower first word instead of an error.
- Concatenated answers are impossible by construction, not by hoping the
  network fails politely.
- One policy, one test matrix (`retry.rs` tests cover every rule), two
  providers.

Costs, honestly:

- A **mid-stream** connection drop is surfaced as an error with the partial
  answer kept — deliberately not recovered. The UI keeps what streamed
  (SPEC §9 "an error during a streaming answer keeps the partial answer"),
  and the user can Regenerate.
- Exactly one retry means a genuinely flaky network still fails; this tool
  runs on the same network as the call itself, so if the network is that bad
  the user has bigger problems.
- Coupling: the predicates key off message constants, so renaming an error
  message can silently change retry behavior. The constants are `const`s next
  to a comment saying exactly this.

## If revisited

If a provider ever offers resumable streaming (continue from token N), the
delta guard relaxes from "never retry after a delta" to "resume after a
delta" — the `Attempt` flag would then carry the byte offset instead of a
boolean. Provider-side idempotency keys would make retrying *requests* safer
but would not fix concatenation, which is a client-side display problem; the
guard stays regardless.

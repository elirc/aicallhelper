# Technical questions — AI Call Assistant v3

10 questions, all specific to this codebase. Write your own answer before checking
`training/_answers/interview-technical-questions.md`. Questions 6-10 are a focused set on
**latency budgets for AI features** — the framing every one of these questions is really
testing is "can you reason about where time goes, put a number on a budget, and design for
graceful degradation when the budget is blown," using this app's actual numbers.

1. **Streaming architecture.** `AnthropicProvider::stream_answer` forwards every
   `content_block_delta`/`text_delta` to the sink "the moment it is decoded" (see the module
   doc at the top of `anthropic.rs`). Name the one thing in this codebase that *would* have
   silently violated that invariant if it existed, and where you'd look to prove it doesn't.

2. **Cancellation semantics.** Walk the full path from a user clicking Stop mid-answer to
   the Anthropic HTTP connection actually closing. Name every layer the signal crosses
   (React state → bridge call → Tauri command → session manager → `CancellationToken` →
   provider `select!`). Where in that chain did INC-002 break it?

3. **Retry policy.** `with_retry_once` retries "exactly once, and only when it is provably
   safe." List the three vetoes (there are three, not counting the up-front cancellation
   check) and, for each, describe a concrete production scenario where skipping that veto
   would produce a visibly broken answer to the user.

4. **Database/state.** This app has no database — session state lives in an in-process
   `Mutex<Slot>` with one active session and a bounded ring of recent outcomes
   (`OUTCOME_RETENTION = 16`, `session/machine.rs`). Why is a bounded VecDeque the right
   structure here instead of, say, a HashMap keyed by session id with no eviction? What
   breaks if `OUTCOME_RETENTION` were 1 instead of 16 — trace it through the
   adoption-reconciliation flow in `useSession.ts`'s `adopt()`.

5. **Testing.** `docs/REVIEW.md`'s own honest assessment: "every state-machine test runs on
   a paused, current-thread runtime" — deterministic *cooperative* interleavings, never true
   OS-thread parallelism. Name one specific bug class this structurally cannot catch (the
   review names one: `push_audio` from the real-time capture thread racing `stop`). Why does
   adding more test *cases* on this harness not close that gap?

### Latency budgets for AI features

6. **State the budget.** This app's product promise is "~1s stop-to-first-word." Break that
   budget into the pipeline stages it has to cover (STT finalize → LLM connect/prewarm →
   time to first token) using this repo's own constants: `limits::STT_FINALIZE` (5s
   *ceiling*, not typical case), `LLM_FIRST_TOKEN` (10s timeout), the `prewarm()` call in
   `commands.rs` fired the moment recording starts. If STT finalize typically takes 200-400ms
   and first-token typically lands in 300-600ms after that, where does the *typical* case
   leave headroom against the 1s promise, and where do the *timeout* constants (5s, 10s) tell
   you the budget is allowed to blow past 1s under degradation?

7. **Why prewarm, and why does it matter for the budget.** `deps.llm.prewarm()` is called in
   `commands.rs` "the instant recording starts" (per the comment), not when the question is
   asked. What part of the 1s budget does this buy back, specifically? What would you
   measure to prove prewarm is actually saving time rather than being a no-op (the codebase
   has exactly this kind of honesty check already, for a different feature — which one, and
   what's the analogous measurement for prewarm)?

8. **Batching vs. first-word latency — the direct trade-off.** The delta coalescer
   (`DELTA_COALESCE_MS = 16`) intentionally does NOT delay the *first* delta of a burst — it
   dispatches synchronously and only coalesces what arrives inside the 16ms window after.
   Why is that asymmetry (leading-edge-immediate, trailing-edge-batched) the correct design
   for a product whose promise is about first-word latency specifically, rather than average
   latency? What would go wrong for this product if someone "simplified" it to a flat 16ms
   batch window applied uniformly to every delta?

9. **Degradation under a slow/rate-limited provider.** Anthropic returns 429 (rate limit) or
   529 (overloaded) — both mapped to specific `ErrorCode`s in `anthropic.rs::map_status`, and
   neither is retried (`is_retryable` only accepts pre-response connection failures, never an
   HTTP status). Given the 1s latency promise, is "surface the error immediately, don't
   retry" the right call here, or would you want the retry-once policy to also cover 429/529
   with backoff? Argue both sides, then say what you'd actually ship and why (this is a
   trade-off question — the "right" answer is a defensible one, not a specific one).

10. **Local/offline fallback and its latency budget.** `limits::LOCAL_FIRST_TOKEN` is 90s and
    `LOCAL_TOTAL` is 300s — roughly 9x and 5x the cloud limits respectively. What does that
    ratio tell you about what "latency budget" means once you're choosing between a cloud API
    and an on-device model, and why would presenting both under one product promise ("~1s
    stop-to-first-word") without disclosing which mode you're in be a UX/trust problem this
    codebase would need to solve explicitly (check whether/how the UI actually surfaces which
    provider is active — is this a real gap or already handled)?

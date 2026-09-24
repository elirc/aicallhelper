# Technical questions — reference answers

Grade yourself for mechanism, not vocabulary. "Because caching" is not an answer; "because
X is checked before Y, so Z can't happen" is.

1. **Streaming architecture.** The delta coalescer in `useSession.ts` (`DELTA_COALESCE_MS`)
   is exactly the kind of thing that *would* have violated "every delta reaches the sink the
   moment it is decoded" if it lived on the Rust side — but it deliberately doesn't. It only
   buffers dispatches to the React reducer, after the Rust→JS event boundary; the Rust
   provider itself (`anthropic.rs`) still calls `sink.on_delta(text)` synchronously per
   decoded chunk, unbatched. Look at `anthropic.rs`'s `apply_event` (never buffers) vs.
   `useSession.ts`'s `onDelta` (buffers within a 16ms window) to see the invariant is upheld
   on one side of the IPC boundary and intentionally relaxed on the other, for a
   rendering-cost reason, not a product-promise one.

2. **Cancellation semantics.** `toggleRecord`/`submitAsk` (React) → `bridge.cancelSession(id)`
   (`bridge.ts:90`, fire-and-forget `invoke('cancel_session', ...)`) → `cancel_session` Tauri
   command (`commands.rs:440`) → `state.sessions.cancel(session_id)` (`session/machine.rs`) →
   flips the session's `CancellationToken` → `AnthropicProvider::stream_answer`'s
   `tokio::select! { biased; _ = cancel.cancelled() => ... }` in `stream_once` wins the race
   and returns `AppError::aborted()`. INC-001 broke the *first* hop (the frontend never
   called `cancelSession` for the superseded id). INC-002 broke a check *inside*
   `with_retry_once`, downstream of the same token, in the narrow case where attempt 0's
   failure race lands on a non-`aborted` error just as cancel fires.

3. **Retry policy — three vetoes.**
   - *HTTP status ⇒ never retry*: the server answered; a 401 is a 401 again, and repeating a
     429 just burns the first-token budget on a request you already know fails.
   - *A delta already reached the UI ⇒ never retry*: the UI **appends**; a second attempt
     would stream a second answer onto the first's partial text, producing gibberish worse
     than a clean truncation.
   - *An abort ⇒ never retry*: the user moved on; resurrecting an answer nobody wants races
     the one they do.
   Skipping veto 1 wastes latency/cost on a deterministic failure. Skipping veto 2 produces a
   visibly garbled, concatenated answer. Skipping veto 3 (INC-002's bug, via the missing
   *cancellation* check that feeds this decision) fires a live request nobody asked for and,
   worst case, races a superseding session's own answer onto screen.

4. **State.** A bounded ring beats an unbounded map because the only reader is
   `useSession.ts`'s `adopt()`, which needs the outcome of *the one id it's currently
   waiting on* — a lookup, not an iteration — and outcomes past a small recent window are
   worthless (the UI has long since moved on). Unbounded storage would be a slow memory leak
   for a long-running session. If `OUTCOME_RETENTION` were 1: any session whose outcome
   lookup is delayed by even one more session completing (e.g., a rapid double-supersede)
   would find its outcome already evicted — `adopt()`'s `sessionOutcome(id)` call would come
   back "not found," which the reducer maps to `LOST_SESSION_ERROR` ("Lost track of that
   answer before it finished") even though the session actually completed fine. 16 is a
   safety margin against exactly that eviction race, not an arbitrary number.

5. **Testing.** The specific bug class: a genuine OS-thread data race — e.g. `push_audio`
   called from the real-time capture callback thread landing *between* two operations a
   single-threaded cooperative scheduler would always execute atomically (like a stop
   flipping a flag while a frame is mid-flight into the channel). `start_paused = true` gives
   deterministic *interleavings among awaits on one thread*; it cannot manufacture the actual
   concurrent-write timing a second OS thread produces. Adding more test cases on the same
   harness explores more *orderings of the same single thread*, which is a different
   dimension entirely from "does this survive two threads touching shared state at once" —
   no amount of additional single-threaded cases closes a multi-threaded gap. You'd need a
   real multi-threaded stress test or a loom-style exhaustive-interleaving tool.

### Latency budgets for AI features

6. **The budget.** Stages: (a) STT finalize — `CloseStream` round-trip to Deepgram, typically
   a couple hundred ms of network + smart_format flush, capped at `STT_FINALIZE` = 5s as a
   *worst-case ceiling*, not the expected path; (b) LLM connect — mitigated to ~0 by
   `prewarm()` firing at record-start, well before the answer request exists; (c) time to
   first token — Haiku 4.5 typically first-tokens in a few hundred ms once the connection is
   warm, capped at `LLM_FIRST_TOKEN` = 10s. Typical case: ~200-400ms (STT finalize) +
   ~300-600ms (first token) ≈ 500ms-1s — right at the edge of the ~1s promise, which is why
   prewarm and the two-block cached-prompt design (ADR 007) exist: they're the two levers
   that keep the *typical* case inside budget. The 5s/10s constants aren't the target — they're
   the "give up and report a real error instead of hanging forever" ceiling for the tail,
   sized generously so a real but slow network isn't mistaken for a hang.

7. **Prewarm.** It buys back the TLS/TCP handshake and connection-pool warmup — the part of
   "LLM connect" that happens once per idle-to-active transition, not per request — by doing
   it during the ~seconds a user is talking, off the critical path entirely. Comment in
   `anthropic.rs` even notes the honesty risk: "the pool lives inside the Client, so the warm
   and the answer must share one instance — otherwise prewarm silently does nothing" (ADR
   005). The analogous honesty check already in the codebase: `last_cache_read_input_tokens`
   on `AnthropicProvider` — it doesn't trust "the code that should engage the cache ran," it
   reads `usage.cache_read_input_tokens` back from the actual API response to prove the
   cache engaged. The equivalent for prewarm would be timing the actual first request's
   connect phase (e.g. via a `reqwest` trace/timing hook) and confirming it's near-zero, not
   just trusting that `prewarm()` was called.

8. **Batching asymmetry.** Leading-edge-immediate matters because the product's *specific*
   promise is about the first word, not the average word — a user perceives "the app is
   fast" the instant text starts appearing, and every subsequent millisecond of buffering is
   nearly free perceptually (they're already reading). Batching the *first* delta into a flat
   16ms window would add a fixed, pointless 16ms to literally every single answer's
   perceived start, for a benefit (fewer renders) that only matters once a burst is already
   underway. If someone "simplified" the coalescer to a uniform window, the product's
   headline latency number would regress by exactly the window size on every answer, for
   zero correctness gain — a classic case of a "simplification" that quietly breaks the one
   number the whole feature is built around.

9. **429/529 and retry.** Argument for retrying with backoff: rate limits and overload are
   often transient at the scale of seconds, and a user who gets a hard error on a rate-limit
   blip has a worse experience than one who waits an extra second for a retried success —
   especially since the app promises low latency generally, an occasional slower-but-working
   answer beats a failed one. Argument against: this app's actual retry budget is "exactly
   once, only for pre-response connection failures" specifically *because* a 429/529 is a
   deterministic-for-now signal from a server that just told you no — an immediate retry is
   very likely to hit the same wall (rate limits especially), burning another connection
   and more of the user's perceived time on a doomed attempt, and during a live call the
   user needs to know *now* that the app can't help this turn, not be left staring at a
   spinner through two failed attempts. What ships: surface the error immediately (current
   behavior) — but the honest trade-off is that a smarter policy (bounded exponential
   backoff specifically for 429/529, capped well under the 1s-adjacent budget, maybe a
   single retry after 300-500ms for 529 only, never for 429 which usually needs longer) is a
   defensible v2, not a bug in v1.

10. **Local fallback ratio.** 90s/300s vs. 5s/10s (roughly 9x/but really ~6-30x depending on
    which pair you compare) tells you "latency budget" isn't one number for this product —
    it's a *contract with the user about what to expect*, and that contract changes entirely
    once inference moves from a data-center GPU to the user's own CPU. A 1s promise made
    generically across both modes would be false advertising in local mode nearly every
    time. **This is already handled, not a gap**: `MainView.tsx` renders a persistent
    `local-mode-banner` whenever `settings.llmProvider === 'local'` — "Free local voice · no
    API fees" / "English system audio · CPU answers may take longer" — which resets the
    user's latency expectation explicitly and visibly rather than silently letting the "~1s"
    promise fail to hold. The right follow-up question in an interview: is a static banner
    enough, or should the *specific* wait (a spinner with elapsed time, given local answers
    can legitimately take up to 90s to first token) be surfaced too, given a user watching a
    silent screen for tens of seconds might assume the app hung rather than "CPU answers may
    take longer"?

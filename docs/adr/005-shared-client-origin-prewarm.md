# ADR 005 — One shared reqwest client + origin prewarm as the latency instrument

**Status**: accepted

## Context

A cold HTTPS request spends its opening milliseconds on DNS + TCP + TLS before
a byte of the question leaves the machine — roughly 300 ms against the
Anthropic/Groq origins from here. Stop-to-first-word is the one number this
app is judged on (SPEC §3), and 300 ms of handshake inside that window is a
third of the whole budget spent on transport.

The obvious fix — open the connection early — has a silent failure mode:
reqwest's connection pool lives *inside* the `Client`. Warm through one client
and answer through another, and the warmed connection is stranded in a pool
nobody reads. The feature then looks fully implemented, passes every test, and
does nothing (`src-tauri/core/src/llm/http.rs:1-12`).

## Decision

- **One process-wide `reqwest::Client`** for all LLM traffic, enforced as a
  `&'static` singleton so no caller can be tempted to build their own
  (`http.rs:35-57`). `pool_idle_timeout` is 120 s, sized for the human-length
  gap between Record and Stop — a question being asked and thought about
  (`http.rs:16-24`). Deliberately **no client-wide timeout**: that would be a
  ceiling on whole streamed answers; per-stage deadlines live in the state
  machine (ADR 012, `http.rs:45-49`).
- **Prewarm at every moment that predicts an answer request**: on Record
  (`machine.rs:229-231`, and again at the shell, `commands.rs:133-135`), on
  Ask (`machine.rs:307`), and on Stop *before* the finalize await runs
  anywhere — so the handshake overlaps the STT flush (`machine.rs:261-267`).
- The warm itself is an **unauthenticated** fire-and-forget
  `GET <origin>/v1/models` with a 3 s cap, drained to completion so the
  connection actually returns to the pool, throttled to one per origin per 2 s
  (`src-tauri/core/src/llm/warm.rs:75-100`). Unauthenticated on purpose: a 401
  completes the TCP+TLS handshake just as well as a 200, and the API key has
  no business travelling on a request whose response nobody reads
  (`warm.rs:70-74`). A failed warm never panics, propagates, or logs loudly.

## Consequences

- By the time Stop fires the answer request, the TLS handshake happened during
  the recording (or during the finalize round-trip). The measured latency chip
  is the receipt.
- Failure costs nothing but the handshake the answer request would have paid
  anyway — the warm is a pure optimization with no correctness weight.

Costs, honestly:

- **A regression here is invisible.** Nothing fails when prewarm stops
  working; the app just gets ~300 ms slower. That is why the singleton is
  pinned by pointer-identity tests across calls *and threads*
  (`http.rs:63-86`) and why the drain-to-completion rule is written down —
  dropping a response mid-body makes reqwest close the connection instead of
  pooling it (`warm.rs:16-17`).
- Process-global state: the client and the throttle map (`warm.rs:42-45`) are
  singletons, with the test-coupling that implies (each throttle test must use
  its own origin strings).
- No client-wide timeout means every request must remember to carry its own —
  the warm does (`warm.rs:88-90`); a future caller that forgets hangs
  politely.
- The provider sees periodic pointless requests; the 2 s throttle keeps
  rapid-fire Record presses from turning that into a burst.

## If revisited

If a provider starts rejecting or rate-limiting unauthenticated `/v1/models`,
switch the warm target — any endpoint on the origin completes the handshake.
If reqwest ever exposes pool introspection, assert on it and the invisible
regression becomes visible. HTTP/2 keepalive pings could replace the GET
entirely; the throttle and the "same client" invariant would survive
unchanged.

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

- **One process-wide `reqwest::Client`** for all cloud LLM traffic, enforced
  as a `&'static` singleton so no caller can be tempted to build their own
  (`http.rs:87-92`). `pool_idle_timeout` is 120 s, sized for the human-length
  gap between Record and Stop — a question being asked and thought about
  (`http.rs:23-30`). Deliberately **no client-wide timeout**: that would be a
  ceiling on whole streamed answers; per-stage deadlines live in the state
  machine (ADR 012, `http.rs:76-81`).
- **Two transport guards keep a warmed connection worth having** (RS-6,
  `http.rs:38-60`): a `connect_timeout` of 3 s bounds only the TCP+TLS
  connect — against an origin black-holing SYNs the OS default is ~21 s on
  Windows, twice the whole first-token budget, before the retry policy even
  gets a look; and TCP keepalive probes from 20 s idle at 1 s intervals, so a
  consumer NAT or VPN gateway does not silently forget the idle socket
  during a long recording (reqwest would then hold a socket the far end has
  dropped, the answer POST would die on its first write, and the one retry
  would be spent on a cold handshake). The interval is set explicitly
  because socket2 hands an unset interval to `SIO_KEEPALIVE_VALS` as 0 on
  Windows. Neither bounds a whole request — the body streams.
- **Prewarm at the moments that predict an answer request, minus one**: on
  Record (`machine.rs:230`, and again at the shell, `commands.rs:180`), on
  Stop *before* the finalize await runs anywhere — so the handshake overlaps
  the STT flush (`machine.rs:265`) — and on the 120 s auto-stop, which
  behaves exactly like a user stop (`machine.rs:576`). **Not on Ask** (RS-1,
  `machine.rs:307-312`): the answer request fires from the task spawned a few
  lines later, microseconds away, so a warm fired first cannot finish a
  handshake before it — it only races the real request for the pool, and,
  when the 2 s throttle lets it through, opens a second connection to the
  same origin for nothing. **Not at launch** either (RS-4, refuted): the
  pooled socket is reaped after the 120 s idle timeout long before a typical
  first question, and an unauthenticated request at every launch is a
  privacy cost with no measured gain.
- The warm itself is an **unauthenticated** fire-and-forget
  `GET <origin>/v1/models` with a 3 s cap, drained to completion so the
  connection actually returns to the pool, throttled to one per origin per 2 s
  (`src-tauri/core/src/llm/warm.rs:75-100`). Unauthenticated on purpose: a 401
  completes the TCP+TLS handshake just as well as a 200, and the API key has
  no business travelling on a request whose response nobody reads
  (`warm.rs:69-74`). A failed warm never panics, propagates, or logs loudly.

## Scope

The singleton rule covers the **cloud** providers — the ones with a TLS
handshake worth hiding. The free local provider deliberately does **not**
use it (`src-tauri/core/src/llm/local.rs:26-36`): it talks to Ollama on
`http://127.0.0.1:11434` through its own `reqwest::Client` built with
`no_proxy()` (a system proxy must never see loopback traffic), redirects
disabled, and a 2 s connect timeout. Its `prewarm()` is a no-op
(`local.rs:171-173`) — there is no handshake to overlap, and loading the
model is an explicit "Start and warm" gesture in Settings rather than a
side effect of every Record. The Moonshine speech service on
`ws://127.0.0.1:8765` is a WebSocket and never touches either client. The
pointer-identity tests in `http.rs` therefore pin the invariant for exactly
the traffic it protects, and "no caller can be tempted" reads as "no cloud
caller".

## Consequences

- By the time Stop fires the answer request, the TLS handshake happened during
  the recording (or during the finalize round-trip). The measured latency chip
  is the receipt.
- Failure costs nothing but the handshake the answer request would have paid
  anyway — the warm is a pure optimization with no correctness weight.

Costs, honestly:

- **A regression here is invisible.** Nothing fails when prewarm stops
  working; the app just gets ~300 ms slower. That is why the singleton is
  pinned by pointer-identity tests across calls *and threads* and the
  connect-timeout / no-whole-request-timeout pair by a `Debug`-string test
  (`http.rs`, tests), and why the drain-to-completion rule is written down —
  dropping a response mid-body makes reqwest close the connection instead of
  pooling it (`warm.rs:14-17`).
- **Keepalive is a bet on gateway timers.** 20 s idle covers the consumer
  NAT and VPN mappings seen so far; a gateway that drops idle mappings faster
  still costs one cold handshake on the retry — the same price as before
  RS-6, never worse.
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

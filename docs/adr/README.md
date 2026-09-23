# Architecture Decision Records

Reverse-engineered ADRs for the decisions that actually shape this codebase.
Each one records the forces as they stood, the call, the honest costs, and
what would change the call. Section references (§) are to
[docs/SPEC.md](../SPEC.md); code citations are `path:line` into this repo.

| ADR | Decision | One line |
| --- | --- | --- |
| [001](001-native-rust-core-thin-webview.md) | Native Rust core + thin webview, not Electron | v2's races were untestable and its latency path uncontrollable; the rewrite put the whole pipeline in one testable crate. |
| [002](002-one-session-slot-supersession.md) | One live session slot + supersession, not a queue | `Mutex<Option<Active>>` encodes "one live pipeline"; slot ownership is the permission to emit, so losers are silent by construction. |
| [003](003-trait-injected-dependencies.md) | Trait-injected dependencies | Every §5 race is a deterministic test because STT/LLM/audio/events are traits driven by scripted fakes under a paused clock. |
| [004](004-loopback-capture-not-microphone.md) | Loopback capture of the render device, not the mic | The far end's voice only exists on the playback path; WASAPI loopback captures it from any call app, at the cost of hearing all system audio. |
| [005](005-shared-client-origin-prewarm.md) | One shared reqwest client + origin prewarm (cloud providers) | The pool lives inside the Client, so the warm and the answer must share one instance — otherwise prewarm silently does nothing. The loopback provider is scoped out on purpose. |
| [006](006-retry-once-delta-guard.md) | Retry exactly once, vetoed by the delta guard | Only pre-response connection failures retry; one delta on screen vetoes it, because a concatenated answer is worse than a truncated one. |
| [007](007-byte-stable-two-block-prompt.md) | Byte-stable two-block prompt, cache breakpoint after the profile | Style flips never invalidate the cached profile; a profile switch does, deliberately; the README says out loud when the cache is a no-op. |
| [008](008-in-repo-markdown-no-links.md) | In-repo markdown renderer, links unparsed | Model text only ever becomes DOM text nodes; with no href there is nothing to sanitize — XSS is absent, not filtered. |
| [009](009-dpapi-secrets-honest-fallback.md) | DPAPI secrets, honest `plain:` fallback, fail-closed | Decode by stored prefix, never keystore state; anything undecodable reads as unset rather than being handed to a provider. |
| [010](010-closed-error-code-set.md) | Closed error-code set as the UI contract | Fourteen codes, wire strings pinned by test; codes drive behavior, messages tell the user what to do next. |
| [011](011-session-tagged-events-stale-drop.md) | sessionId-tagged events + frontend stale-drop | Supersession is enforced twice — core gate and frontend id-drop — because an emitted event cannot be recalled. |
| [012](012-core-authoritative-timeouts.md) | Core-authoritative timeouts | The frontend never enforces a deadline the core also enforces — learned from the 120s-cap double-stop bug fixed 2026-08-13. |
| [013](013-window-placement-dock-to-camera.md) | Dock to the camera by default, remember on request | Pure physical-pixel dock math in the core, a thin shell adapter, three call sites (button, launch, sanitizer fallback); the saved size is the user's, the position is the camera's. |
| [014](014-call-profiles.md) | Call profiles: one active grounding bundle | Profiles are grounding data, not personas; one pure normaliser owns the invariant; the v3 file migrates on read; the interview prefix stays byte-identical to v3. |
| [015](015-session-outcomes-and-adoption.md) | Session outcomes, adoption reconciliation, terminal answer outcomes | The core records every session's outcome per id under the slot lock; start/ask return it, the UI replays held pre-adoption events for the adopted id only and reconciles once; only the protocol terminator completes an answer, and each history entry carries its own outcome. |
| [016](016-settings-revisions-and-effects.md) | Settings revisions, reconciled OS effects, the save lock, and the local prompt budget | A committed revision is compared and advanced under the store lock (geometry never bumps it); the UI installs only newer views and rebases a stale form on reload; after every commit the shell applies the CURRENT settings' OS state; the core computes the exact local request size for unsaved drafts; a damaged settings file is preserved before it is replaced. |

## How to read these for interview prep

Each ADR translates into one transferable engineering lesson you can tell as
a story ("we faced X, chose Y, paid Z"):

- **001** — Choose the runtime for your hardest constraint (here: testable
  races + realtime audio), not for developer familiarity.
- **002** — Encode cardinality invariants in types (`Option`, not a map) so
  the rule can't be forgotten on any code path.
- **003** — Inject dependencies at the wire level so races become
  deterministic tests instead of production incidents.
- **004** — Capture data where it actually lives; then own the platform's
  blind spots (silent default-device switches) explicitly.
- **005** — An optimization whose failure is silent needs its invariant
  enforced and tested, or it will quietly stop working.
- **006** — Retries are only safe when you can prove no side effect reached
  the user; track that proof as state, not as hope.
- **007** — Caching by byte-prefix means prompt construction is a
  determinism problem; also: document when your optimization *doesn't* pay.
- **008** — Removing a feature (links) can remove an entire attack surface;
  structural security beats filter-based security.
- **009** — Fail closed on secrets, label degraded modes honestly, and decode
  by what you stored — not by what you could store today.
- **010** — Make cross-boundary contracts closed sets pinned by tests;
  free-form strings degrade silently.
- **011** — In async systems, receivers need their own staleness check;
  sender-side suppression can't recall what's already in flight.
- **012** — One deadline, one owner: duplicated enforcement is a race, and
  the component that owns the work owns the clock.
- **013** — Keep geometry math pure and unit-tested, keep the platform
  adapter thin, and be explicit about which units (logical vs physical)
  cross each boundary — the one confusion that slipped through cost every
  hi-DPI user their window size.
- **014** — When a feature multiplies user-owned state, write one pure,
  deterministic normaliser that owns the invariant and route every entry
  point through it; migrate by reading the old shape once and never writing
  it back.
- **015** — When work starts before its handle reaches the caller, record the
  outcome where the work ends and let the caller reconcile; and never let
  "the stream closed" stand in for "the protocol said it finished".
- **016** — Version shared state where it is committed, compare and commit
  in one critical section, and drive side effects from the latest committed
  state rather than from each change; and compute a limit's preview with the
  same code that enforces it.

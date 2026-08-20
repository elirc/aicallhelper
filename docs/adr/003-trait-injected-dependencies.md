# ADR 003 — Trait-injected dependencies so the state machine tests with fakes

**Status**: accepted

## Context

Every invariant in SPEC §5 is a race: latest-start-wins, stop-during-connect,
error-during-finalize, deltas racing a timeout. Races cannot be tested against
live providers — the network decides the interleaving, so the test passes
until the one day it doesn't, which is exactly how v2's session bugs shipped.
SPEC §12 makes the constraint explicit: no network, no audio device, no live
provider in any test.

The state machine also needs to outlive provider churn: Deepgram's wire quirks
and Groq's SSE dialect must not leak into the session logic.

## Decision

The state machine depends only on traits, resolved fresh per session through
`SessionDeps` (`src-tauri/core/src/session/mod.rs:106-115`):

- `SttConnector` / `SttStream` / `SttSink` — the STT wire, at the level of
  "connect, send frames, finalize, abort"
  (`src-tauri/core/src/stt/mod.rs:15-48`).
- `LlmProvider` / `LlmSink` — "stream an answer, push deltas"
  (`src-tauri/core/src/llm/mod.rs:86-111`), including the byte-for-byte
  deltas-equal-answer contract in the trait docs.
- `AudioCapture` / `AudioSink` — device open and frame delivery
  (`src-tauri/core/src/audio/mod.rs:17-34`).
- `EventSink` — where session events go; Tauri emits to the webview, tests
  push into a `Vec` (`session/mod.rs:100-104`).

Tests drive the machine with scripted fakes under a **paused tokio clock**
(`machine.rs` test module from `machine.rs:762`; the scripted fakes
`RecordingSink`, `ConnectScript`, `LlmScript` sit at ~806–1134, ahead of the
first test), so "the connect takes 300 ms and a second
Record lands at 50 ms" is a deterministic statement, not a hope. The real
Deepgram/Anthropic/Groq clients get their own wire-level tests against
scripted local servers that control the exact bytes (docs/TESTING.md, intro).

## Consequences

- 233 core tests run in seconds, and every §5 race is a pinned regression
  test. The fakes can express things reality only produces under load:
  `dead_on_arrival` errors fired inside `connect` before the stream exists
  (`machine.rs:917-921`), deltas pushed from a detached task after the
  pipeline lost interest (`machine.rs:1044-1046`).
- The trait seam doubles as a hardening layer: `SessionSttSink` enforces
  at-most-one-error *even against a misbehaving implementation*
  (`machine.rs:428-445`), because the contract is cheap to enforce at the
  boundary and expensive to debug when violated.
- Provider churn stays out of the machine. Swapping Groq models or Deepgram
  URL parameters touches zero session code.

Costs, honestly:

- `Arc<dyn Trait>` indirection and `async_trait` boilerplate on every
  dependency. Measured against the pipeline's timescales (128 ms frames,
  network round-trips) the dynamic dispatch is noise.
- The fakes are code that must be kept honest. A fake that is friendlier than
  reality (e.g. an `LlmScript` that ignored the cancel token) would quietly
  weaken the suite — which is why the fakes model cancel-aware SSE loops
  (`machine.rs:1085-1097`).
- Two layers of tests to maintain: machine-vs-fakes and client-vs-scripted
  server. The seam between them (the trait contract) is documentation that
  must stay true.

## If revisited

Native `async fn` in traits and better tokio test tooling shrink the
boilerplate but not the shape. The design would only be abandoned if the
pipeline collapsed to a single provider call with no races worth testing —
and §5 is the evidence it will not.

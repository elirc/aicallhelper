# ADR 001 — Native Rust core + thin webview, not Electron

**Status**: accepted

## Context

v2 of this app was Electron. It worked, but every one of its recurring bugs
traced back to the same structural fact: the pipeline — audio capture, the
Deepgram socket, the LLM stream, the session state machine — lived in an
Electron main process that only ran inside Electron. The consequences:

- **The races were untestable.** The v2 session bugs (superseded sessions
  painting over new ones, double-stops, late events) could only be reproduced
  by running the whole app against live providers. Every invariant in SPEC §5
  is annotated "purchased with a real bug in v2" for exactly this reason.
- **The latency path crossed too many boundaries.** System-audio capture on
  Windows means WASAPI, which means a native module or an IPC hop from a
  helper — either way the ~1 s stop-to-first-word promise (SPEC §3) was paying
  a tax in places that couldn't be measured or controlled.
- **Realtime audio and a garbage-collected runtime are a bad marriage.** The
  WASAPI data callback runs on a realtime thread where blocking glitches every
  audio app on the system (`src-tauri/core/src/audio/capture.rs:97-105`).

## Decision

Rewrite as Tauri 2 + a native Rust core, with a deliberate three-layer split:

- **`app-core`** (`src-tauri/core/`) owns the ENTIRE pipeline — capture,
  resampling, the Deepgram WS, LLM streaming, the state machine, settings,
  secrets, metrics (SPEC §2). It knows nothing about Tauri.
- **The shell** (`src-tauri/src/`) owns exactly the glue: IPC envelopes, event
  emission, window lifecycle, hotkey, crash logging
  (`src-tauri/src/lib.rs:1-5`).
- **The frontend** is a thin React view over a five-event contract (ADR 011).

The core is a separate crate on purpose: `cargo test -p app-core` exercises
every §5 invariant with fakes — no webview, no network, no audio device
(README "Architecture").

## Consequences

- 233 core tests run in seconds with a paused tokio clock; every race that was
  a production incident in v2 is now a deterministic regression test (ADR 003).
- The latency path is one process, one language, measurable end to end. The
  prewarm design (ADR 005) only became possible because the core owns the HTTP
  client.
- WASAPI is used directly through `cpal` with proper realtime-thread
  discipline: the data callback does nothing but resample, frame, and
  `try_send` (`capture.rs:97-105`).
- Content protection is one call at window build
  (`src-tauri/src/lib.rs:93-97`), and launch **fails** if the OS refuses —
  no Electron-style "hope the flag worked".

Costs, honestly:

- **Windows-only by construction.** WASAPI loopback, DPAPI, WebView2 — the
  portability Electron gave away for free is gone. Acceptable: the product is
  a personal Windows tool.
- **WebView2's gaps are ours to fill.** Tauri 2 does not surface the renderer
  ProcessFailed event, so crash recovery reaches through raw COM
  (`src-tauri/src/window.rs:293-319`). In Electron that is a one-liner.
- **Two-language boundary.** Every event and error crosses Rust→TS, which
  forced the explicit closed contracts of ADR 010/011 — good discipline, but
  it is work Electron's single-runtime model never demanded.
- Rust iteration is slower than JS iteration. The bet is that this codebase is
  mostly *finished* logic guarded by tests, not a UI playground — and the UI,
  where iteration actually happens, is still TypeScript.

## If revisited

The call flips back toward Electron (or a maintained fork) only if the app
needs cross-platform support or heavy npm-ecosystem UI features faster than
the thin event contract can grow. Any such move would have to re-prove the two
things this rewrite bought with measurements: sub-second stop-to-first-word,
and every §5 race under `cargo test`.

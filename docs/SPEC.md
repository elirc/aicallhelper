# AI Call Assistant v3 — product specification

This is the specification the app was built against. Code comments cite its
section numbers (§5.5, §6.4, …). Where this document pins an exact string,
timeout, or ordering rule, treat it as load-bearing product behavior, not a
suggestion.

v3.1 added **call profiles** (§7, §8), the **answer-first, dockable window**
(§9) and the keyless **free local mode** (§6.5). Every v3 string, timeout and
ordering rule below survived that change byte for byte; the new behavior is
new sections and new elements, never edits to pinned ones.

---

## 1. What the product is

A push-to-record **interview copilot** for Windows — and, through call
profiles, a copilot for sales, support and meeting calls. During a live call
(Zoom/Teams/Meet/phone-through-speakers), the user presses **Record** while
the other person is asking a question. The app:

1. captures **system audio** (what the other person is saying — loopback, not
   the microphone),
2. streams it to a speech-to-text service so a **live transcript renders
   while they are still speaking**,
3. when the user presses **Stop & Answer**, finalizes the transcript and
   streams an **AI-suggested answer** (grounded in the ACTIVE call profile:
   resume, job description or call context, focus, extra instructions — §7)
   into the answer panel,
4. reports the **measured stop-to-first-word latency** in the UI.

The single product promise is **stop-to-first-word latency of roughly one
second**. Every architectural decision below exists to protect that number or
to make being fast safe. Because the user is on a call, the app requests
**Windows capture exclusion** for its window and verifies at launch that
Windows applied it, refusing to start otherwise; whether a given conferencing
app honours the exclusion depends on its capture method and is only known from
recorded tests.

Secondary flows: a typed **Ask** box (same answer pipeline, no audio), a
**Regenerate** button (re-ask the viewed question), a 6-entry **history**, an
**answer style** toggle (brief / balanced / detailed), a **global hotkey**
that toggles record/stop from any app, up to eight **call profiles** switched
from chips on the main view (§8), a **Dock to camera** button and launch
placement that put the answer directly under the webcam (§9), and a
**focus mode** that hides everything but the answer (§9).

This is a personal productivity tool: single user, their own API keys, no
telemetry, no server component of ours. Providers: **Deepgram** (STT) and, for
answers, **Anthropic Claude** (default) or **Groq** (user-selectable) — or the
keyless **Free local voice** mode, which answers with Ollama (Qwen3.5 2B) and
transcribes with Moonshine, both over loopback on this machine (§6.5). Local
mode needs no key at all, Deepgram included; its answers run on the CPU, so
the ~1 s promise is a cloud-mode promise (§3).

---

## 2. Tech stack

- **Shell**: Tauri 2.x (Windows 10/11 target only).
- **Core**: Rust (stable), `tokio` async runtime. The core owns the ENTIRE
  pipeline: audio capture, downsampling, the Deepgram WebSocket, the LLM HTTP
  streaming, the session state machine, settings + secrets, and metrics. The
  frontend is a thin view.
- **Audio capture**: WASAPI **loopback** capture of the default render device
  (via `cpal`'s WASAPI loopback support).
- **WebSocket**: `tokio-tungstenite` (+ rustls).
- **HTTP**: `reqwest` (rustls, streaming). ONE shared `Client` for all cloud
  LLM traffic — its connection pool is what makes pre-warming work (§6.4).
  Configure a generous `pool_idle_timeout` (≥ 90 s; built as 120 s), a
  3 s connect timeout and TCP keepalive (§6.4), and NO whole-request
  timeout (the body streams). The local provider has its own loopback-only
  client (§6.5) — nothing about the cloud pool applies to it.
- **Secrets**: Windows DPAPI (`CryptProtectData`/`CryptUnprotectData` via the
  `windows` crate), stored base64 in the settings file (§8).
- **Global shortcut**: `tauri-plugin-global-shortcut`.
- **Single instance**: `tauri-plugin-single-instance` (second launch focuses
  the running window).
- **Content protection**: `window.set_content_protected(true)`, then a
  read-back with `GetWindowDisplayAffinity` that must return
  `WDA_EXCLUDEFROMCAPTURE` (0x11; Windows 10 version 2004 or later) —
  otherwise setup fails with a message naming that requirement (crash.log).
- **Frontend**: React (latest stable) + TypeScript `strict` (plus
  `noUncheckedIndexedAccess`) + Vite. No component libraries, no CSS
  frameworks — one hand-written dark stylesheet. No markdown or sanitizer
  dependencies: the markdown renderer is written in-repo to the security spec
  in §10.
- **Tests**: `cargo test` for the core, Vitest + @testing-library/react for
  the frontend. **No network and no live audio device in any test** — inject
  traits/fakes at the wire level (§12).

Repo layout (as built — the pipeline lives in the separate `app-core` crate so
`cargo test -p app-core` runs without the Tauri shell):

```
src-tauri/
  core/               crate `app-core` — the entire pipeline, no Tauri
    src/audio/        WASAPI loopback capture → 16 kHz mono i16 frames + RMS
    src/stt/          deepgram.rs (WS client), frame.rs (pure parse fns),
                      local.rs (Moonshine loopback WS, §6.5)
    src/llm/          anthropic.rs, groq.rs, local.rs (Ollama NDJSON, §6.5),
                      http.rs (the shared client), retry.rs, warm.rs,
                      prompt.rs (profiles → cached prefix, §7), sse.rs
    src/session/      the session state machine (§5) — trait-injected deps
    src/store/        settings + profiles (§8), DPAPI secrets, bounds + dock math
  src/                Tauri shell: commands, events, window (geometry, dock,
                      navigation policy), hotkey, local_voice (service
                      readiness/start), logging (crash log), state
src/                  React frontend: views (Main, Settings), components,
                      state (reducer + session hook), markdown renderer
local-voice/          Python Moonshine speech service (127.0.0.1:8765)
scripts/              setup / start / test-free-voice.ps1
docs/TESTING.md       every test documented: what it verifies and why it exists
```

The README carries the same tree with one line per file; this one names only
what the sections below cite.

---

## 3. The latency architecture

```
Record pressed    STT WebSocket connect and audio capture start IN PARALLEL.
                  Frames captured before the socket is open are buffered in
                  order (cap ~15 s, drop oldest) and flushed the instant it
                  opens. The LLM origin is pre-warmed (§6.4).
While recording   loopback audio → downsample to 16 kHz mono i16 → ~128 ms
                  frames (2048 samples) → Deepgram. Interim transcripts render
                  live; `is_final` marks committed text. RMS per frame drives
                  a level meter. Keepalive every 8 s (§6.1).
Stop pressed      THE LATENCY CLOCK STARTS HERE. Send CloseStream; Deepgram
                  flushes its tail (5 s cap). The LLM origin is pre-warmed
                  again, so the TLS handshake overlaps the finalize.
                  The LLM request fires the instant the transcript is final.
Answer            first token streams into the panel; on completion the
                  measured stop-to-first-word lands in the panel header.
```

**Timeouts** (all enforced in the core, all surfacing structured errors):
STT finalize **5 s** · LLM first token **10 s** · LLM total **60 s** (cloud
providers) — **90 s / 300 s when `llmProvider` is `local`** (CPU inference;
ceilings, not speed promises — the ~1 s promise is a cloud-mode promise) ·
recording hard cap **120 s** (auto-stop, then answer normally). The answer
deadlines are a property of the provider (`LlmProvider::answer_limits()`,
default `AnswerLimits::CLOUD`, overridden by the local provider), so the
state machine never matches on the provider kind to learn how long to wait.
Pre-warm (§6.4) is a **no-op for the local provider**: loading the model is
explicit in Settings, and a loopback handshake is not worth warming.

**Metrics** — measured in the core, from the moment stop was requested:

- `sttFinalizeMs` — stop → final transcript in hand. Exactly `0` for typed
  (Ask) questions: there was no STT stage, and billing one would be a lie.
- `firstTokenMs` — stop → first answer token. If a provider returns a full
  answer without ever streaming a delta, report `firstTokenMs = totalMs` —
  never 0, because 0 renders as "instant" and lies about the one number this
  app is judged on.
- `totalMs` — stop → answer complete.

**Prompt caching** (Anthropic): the system prompt is TWO blocks with the cache
breakpoint after the profile block (call framing, resume/JD, grounding note,
focus, extra instructions); the style policy sits AFTER it (§7). Caching is a
byte-prefix match, so prompts must be byte-stable across calls — no
timestamps, no unordered joins — and a style flip must never invalidate the
cached profile. The prefix is built from the ACTIVE profile only, so
switching profiles is a deliberate, between-calls cache write — never a
per-question one (§8). Honesty: Haiku's minimum cacheable prefix is 4096
tokens, so a typical 1–2 K-token profile makes the marker a silent no-op; it
starts paying at roughly 16 K+ characters of profile (writes 1.25×, reads
0.1×, 5-minute TTL). `usage.cache_read_input_tokens` in the response tells
the truth about whether it engaged.

---

## 4. Frontend ↔ core interface

Commands (Tauri `invoke`), all returning a `Result`-shaped envelope
`{ ok: true, value } | { ok: false, error: { code, message } }` rather than
throwing across the boundary (validation errors included) — with one
exception: a malformed `set_settings` patch whose enum field (`llmProvider`,
`answerStyle`, `launchPlacement`, `streamFollow`) carries a value outside its
closed set fails Tauri argument deserialization and REJECTS the invoke. The
bridge folds any rejection into `{ code: "internal" }`, so the UI still sees
one shape; a second client that bypasses `bridge.call()` would see a rejected
promise. `parse_or_default` applies to the settings FILE only (§8), never to
the wire — the UI is compiled against the same enums. The wire shapes
(`SettingsView`, `CallProfile`, `SettingsPatch`, `HotkeyStatus`,
`LocalVoiceStatus`, `LocalPromptBudget`, the events) are declared once in
`src/types.ts`; the Rust side serializes to exactly those shapes
(`rename_all = "camelCase"`).

- `get_settings() -> SettingsView` — the view carries `revision` (the
  committed editable-settings revision, ADR 016) and `storageWarning`
  (non-null only when the settings file could not be used at startup, §8).
- `set_settings(patch) -> SettingsView` (async — the fsync'd write and the
  DPAPI re-encryption run on the blocking pool, never on the event-loop
  thread) — partial patch; omitted fields are untouched. `profiles` is a
  WHOLE-ARRAY replace (the Settings form owns the draft and sends all of it);
  each entry is lenient — a missing field defaults to `""` and an unknown
  `callType` reads as `interview`, so a partial object never fails the whole
  patch. `activeProfileId` may be sent ALONE (the main-view profile switch);
  an id that names no profile leaves the active one unchanged. The four enum
  fields are closed enums on the wire (see the rejection rule above). Returns
  the fresh view after the atomic write, which the UI re-renders from (main is
  the source of truth: ids may have been repaired, names trimmed, the hotkey
  normalized — §8). **Revisions (ADR 016)**: a patch may carry
  `expectedRevision`; the store compares it with the committed revision under
  the same lock that commits the patch and fails a mismatch with
  `settings_conflict` ("Settings changed while this form was open. Reload
  it."), changing nothing. The full Settings form always sends it; the
  single-field chip patches never do (they merge). Every committed patch adds
  1 to the revision; a failed write and a window-geometry save never do.
  **Effects (ADR 016)**: after every committed patch the shell reconciles the
  OS with the CURRENT committed settings, not with this patch's before/after:
  runs are serialized, each reads the committed state inside its lock, the
  hotkey is re-registered (from the main thread) when the desired
  accelerator differs from the last one the OS accepted (a refused
  registration, at launch or on a save, counts as not applied, so every
  later save retries it and a combination another app has since released
  starts working), always-on-top is set when it
  differs, and the window docks at its current size on the transition of
  `launchPlacement` into `camera` (§9). Whatever order saves commit and their
  effect steps run in, the OS ends on the newest committed state.
- `start_session() -> { sessionId, outcome }` — starts capture + STT;
  supersedes any active session. Refuses BEFORE anything opens when a key the
  selected provider needs is missing (`no_stt_key` / `no_llm_key`); the local
  provider needs none (§6.5). With the local provider it also refuses before
  the device opens when the active profile leaves no room for any question
  (the local budget, §6.5), with the gate's own `llm_http` message.
  `outcome` is the session's `SessionOutcome` as
  of the response, read after the device open (ADR 015): a session that
  already ended there — an immediate connect failure, say — reports it here
  (`failed`/`completed`/`cancelled`) instead of a bare id the UI would wait on.
  An already-ended session is still an ok envelope; a capture-device failure
  remains an error envelope (the machine session is cancelled silently).
- `stop_session(sessionId) -> ok/err` — MUST return "not taken" (an error
  Result) when the session is unknown/already ended/already stopping: every
  other outcome arrives as an event, so a silently ignored stop leaves the UI
  in "Finalizing…" forever. This return is the only way it learns.
- `ask(text) -> { sessionId, outcome }` — typed question; trimmed, 1..8000
  chars. With the local provider, a question whose actual size would exceed
  the local cap is refused before a session is claimed (the gate's own
  `llm_http` message, §6.5). Same envelope as `start_session`: the answer
  task starts before the response, so an instant answer is reported in
  `outcome`.
- `session_outcome(sessionId) -> SessionOutcome` — read-only, idempotent
  lookup the UI makes once when it adopts an id whose start envelope said
  `active` (ADR 015). `SessionOutcome` is `{ status: "active" }` ·
  `{ status: "completed", transcript, answer, metrics, stopReason }` ·
  `{ status: "failed", error, transcript, partial }` (`partial` = the answer
  text shown before the failure) · `{ status: "cancelled" }` (cancelled or
  superseded) · `{ status: "unknown" }` (never issued, or retired). The core
  records outcomes per id under the same lock that releases the session slot
  and keeps the last 16 (`OUTCOME_RETENTION`); an outcome is retired only when
  16 newer sessions have been claimed, which cannot happen to the one attempt
  the UI is adopting.
- `cancel_session(sessionId)` — fire-and-forget; invalid ids do nothing
  (never an error — the caller doesn't await it meaningfully).
- `hotkey_status() -> { accelerator, registered }` — what actually took
  effect at startup or after the last save. `registered: false` with a
  non-empty accelerator is the honest "another app owns this combo, or it did
  not parse" signal §9 renders; an empty accelerator with `registered: false`
  is the user having DISABLED the shortcut (§8), not a failure.
- `dock_to_camera() -> null` — sync (window operations belong on the
  event-loop thread). Moves the main window to the top-centre of the display
  it is on, at the reading preset size (§9). Error `internal` "Could not find
  the display this window is on." when neither the current nor the primary
  monitor can be resolved; the button is live before settings load.
- `local_prompt_budget(profile, answerStyle, question) -> LocalPromptBudget`
  (async, pure computation, R4) — the exact byte size of the local request
  the core would build for an UNSAVED profile draft (a lenient
  `CallProfile`), style and question (`""` = what is left for a question):
  `{ usedBytes, limitBytes (7000), remainingBytes (negative when over),
  fixedBytes, profileBytes (edge-trimmed fields), questionBytes, reserveBytes
  (200), status: "ok" | "tight" | "over" }`. `over` = the request exceeds the
  cap (with an empty question: not even a one-byte question fits); `tight` =
  fewer than 200 bytes left, a usability warning only. Computed by the same
  function the local gate enforces (§6.5).
- `local_voice_status() -> LocalVoiceStatus` — live loopback probes of the
  two local services (§6.5), never simulated:
  `{ ollamaRunning, modelAvailable, speechReady }` (booleans).
- `prepare_local_voice() -> LocalVoiceStatus` — starts whichever local
  services are not up, waits for them (≤ 60 s), warms the model, and returns
  the fresh status; never downloads anything (setup owns downloads). Errors
  are `internal` and name the log to read (§6.5). One prepare runs at a time.

There is NO "open URL" command: `open_external` was removed as dead code. The
app never navigates; an https link is bounced to the default browser by the
window's navigation policy and everything else is dropped (§9).

Events (Tauri events). The five session events are every one tagged
`{ sessionId }` — the frontend drops any event whose id is not the session it
is currently tracking; `hotkey:toggle` is the one untagged shell event (it has
no session). Adoption contract (ADR 015): the UI registers its listeners and
awaits their readiness BEFORE any `start_session`/`ask` (a registration
failure, or no registration within 3 s, is a visible start error, never a
silent hang, and the next attempt re-subscribes); while the call is in
flight it holds incoming session events (bounded, merged per id), and on
adoption replays only those tagged with the adopted id, then reconciles the
envelope's `outcome` (or one `session_outcome` lookup). Reconciliation is
guarded by attempt and id and is idempotent with the live terminal events:

- `stt:partial { sessionId, text, isFinal }` — full transcript so far (not a
  delta); `isFinal` true when Deepgram committed the latest segment.
- `llm:delta { sessionId, delta }` — answer text delta.
- `llm:done { sessionId, transcript, answer, metrics, stopReason }` —
  `stopReason` is `"complete"` or `"token_limit"` (a capped answer, shown as
  "cut short"); it is outcome metadata, kept apart from the timing in
  `metrics`. Protocol completion is the provider's terminator only (§6.2,
  §6.3, §6.5).
- `session:error { sessionId, error: { code, message } }`
- `audio:level { sessionId, rms }` — for the meter (~8/s; may be coalesced).
- `hotkey:toggle` — global shortcut fired.

Error codes (closed set — the UI keys behavior off these):
`no_stt_key · no_llm_key · stt_connect · stt_error · stt_timeout · no_speech ·
llm_auth · llm_http · llm_rate_limit · llm_first_token_timeout · llm_timeout ·
aborted · internal · settings_conflict`. Messages are user-facing and actionable ("… Open
Settings (gear icon) and add it."), never raw exception text when avoidable.
`aborted` is special: the UI never shows it — it means the user superseded or
cancelled, which must be silent.

Answer outcomes (R2, ADR 015). A provider's answer is complete only when its
protocol terminator arrives (Anthropic `message_stop`, Groq `data: [DONE]`,
Ollama `done: true`); nothing after the terminator is read. Normal terminator
with usable text → `llm:done` (`stopReason: "complete"`); token-limit stop →
`llm:done` with `"token_limit"`; an error frame, a malformed or oversized
frame, or a clean end of stream before the terminator → `session:error`
(`llm_http`) with the streamed text kept on screen; a terminator without
usable text → `session:error`, never a blank answer. Every history entry
carries its own status (`completed · incomplete · cancelled · limited`) and
reason, so an interrupted answer stays recognisable after the next question
clears the error box.

---

## 5. The session state machine (the crown jewels)

One live question/answer pipeline at a time. These invariants were each
purchased with a real bug in v2; implement and test every one:

1. **Supersession**: `start` or `ask` aborts any active session (its network
   work cancelled, its stream torn down). Events from a superseded session
   are dropped by id — including its `done`. Aborting the old session CAUSES
   its socket to die; that death must not be reported as an error.
2. **Latest-start-wins**: STT connect is a network round-trip; the user can
   press Record again while one is connecting. Claim "newest" BEFORE the
   await; a start that resolves and discovers it lost must tear its stream
   down and report `aborted` (silently) — it must never install itself over
   the winner or misroute audio.
3. **Stop contract**: `stop` returns took/not-took (see §4). A second stop
   while the first runs, a stop after completion, a stop after an error tore
   the session down, and a stop during connect all return "not taken" without
   emitting anything.
4. **Audio routing**: frames are accepted only for the live, not-yet-stopped
   session. Frames arriving after stop was requested are dropped (they would
   race the CloseStream flush). Frames for stale ids are dropped.
5. **STT error policy**: a mid-recording or mid-finalize stream death
   surfaces as ONE `stt_error` and tears the session down — a silently
   truncated transcript answers the wrong question, and "no speech detected"
   for a socket death sends the user debugging the wrong thing. BUT once the
   transcript is finalized, the STT stream's job is done: a late socket close
   must NOT kill an answer that is already streaming.
6. **One error per stream, and never after abort.** An error that occurs
   before the session has registered its error handler is queued and
   delivered on registration, not dropped.
7. **Empty transcript**: a finalize that yields only whitespace surfaces
   `no_speech` ("No speech detected in the recording. Make sure call audio is
   playing.") — never an LLM call on an empty prompt.
8. **Ask path**: validates non-empty BEFORE superseding (garbage input must
   not kill a live session), returns the id as soon as the answer task is
   spawned — events may fire before the UI has the id, which is what the
   adoption contract (§4, ADR 015) covers — then emits the trimmed question
   as one `stt:partial` with `isFinal: true`
   (the UI renders both paths through one event shape), then deltas, then
   done with `sttFinalizeMs: 0`.
9. **Timeout interplay**: the first-token timer is disarmed by the first
   delta; the total timer runs to completion. A timeout aborts the in-flight
   work and reports its specific code. Deltas that race in after a timeout
   fired are suppressed — nothing paints after the error.
10. **Cancel** is silent: no done, no error, work aborted, slot released.
11. **Slot release**: whatever the outcome (done, error, abort), the active
    slot is released exactly once, so the next session never thinks it is
    superseding a ghost.
12. **Recorded outcome** (ADR 015): the slot release and the session's
    terminal outcome are written in one critical section and settle once —
    a later path (a second error, a late device error, the driver guard) never
    rewrites how a session ended. `failed` carries exactly the text the UI was
    shown.
13. **Supervised driver**: a drop guard on every driver task settles an
    unsettled session with `internal` "The answer stopped unexpectedly. Try
    again." — a panicking provider or connector, or one returning `aborted`
    nobody asked for — and aborts the STT stream it still owned. Every STT
    abort path tears the socket down exactly once. Cancel and supersession
    stay silent. During a panic unwind the settle is synchronous and the
    error is emitted from a fresh task, so a panicking event sink can never
    turn one failed answer into a process abort. A capture-device error
    checks the phase and releases the slot in one critical section, so a
    Stop accepted at the same instant is never overridden.

The state machine is a plain module with injected dependencies (an
`SttStream` trait, an `LlmProvider` trait, an event sink) — `cargo test`
exercises every rule above with fakes, no network.

---

## 6. Provider wire protocols

### 6.1 Deepgram (STT)

Cloud providers only: when `llmProvider` is `local` the loopback speech
service replaces Deepgram entirely and no Deepgram key is needed (§6.5).

- URL: `wss://api.deepgram.com/v1/listen?model=nova-3&encoding=linear16&sample_rate=16000&channels=1&interim_results=true&smart_format=true`
- Auth: WebSocket subprotocol `["token", <api key>]`.
- Send: binary frames of raw little-endian i16 PCM, 16 kHz mono, ~128 ms
  (2048 samples) each.
- Keepalive: send `{"type":"KeepAlive"}` every **8 s** while open — Deepgram
  kills idle sockets ~10 s after the last audio (NET-0001), and silence
  during a call is normal. Stop the keepalive the moment a close is
  requested: a KeepAlive after CloseStream can error on the CLOSING socket
  and fabricate a "lost connection" during a stop that is succeeding.
- Receive (JSON text frames):
  - `{"type":"Results", "is_final": bool, "channel":{"alternatives":[{"transcript": "..."}]}}`
    — maintain the full transcript as: committed prefix (append each
    non-empty final's transcript) + latest interim. Accumulate the committed
    prefix incrementally (O(1) per message, not a re-join of the whole
    recording). `is_final` must be literally `true` — truthy imposters are
    interim. Non-string transcripts, missing/null channel, empty
    alternatives: ignore the frame. Malformed or pathologically nested JSON:
    ignore, never crash.
  - `{"type":"Error", ...}` — two shapes exist: v1 listen
    `{description, message, variant}` and newer `{code, description}`. Quote
    whatever detail is present in the surfaced `stt_error` message.
  - `Metadata`, `UtteranceEnd`, anything else: ignore.
- Finalize (on stop): send `{"type":"CloseStream"}` — the server flushes any
  held-back tail (including smart_format entity hold-back) and closes. Wait
  for the close (or the 5 s cap), then return the full transcript.
  Deliberately do NOT tune `endpointing`/`no_delay`: this client never waits
  on the endpointer, so those knobs only cost smart_format quality.
- Connect failures: Deepgram rejects bad keys/requests by CLOSING the socket,
  often without an error frame — close code 1008 carries a `DATA-xxxx`
  reason, 1011 a `NET-xxxx` server fault. A close before open is a connect
  failure (surface the code+reason detail); connect also has its own timeout
  (5 s). Message: "…Check the API key and your network."
- Finalize must be idempotent (a second call joins the first). A stream that
  has already died — or can no longer open — finalizes immediately with
  whatever it has rather than burning the timeout. A dial still in flight is
  neither: the pre-open buffer holds the user's entire captured question, so
  a stop waits the connect out (within the finalize cap) and flushes it
  rather than discarding the recording into a false `no_speech`.

### 6.2 Anthropic (default answer provider)

- `POST https://api.anthropic.com/v1/messages` with headers `x-api-key`,
  `anthropic-version: 2023-06-01`, `content-type: application/json`.
- Body: `model: "claude-haiku-4-5"`, `max_tokens: 1024` (spoken answers are
  short; an uncapped completion is pure tail latency), `stream: true`,
  `system` as TWO blocks: `[{type:"text", text: <cachedPrefix>,
  cache_control:{type:"ephemeral"}}, {type:"text", text: <styleSuffix>}]`,
  `messages: [{role:"user", content: <user wrapper §7>}]`.
- SSE stream: accumulate `content_block_delta` events with
  `delta.type == "text_delta"` → `delta.text` is the answer delta. The final
  answer is the concatenation of ALL text deltas across ALL content blocks,
  joined with NOTHING between blocks — it must equal what streamed into the
  panel byte for byte.
- Completion (R2, ADR 015): ONLY `message_stop` finishes the answer; nothing
  after it is read. `message_delta.delta.stop_reason` is metadata
  (`max_tokens` → `stopReason: "token_limit"`, anything else → `"complete"`)
  and does not complete the answer on its own. A clean end of stream before
  `message_stop` → `llm_http` "Anthropic stopped sending before the answer was
  finished, so it is incomplete. Try again." (streamed text kept); a
  `message_stop` with no usable text → `llm_http` "…finished without any
  answer text"; an `error` event → `llm_http` quoting its detail; a frame that
  is not JSON, or a `text_delta` without text → `llm_http` "…malformed
  streaming frame…"; unknown event types and non-text deltas are harmless.
  No SSE line or event may exceed 1 MiB (enforced while reading → `llm_http`
  "…oversized streaming frame…"); error bodies are read at most 16 KiB.
- Error mapping (HTTP status → code + actionable message): 401 → `llm_auth`
  ("Anthropic rejected the API key (401). Check it in Settings."); 403 →
  `llm_auth` (key not allowed to use the model); 429 → `llm_rate_limit`
  (wait / check credit balance); 529 → `llm_http` ("Anthropic is overloaded
  (529). Try again in a moment."); other statuses → `llm_http` with status +
  body snippet; connection-level failure → `llm_http` ("Could not reach
  Anthropic. Check your internet connection."); caller abort → `aborted`,
  checked FIRST (an abort must never surface as a scary HTTP error).

### 6.3 Groq (user-selectable "fastest" preset)

- `POST https://api.groq.com/openai/v1/chat/completions`, `Authorization:
  Bearer <key>`.
- Body: `model: "openai/gpt-oss-120b"` (pinned in ONE constant),
  `stream: true`, `temperature: 0.7`, `max_completion_tokens: 1024`,
  `reasoning_effort: "low"`, `include_reasoning: false` — gpt-oss is a
  reasoning model and reasoning is the enemy of time-to-first-word; these two
  are the supported knobs for this family (`reasoning_format` is a
  Qwen-family knob — do not send it). System prompt as ONE string (§7).
- OpenAI-style SSE: `data:` lines; delta is
  `choices[0].delta.content`; `data: [DONE]` is the protocol TERMINATOR
  (R2, ADR 015): it alone completes the answer, and nothing after it — even
  in the same chunk — becomes answer text. `finish_reason` is metadata
  (`length` → `stopReason: "token_limit"`). A structured `{"error": …}` frame
  → `llm_http` quoting its message; a non-JSON frame → `llm_http`
  (malformed); clean EOF before `[DONE]` → `llm_http` (incomplete, text
  kept); `[DONE]` with no usable text → `llm_http` (no blank success); usage
  and unknown frames are harmless. Same 1 MiB frame and 16 KiB error-body
  caps as §6.2. The SSE parser (framing only — it never stops on `[DONE]`)
  must handle chunks split at ANY byte boundary (mid-line, mid-JSON, between
  `\r` and `\n`), CRLF and LF alike, comment/keep-alive lines, and must flush
  a final un-terminated `data:` line at end of stream (a truncated stream
  otherwise silently loses the answer's last words). Decode UTF-8 with a
  streaming decoder so multi-byte characters split across chunks survive.
- Error mapping: 401/403 → `llm_auth` (report the ACTUAL status — a 403
  labelled 401 sends the user debugging the wrong thing); 429 →
  `llm_rate_limit`; 404 → `llm_http` with "the model may have been retired —
  update the pinned model constant" (Groq retires models on short notice;
  that is the likely cause); ≥500 → `llm_http` "Groq is unavailable"; 200
  with an empty body → `llm_http` (not a crash); mid-stream drop →
  `llm_http` "connection dropped while the answer was streaming".

### 6.4 Retry policy and pre-warm (cloud providers)

- **Retry exactly once**, and ONLY when the initial request failed at the
  connection level BEFORE any delta reached the UI. Never retry an HTTP error
  status (the server heard us and said no — an instant retry burns the
  first-token budget), never after a delta (the UI appends deltas; a second
  attempt would concatenate two answers), never after an abort. The retried
  request must be byte-identical (build the body once). Extract this policy
  into one shared helper both providers use, with the "is this retryable"
  predicate supplied per-provider.
- **Pre-warm**: on Record press, on Stop press (before awaiting the stop),
  and on the 120 s auto-stop (exactly like a user stop) — NOT on Ask, and NOT
  at launch. Fire an unauthenticated fire-and-forget `GET <origin>/v1/models`
  through the SHARED reqwest client with a 3 s timeout, reading the body to
  completion so the connection returns to the pool. Throttle to one warm per
  origin per 2 s. A failed warm costs nothing and must never throw or log
  loudly. Why not Ask: the answer request fires microseconds after the ask
  resolves, so a warm cannot finish a handshake first — it can only race the
  real request for the pooled connection (a second socket to the same origin
  for nothing). Why not launch: the pooled socket is reaped by the idle
  timeout long before a typical first question, and an unauthenticated
  request at every launch is a privacy cost with no measured gain. The other
  three sites have a human-length gap to overlap. The local provider's
  `prewarm()` is a no-op (§6.5).
- **Transport guards** on the shared client, so a warmed connection stays
  worth having: `connect_timeout` **3 s** — bounds ONLY the TCP + TLS
  connect (a black-holed SYN otherwise sits in the ~21 s Windows timeout,
  twice the first-token budget, before the retry gets a look); TCP keepalive
  after **20 s** idle with a **1 s** probe interval, set explicitly (socket2
  hands an unset interval to Windows as 0) so a NAT or VPN gateway does not
  forget the idle socket during a long recording; `pool_idle_timeout`
  **120 s** (the Record→Stop gap is a human-length pause); and NO
  whole-request timeout — the body streams, and the per-stage deadlines live
  in the state machine (§3).

### 6.5 Free local voice (Ollama + Moonshine; `llmProvider = "local"`)

Everything stays on the machine: two loopback origins, no key of any kind.
`LlmProviderKind::needs_cloud_keys()` and `uses_deepgram()` are both false
for `local`, and the shell keys its key gates and its STT choice off those
two capabilities — never a `== Local` check — so a recording starts with no
keys stored at all, the first-run nudge is never shown (§9), and the
Deepgram connector is replaced by the loopback speech client below.
Selecting local mode does not erase saved cloud keys. Setup, hardware
limits and the manual test script live in `docs/FREE_VOICE_MODE.md`; this
section pins the wire behavior.

**Answers (Ollama)**:

- `POST http://127.0.0.1:11434/api/chat`, model `qwen3.5:2b` (ONE pinned
  constant), body `stream: true`, `think: false`, `keep_alive: "10m"`,
  `options: { num_ctx: 8192, num_predict: 512, num_thread: 4,
  temperature: 0.3 }`, `messages: [{ role: "system", content:
  <cachedPrefix + "\n\n" + styleSuffix> }, { role: "user", content:
  <user wrapper §7> }]`.
- **Input cap**: the system string plus the user message must fit
  **7 000 UTF-8 bytes** (bytes, so non-English text is budgeted honestly).
  Over the cap the request is REFUSED before it is sent — never silently
  truncated — with `llm_http` and exactly: "Free local mode supports about
  7 KB of combined instructions, profile (resume, job description, focus,
  extra instructions) and question. Shorten the active profile in Settings
  or use a cloud model." The count is `request_input_bytes` in `prompt.rs`,
  and the same function backs the budget (R4): `local_prompt_budget` builds
  the request exactly as the answer path does (trimming, style suffix,
  question wrapper). The fixed overhead for an interview profile with only a
  resume and Balanced style is 771 bytes (pinned by test), so a 6 500-byte
  resume plus "Hi?" is 7 274 bytes. The budget is checked three more times
  before anything is sent. The core refuses with this same message before
  recording (the active profile with no question) and before a typed ask
  (the actual question). The UI checks a typed question first, so it
  supersedes nothing and stays in the box; its refusal (`llm_http`) is
  formatted from the core's figures: "This question is N bytes too long for
  free local mode, which allows 7,000 bytes for the instructions, the active
  profile and the question together. Shorten the question or the active
  profile in Settings, or use a cloud model." The gate here stays
  authoritative for the real transcript after Stop. Settings previews the
  unsaved draft (§9).
- **Stream**: NDJSON — one JSON object per line; HTTP chunks may split a line
  or a UTF-8 sequence anywhere. `message.content` is the answer delta
  (empty strings skipped); `message.thinking` is NEVER emitted; `done: true`
  ends the answer and nothing after it is read (R2); its `done_reason:
  "length"` (the `num_predict` cap) → `stopReason: "token_limit"`. A frame carrying `error` → `llm_http` "Ollama could not
  generate an answer. Check that qwen3.5:2b is installed, then start and warm
  free mode in Settings." An unparseable line, or a line over 256 KiB →
  `llm_http`. A stream that ends without `done` → `llm_http` "Ollama stopped
  before finishing the answer, so it is incomplete. Try again." (streamed
  text kept); a whitespace-only answer →
  `llm_http` "The local model returned an empty answer. Try again."
- **Error mapping**: caller abort → `aborted`, checked first; connection
  failure → `llm_http` "Ollama is unavailable. Open Settings and start and
  warm free local mode."; 404 → `llm_http` "Qwen3.5 2B is not installed. Run
  free voice setup to download it."; 500 → `llm_http` "Ollama could not load
  or run the local model. Close unused apps to free several GB of RAM, then
  start and warm free mode again. Check ollama.log if this continues."; any
  other status → `llm_http` "Ollama returned HTTP <n>. Check the local
  service and retry."
- Its own `reqwest` client (no proxy, no redirects, 2 s connect timeout),
  deliberately NOT the shared cloud client of §6.4: a loopback socket has
  nothing to pre-warm and must never inherit a proxy setting. No retry;
  `prewarm()` is a no-op; deadlines 90 s / 300 s (§3).

**Speech (Moonshine service)**:

- `ws://127.0.0.1:8765/transcribe`, no auth. The first text frame must be
  `{"type":"ready","protocol":1}`; anything else is `stt_error` ("Local
  speech is busy or incompatible. Wait for the previous recording to finish,
  then retry."). A refused socket is `stt_connect` ("Local speech is
  unavailable. Open Settings and start and warm free local mode."); a connect
  + handshake that misses the 5 s connect cap is `stt_connect` ("Local speech
  did not become ready. Start and warm it in Settings.").
- Send: binary frames of raw little-endian i16 PCM, 16 kHz mono, exactly
  like Deepgram. PCM callbacks never wait on inference: frames queue (128
  deep) and, if the service falls behind, the recording fails with ONE
  `stt_error` ("Local speech could not keep up with audio. Close other
  CPU-heavy apps and retry.") rather than a silently gapped transcript.
- Receive: `{"type":"transcript","text":<full transcript so far>,"final":bool}`
  → the live transcript (revised full lines, the same full-text semantics as
  Deepgram); `{"type":"error"}` → `stt_error`; ping/pong ignored; anything
  else → `stt_error`.
- Finalize (on stop): drain the queued audio, send `{"type":"finish"}`, and
  wait for `{"type":"done","text":<final transcript>}` within the 5 s
  finalize cap (`stt_timeout` "Local transcription took too long to finish.
  Try a shorter recording." past it). A disconnect before `done` is
  `stt_error` — never a partial success. Finalize is idempotent (a second
  call joins the first); abort is silent.

**Readiness and start** (shell, `local_voice.rs`): `local_voice_status`
probes `GET http://127.0.0.1:11434/api/tags` (running = the body has a
`models` array; available = an entry whose `name` or `model` is
`qwen3.5:2b`) and `GET http://127.0.0.1:8765/health` (ready = `service:
"callhelper-local-speech"` AND `protocol: 1` AND `ready: true` — all three,
so a stranger on the port never reads as ours), each with a 3 s timeout.
`prepare_local_voice` launches only the services that are down — Ollama
first (`ollama.exe serve` with `OLLAMA_HOST=127.0.0.1:11434`,
`OLLAMA_NO_CLOUD=1`, `OLLAMA_MODELS=<setup>\models\ollama`), then the speech
service (`venv\Scripts\python.exe -u server.py --home <setup>`) — from the
setup folder recorded in `%LOCALAPPDATA%\AI Call Assistant\local-voice.json`
(`{ "dataDir": <absolute path> }`; a UTF-8 BOM is tolerated, a relative path
is refused). Each service's stderr appends to `ollama.log` / `speech.log` in
that folder. It then polls every 500 ms for up to 60 s and finally warms the
model (`POST /api/chat` with `messages: []`, `keep_alive: "10m"`, 120 s cap).
Errors are `internal`: not installed → "Free voice is not installed yet. Run
scripts\setup-free-voice.ps1 from the project folder, then retry. Setup
requires Python and several GB of free disk space."; speech never came up →
"Local speech did not start. Check speech.log in your free voice setup folder
and rerun setup if the models are missing."; model missing → "Qwen3.5 2B is
not installed in the running Ollama service. Run free voice setup to finish
the download."; a second concurrent prepare → "Free mode is already starting.
Please wait."

---

## 7. The prompt (exact strings — product behavior)

Build the system prompt as `cachedPrefix` + `styleSuffix` (two blocks for
Anthropic, joined with `\n\n` into one string for Groq and for the local
provider). Byte-stable across calls for identical inputs, and built from the
ACTIVE call profile only (§8) — the prompt never sees the other profiles.

`cachedPrefix` = role instructions, then the call-type line (non-interview
profiles only), then optional sections in a fixed order:

Role instructions (verbatim):

> You are a real-time call assistant helping the user answer questions asked
> of them during a live interview or call. You are given a transcript of what
> the other person just said. Reply with the answer the user should say,
> written in first person, in natural spoken English. Do not add meta
> commentary, greetings, or quotation marks — output only the answer itself.
> If the transcript contains no real question, briefly suggest what the user
> could say next.

**Call-type line** (verbatim; appended directly after the role instructions
for every call type EXCEPT `interview`, which appends nothing here — that
absence is what keeps a migrated v3 profile byte-identical):

- `sales`: `\n\nThis is a sales call: the user is selling to the other person. Answer as the user speaking to a prospect or customer — specific, helpful, and never pushy.`
- `support`: `\n\nThis is a customer support call: the user is helping the other person. Answer as the user speaking to a customer — calm, clear, and focused on resolving their issue.`
- `meeting`: `\n\nThis is a work meeting: the user is a participant, not a candidate. Answer as the user speaking to colleagues — direct and to the point.`
- `other`: `\n\nThis is a general call, not a job interview. Answer as the user speaking to the other person.`

**Header set** by call type. `interview` keeps the v3 headers; every other
type reuses the two text slots as "about the user" and "context for this
call", with a grounding note that names neither a resume nor a target role
(the interview wording would tell a sales rep to ground answers in a job they
are not interviewing for):

- `interview`: resume header `\n\n--- THE USER'S RESUME ---\n` · JD header
  `\n\n--- THE JOB THEY ARE INTERVIEWING FOR ---\n` · grounding note
  `\n\nGround every answer in the resume and target role above. Never invent experience the resume does not support.`
- `sales` / `support` / `meeting` / `other`: background header
  `\n\n--- ABOUT THE USER ---\n` · context header
  `\n\n--- CONTEXT FOR THIS CALL ---\n` · grounding note
  `\n\nGround every answer in the background and call context above. Never invent experience or facts the background does not support.`

Then, in this order (each section trimmed at the edges only — interior
formatting survives verbatim; whitespace-only text counts as absent):

1. If the trimmed resume is non-empty, append the resume/background header +
   resume.
2. If the trimmed job description (call context for non-interview types) is
   non-empty, append the JD/context header + text.
3. If EITHER of those was present, append the call type's grounding note.
   Focus alone does NOT trigger it: a list of things to emphasize is a steer,
   not a background to ground in, and the sentence would be a lie the model
   then tries to obey.
4. If the trimmed focus is non-empty, append
   `\n\n--- WHAT TO EMPHASIZE ---\n` + focus.
5. If the trimmed extra instructions are non-empty, append
   `\n\n--- ADDITIONAL INSTRUCTIONS FROM THE USER ---\n` + text.

**Byte identity**: an `interview` profile with empty focus and empty extra
instructions builds a `cachedPrefix` byte-identical to v3 (role · resume ·
JD · grounding). The migration in §8 relies on this — upgrading changes
nothing the app says and costs no existing user a cache write. Pinned by
test, as is every string above.

**Per-profile cache**: the prefix is a pure function of the active profile's
text — no clock, no map iteration, no environment reads anywhere on the path
from settings to prompt — so it is byte-stable across questions and across
style flips, and changes exactly when the active profile changes (a chip
switch or a saved edit). That switch is a deliberate between-calls cache
write; there is no per-question profile selection by design.

`styleSuffix` by answer style (verbatim; unknown/corrupt style falls back to
balanced):

- **brief**: "Answer in one or two spoken sentences — the shortest reply that
  fully answers the question. No lists, no headings, no lead-in."
- **balanced**: "Be concise and confident: a few sentences for simple
  questions, short structured points for complex ones."
- **detailed**: "Give a structured answer: one sentence that answers
  directly, then three to five short supporting points (what the situation
  was, what you did, what the result was). Keep every point short enough to
  say in one breath — this is spoken aloud, not read."

User message wrapper (verbatim):

```
The other person on the call just said:
"""
<transcript>
"""

What should I say?
```

The style suffix lives AFTER the cache breakpoint so flipping styles is
latency-free (§3). The user message lives outside the system prompt so the
cached prefix stays stable. Style is global, not per profile: a second source
of truth for the chips was deliberately not built.

---

## 8. Settings, secrets, persistence

One JSON settings file (`settings.json`) in the app's data directory. This is
the only shape ever written:

```
{ "profiles": [ { "id", "name", "callType", "resume", "jobDescription", "focus", "extraInstructions" }, … ],
  "activeProfileId": "default",
  "alwaysOnTop": true, "llmProvider": "anthropic", "answerStyle": "balanced", "hotkey": "Ctrl+Shift+Space",
  "launchPlacement": "camera", "streamFollow": "tail",
  "deepgramKey": "enc:…", "anthropicKey": "enc:…", "groqKey": "enc:…",   // each only when set
  "windowBounds": { "x", "y", "width", "height" } }                       // only once saved
```

Fields:

- `profiles` — array of **call profiles**, 1..8. Each: `id`
  (`[A-Za-z0-9_-]{1,40}`, unique), `name` (trimmed, ≤ 60 chars, blank →
  `"Untitled"`), `callType` (`"interview"` | `"sales"` | `"support"` |
  `"meeting"` | `"other"`; unknown → interview), `resume` and
  `jobDescription` (≤ 200 000 chars each, stored verbatim — NOT trimmed;
  profile formatting belongs to the user; for non-interview types the two
  slots hold "about you" and the call context, §7), `focus` and
  `extraInstructions` (≤ 2 000 chars each). Every cap is counted in
  characters, never bytes — a byte cut can split a UTF-8 sequence and turn a
  resume into invalid data on the next load.
- `activeProfileId` — always names an entry of `profiles`; the prompt is
  built from that entry alone (§7).
- `alwaysOnTop` (bool, default true).
- `llmProvider` (`"anthropic"` default | `"groq"` | `"local"`); `local`
  needs no key — `active_llm_key()` is None and both key gates are skipped
  (§6.5).
- `answerStyle` (`"brief"|"balanced"|"detailed"`, default balanced).
- `hotkey` (accelerator string, ≤100 chars, default Ctrl+Shift+Space; empty
  string means "shortcut disabled" and must NOT spring back to the default).
- `launchPlacement` (`"remembered"` | `"camera"`, default **camera**) and
  `streamFollow` (`"tail"` default | `"top"`) — both §9.
- `deepgramKey` / `anthropicKey` / `groqKey` — top-level, `enc:`/`plain:`
  encoded (below), present only when set; a cleared key is omitted, never
  written as `""`, so an absent field and an unset key mean the same thing.
- `windowBounds` (optional `{x,y,width,height}`, physical pixels).

Rules, each one a lesson:

- **Validation with per-field fallback**: the file is user-writable and
  survives upgrades — untrusted input. Every field falls back to its default
  individually; one corrupt value must never cost the user their profiles or
  keys. A missing file is a clean first run. A file that was read but is not
  a JSON object at all (bad syntax, a non-object, invalid UTF-8, empty) is
  renamed to `settings.json.corrupt-<unix-seconds>` (with `-1`…`-9` on a
  collision; an existing backup is never overwritten) BEFORE the app starts
  from defaults, and `storageWarning` names the copy until the first
  successful save. If that rename fails, or the file cannot be read at all
  (permissions, a sharing lock, a transient error — not known to be damaged),
  the app runs on defaults in memory and refuses every write, geometry
  included, so the original is never replaced; `storageWarning` says why.
  Never a crash. Fallback applies INSIDE a profile too: a bad `callType` reads as
  interview and a bad or missing string as `""`; only a non-object entry in
  `profiles` is dropped (there is nothing in it to save). A `profiles` value
  that is not an array at all is the migration case below, and the keys,
  hotkey and bounds beside it survive untouched.
- **Migration** (v3 → profiles): a file whose `profiles` is not a JSON array
  — a v3 flat file, a missing key, or a corrupt value — loads as ONE profile
  `{ id: "default", name: "Default", callType: "interview", resume:
  <top-level "resume">, jobDescription: <top-level "jobDescription"> }` with
  `activeProfileId` `"default"`: exactly the prompt the user had before the
  upgrade (§7 byte identity). The legacy top-level `resume`/`jobDescription`
  are read once and never written again; the file upgrades in place on its
  first save. Keys, hotkey and bounds are untouched by migration.
- **One normalizer**: every profile list that reaches memory — from disk or
  from a patch — passes through one pure, deterministic, idempotent
  function, the ONLY writer of the invariant: truncate to 8; an empty list
  becomes the Default profile; names trimmed and capped, blank → "Untitled";
  resume/JD capped verbatim at 200 000 chars, focus/extra at 2 000; an
  invalid or duplicate id is repaired to the smallest unused `p<n>` in list
  order (the first occurrence keeps its id), so the same broken input always
  repairs to the same ids; `activeProfileId` = the requested id if it names a
  profile, else the previously active id if it still exists, else the first
  profile. Pure on purpose: nothing on the path from settings to prompt may
  smuggle nondeterminism into the cached prefix (§7). On load there is no
  "previous", so an unknown `activeProfileId` falls back to the first
  profile.
- **A switch never rewrites text**: the main-view chips send a patch carrying
  ONLY `activeProfileId`; the core re-runs the normalizer over the STORED
  list, so not a byte of profile text is touched by a switch (a stale
  in-memory copy can never clobber a saved edit). A switch to an id that
  names no profile leaves the active profile unchanged — the returned view
  then tells the chips the truth, because they mirror the persisted id,
  never the click (§9). The Settings form, by contrast, sends the WHOLE array
  plus the selected id; ids it invented are repaired as above and the form
  re-seeds from the returned view.
- **Atomic writes**: write to `settings.json.tmp`, then rename over the real
  file. A crash or full disk mid-write must not truncate the file into
  "defaults" (which silently destroys every setting including keys).
  Update the in-memory cache only AFTER the write lands, so a failed write
  leaves memory matching disk.
- **Secrets**: keys are encrypted with DPAPI and stored as
  `enc:<base64>`; if the OS keystore is unavailable, fall back to a MARKED
  `plain:<base64>` (honestly labeled, still functional). Decode by stored
  prefix, not by current keystore availability. Undecryptable (copied from
  another machine) or unknown-prefix values read as unset — fail closed,
  never hand the raw stored string to a provider.
- **Write-only across the UI boundary**: the frontend NEVER receives key
  material — only `hasDeepgramKey`/`hasAnthropicKey`/`hasGroqKey` booleans.
  A settings patch that omits a key field leaves it untouched; an empty (or
  whitespace-only, trimmed first) key value CLEARS the stored key. Key
  inputs in the UI are password fields whose placeholder shows "saved — type
  to replace" when a key exists; their value is always empty.
- **Hotkey**: trimmed on save (whitespace-only → `""` = disabled — raw
  spaces would make the shortcut registration throw).
- **Window bounds**: saved debounced (500 ms) on move/resize AND flushed on
  close (a dock raises the same events, so docked geometry persists exactly
  like a drag). Saving geometry must NEVER throw during shutdown — swallow
  write failures (it is cosmetic data). On launch, restore through a pure
  sanitizer: clamp size up to the window minimum (the logical 380×520 applied
  unscaled to the saved physical pixels — the OS floor clamps again at any
  DPI, so this only decides visibility), round to integers, and keep the
  position only if at least **40 px** of the window (judged at its CLAMPED
  size) lands on some display's work area on BOTH axes — otherwise drop the
  position and dock the window under the camera (top-centre of its display,
  §9), centring only if no display can be resolved at all. Negative
  coordinates are valid (displays left of primary). Corrupt bounds are
  dropped as a unit while the rest of the file survives. On first run (no
  saved bounds) the builder's LOGICAL 460×700 is kept and no physical
  `set_size` is issued — re-applying a logical size as physical pixels
  shrinks the window by the DPI factor on every hi-DPI display. When
  `launchPlacement` is `"camera"` (the default) the saved POSITION is
  ignored on every launch: the window is re-docked at its saved size before
  it is shown; `"remembered"` restores the sanitized bounds exactly as above.

---

## 9. Window, UX, and exact behavior

**Window**: default 460×700 logical px, minimum 380×520, dark background
`#16181d`, always-on-top per setting, menu bar hidden, Windows capture
exclusion requested and verified at launch (the launch is refused if Windows
did not apply it; whether a conferencing app honours it depends on its capture
method and is only known from recorded tests — a moat feature). Built hidden:
geometry, docking and content protection are applied before the first
visible frame, so there is no jump and no unprotected frame. Single instance:
a second launch focuses/restores the first. The app never navigates: an https
link is bounced to the default browser (handed to the OS as a single
argument — no whitespace, control characters or quotes), and everything else
(file:, http:, javascript:) is dropped.

**Dock to camera**: the answer is designed to be read at eye level, so the
window can sit at the top-centre of the display it is on — directly under a
webcam, where reading the answer looks like eye contact. Geometry is pure
core math on the display's work area (physical px): outer x = work-area
centre − outer width / 2 (a window wider than the display hugs its left edge
so the title bar stays grabbable), outer y = work-area top + **8 px**. Two
sizes: the header's ⬆ button applies the reading **preset** — inner width 600
logical px scaled by the monitor's DPI (capped at the display), height 45 %
of the work area clamped to [520, 720] logical and never taller than the area
under the margin; launch and the restore fallback **keep** the current size
and only move. The frame delta is measured before any resize. The current
monitor is used, else the primary, else the dock fails with "Could not find
the display this window is on." `launchPlacement` (§8): `camera` (default)
re-docks at the saved size on every launch, pre-show; `remembered` restores
the saved bounds. Choosing `camera` in Settings docks immediately (keeping
size) so the choice demonstrates itself. Docking is never automatic
mid-call.

**Main view** (top to bottom — answer first, because in a docked window the
top of the column is what sits under the camera):

1. Header: status dot · app title · **Focus mode** button (`aria-label`
   "Focus mode", `aria-pressed`, title "Show only the answer
   (Ctrl+Shift+F)") · **Dock to camera** button (`aria-label` "Dock to
   camera", title "Move this window to the top of the screen, under the
   webcam"; live even before settings load) · gear (Settings; disabled until
   settings load).
2. **Profile chips** (`role="group"`, `aria-label` "Call profile"): one chip
   per profile, rendered only with 2+ profiles; `aria-pressed` mirrors the
   PERSISTED `activeProfileId`, never the clicked chip; clicking the active
   chip is a no-op; a click sends ONLY `{ activeProfileId }` (§8); a failed
   save shows in the error box. Hidden in focus mode.
3. **"Suggested answer" panel** — the flex-grow reading surface, 16 px text
   at line-height 1.5 (18 px in focus mode). Markdown-rendered; placeholder
   "Your AI-suggested answer will stream here."; "generating…" tag and
   `aria-busy` while answering; latency chip "X.Xs to first word" with a
   hover title breaking down "First word N ms after Stop · transcript
   finalized N ms · full answer X.X s"; in the panel head's right slot the
   **history bar** (hidden until 2+ entries: `nav` "Answer history" with
   "Previous answer"/"Next answer" arrows, the "n/m" label and Clear), then
   the Regenerate and Copy buttons.
4. Error box (`role="alert"`) — directly under the answer it interrupts.
5. Status line (`role="status"`; renders EMPTY until settings have loaded, so
   there is no wrong first frame).
6. Control row: compact Record button (label cycles Record / Starting… /
   Stop & Answer / Record, with the formatted hotkey shown as a chip when
   registered) · style chips (Brief/Balanced/Detailed as a segmented control;
   aria-pressed reflects the PERSISTED style returned by the save, not the
   clicked chip). Right after it, a notice line when the hotkey is TAKEN by
   another app ("<hotkey> is already taken by another app, so the shortcut is
   off — record from this window, or pick a different one in Settings.") —
   register honestly: if registration fails, say so rather than leaving a
   dead key. Then an always-rendered recording row with a reserved height
   (nothing below shifts when it fills): level meter + mm:ss timer, mounted
   only while recording.
7. Ask row (hidden in focus mode): Ask form (text input + Ask button) with
   the **Interview prep** toggle (the lazy-loaded practice library; browsing
   never calls a provider) shown only when the active profile's call type is
   `interview` — switching away closes an open library · a one-line tip
   "Tip: press <hotkey> from the meeting window — the answer streams here."
   while history is empty and the hotkey is registered · an ungrounded-profile
   hint "No resume or job description saved for <name> — answers won't be
   grounded. Add them in Settings." when the active profile has neither and
   every required key is present.
8. Local-mode banner (`aria-label` "Current mode": "Free local voice · no API
   fees", "English system audio · CPU answers may take longer.", a "Local
   setup" button that opens Settings) — only when `llmProvider` is `local`;
   hidden in focus mode.
9. **"Question heard"** — a compact strip that auto-collapses (hidden in focus
   mode). Expanded while starting/recording/finalizing, when idle with no
   question yet (so the placeholder stays visible), or when the user toggled
   it open; collapsed otherwise to a one-line ellipsised caption of the
   question. Every new recording resets the user's toggle. The head is a
   standard disclosure: an `<h2>` containing a `<button aria-expanded
   aria-controls="transcript-body">` named exactly "Question heard", with the
   "live" tag and the caption as siblings; the body is `hidden` and empty
   when collapsed, so the question exists exactly once in the DOM.
   Placeholder "The live transcript will appear here while you record.";
   "Listening…" while recording with nothing heard yet; a "live" tag while
   recording.
10. An sr-only `role="status"` announcement span, permanently mounted.

Settings and the practice library are code-split and load on demand
(pre-fetched on hover/focus of their toggles and 1.5 s after launch).

**Focus mode** (`.main-view--focus`): an answer-only posture for mid-call.
The header, answer panel (18 px), error box, status line and control row
stay; profile chips, the Ask row and its hints, the local banner and the
transcript strip are hidden with the `hidden` attribute — they stay mounted,
so no live region is ever inserted with its content. Toggled by the header
button or by **Ctrl+Shift+F** while this window has focus (a plain window
keydown, NOT a global shortcut; ignored when the target is a text field). Not
persisted: a window that opens with its controls hidden reads as broken on
the next launch.

**State machine** (frontend): `idle → starting → recording → finalizing →
answering → idle`. Status lines per state: "Ready — press Record while the
other person is speaking" (idle; append "or <hotkey>" when registered) ·
"Opening the microphone feed…" (starting) · "Recording call audio…" ·
"Finalizing transcript…" · "Generating answer…" · "Done — press Record for
the next question" after completion · at the 120 s cap: "Reached the 120s
limit — answering now". First run with a missing key for the SELECTED
provider (or missing Deepgram key): "First run: open Settings (gear icon) and
add your API keys" — never shown while `local` is selected, which needs no
keys (§6.5). While settings have not loaded the line renders empty (main
view item 5).

**Gestures and edge rules**:

- Record toggles: recording → stop; starting → abort (silent teardown);
  idle/answering → start (starting over a streaming answer supersedes it).
- The global hotkey does exactly what the Record button does, but is IGNORED
  while Settings is open (the user may be typing the hotkey itself).
- The Ask box is disabled during starting/recording/finalizing (and the
  submit handler independently refuses, belt and braces) — but stays ENABLED
  during answering: asking over a streaming answer supersedes it. The input
  is cleared only when the ask was accepted; kept on failure so the user can
  retry. Empty/whitespace submits never reach the core.
- Regenerate is visible when the VIEWED entry has a question and state is
  idle or answering; it re-asks the viewed entry's question as a NEW history
  entry.
- Copy copies the markdown SOURCE (bullets survive pasting), only visible
  when an answer exists; shows "Copied ✓" for ~1.2 s; announces to screen
  readers; clipboard failure surfaces in the error box.
- History: last **6** entries. A new recording/ask pushes a live entry and
  jumps the view to it (trimming the oldest beyond 6 — the in-flight entry
  can never be trimmed). An aborted/failed attempt that captured NOTHING
  (whitespace-only counts as nothing) is discarded; one that captured a
  question or partial answer is retired into history (the transcript is user
  work; a half-streamed answer looks like data loss if it vanishes). Clear
  is enabled only when idle; it wipes everything, announces "History
  cleared", and moves focus to Record (the button it lived on disappears).
- Stale events (wrong session id) change nothing, ever. Events arriving
  before the start/ask call resolved (id not yet adopted) are dropped.
- Errors: show the message, return to idle, tear down capture. An error
  during a streaming answer keeps the partial answer.
- Answer panel autoscroll (`streamFollow: "tail"`, the default): stick to the
  bottom only if already at the bottom (within ~28 px) — a user who scrolled
  up to re-read must not be yanked down by each token. Switching history
  entries resets scroll to top. `streamFollow: "top"` (teleprompter pacing)
  keeps the reader parked at the opening sentence — implemented purely by
  never deeming the reader "following" (seeded at mount and on every entry
  switch), so the stick branch itself is unchanged.
- Rendering is coalesced to one paint per animation frame while streaming,
  and `llm:delta` events are coalesced in the session hook: the FIRST delta
  of a burst dispatches synchronously (first paint as early as ever); later
  deltas inside a 16 ms window are merged into one dispatch by a timer, not
  `requestAnimationFrame` (WebView2 throttles rAF in a minimized window, and
  the hotkey flow runs minimized). Every other event handler, and a
  supersede from Record or Ask, flushes the buffer first, so event ORDER is
  preserved and a buffered token lands on the retiring entry.

**Accessibility**: `aria-live="polite"` on the answer panel with `aria-busy`
during streaming (announced on completion, not per token) · `role="alert"`
errors · `role="status"` status line · every live region (the status line,
the two sr-only `role="status"` spans, the answer body) is permanently
mounted with only its TEXT changing — several screen reader/browser combos
ignore a region inserted together with its content · focus moves into
Settings heading on open and back to the gear on close, and to Record after
Clear · Escape closes a CLEAN Settings form (a dirty one shows the guard
below, and Escape on the guard means "keep editing") · `aria-pressed` on
style and profile chips mirrors the PERSISTED value, never the click · the
transcript strip is a standard disclosure (`aria-expanded`/`aria-controls`)
and focus mode hides rows with the `hidden` attribute rather than unmounting
them · visible focus rings · `prefers-reduced-motion` respected · AA
contrast on the dark theme.

**Settings view** (replaces main view, not a dialog; code-split and loaded on
demand): a draft of the loaded view in three sections, a scrolling form with
the actions pinned to the bottom (sticky, so Save is never below the fold):

- **Keys & model**: "Answer model" select ("Claude Haiku 4.5 (recommended)" /
  "Groq GPT-OSS 120B (fastest)" / "Free local voice (Qwen3.5 2B +
  Moonshine)") · the key fields the selected provider needs — "Deepgram API
  key", "Anthropic API key", "Groq API key (only for the Groq preset)" — the
  others carry the `hidden` attribute rather than unmounting, so a typed key
  survives a provider round trip; with local selected all three are hidden
  and the local readiness panel appears instead ("Free local voice": live
  Ollama / Qwen3.5 2B / English speech checks, "Start and warm free mode" and
  "Check status" buttons, a setup note) · "Answer style" select.
- **Profile**: "Profile" select + New / Duplicate / Delete (New and Duplicate
  disabled at 8 profiles, Delete disabled with one; the selected profile is
  both the one being edited and the one made active on Save — help text "The
  selected profile grounds the next answer once you save.") · "Profile name"
  (maxLength 60) · "Call type" select (Job interview / Sales call / Customer
  support call / Work meeting / Other) · "Focus (optional)" (placeholder
  "What to emphasize, e.g. Rust, tokio, async") · a textarea labelled
  "Resume" for an interview profile and "About you (optional)" otherwise · a
  textarea labelled "Job description" for an interview profile and "Call
  context (account, product, agenda)" otherwise · "Extra instructions
  (optional)" · each text field carries a character counter "n / 200,000
  characters" (2,000 for focus and extra instructions) · with local selected,
  the core's budget for the profile being edited and the style chosen in the
  form (unsaved, debounced 250 ms, obsolete answers discarded, shown only for
  the profile and style it was computed for — §4 `local_prompt_budget`):
  `ok` "N bytes left for the question in free local mode (U of 7,000 used by
  the instructions and this profile)." · `tight` (a warning) "Only N bytes
  left for the question in free local mode (most spoken questions need about
  200). Shorten this profile or use a cloud model." · `over` (an error note)
  "Too long for free local mode: … so no question fits. Recording and Ask are
  refused in local mode with this profile; you can still save it for a cloud
  model." · a failed check says "Could not check the free local mode size.
  The limit is still checked when you record or ask." Save stays enabled in
  every state. New profile ids are client-generated
  (UUID); the core repairs anything invalid (§8).
- **Shortcut & window**: "Global shortcut" text field with the default
  (`Ctrl+Shift+Space`) as placeholder and help "Modifier+key combination,
  e.g. Ctrl+Shift+Space. Works while any app is focused. Leave empty to turn
  the shortcut off. Applied when you save." · "Keep this window always on
  top" checkbox with help "Keeps the assistant above your call window so you
  can read answers while the meeting is focused." · "Window position at
  launch" select: `remembered` → "Remember where I left it", `camera` → "Dock
  under the camera (top centre)", help "Docked windows sit at the top of the
  screen, so reading the answer looks like eye contact with the webcam. The
  ⬆ button on the main screen docks at any time." · "While an answer
  streams" select: `tail` → "Follow the newest text", `top` → "Stay at the
  opening sentence (teleprompter)".

Then a note: "Keys are stored encrypted and never shown again. Windows
capture exclusion was verified at launch; test your sharing app before relying
on it." · Save + Back buttons · "Saved ✓"
note for ~1.5 s. Save sends the whole form (`profiles`, `activeProfileId` =
the selected profile, `alwaysOnTop`, `llmProvider`, `answerStyle`, `hotkey`,
`launchPlacement`, `streamFollow`) BUT only the key fields the user actually
typed into; on success every field re-seeds from the returned view (ids may
have been repaired, names trimmed, the hotkey normalized) and the key drafts
reset. Failures report in a settings-local error box (the main one is hidden
behind this view). **Save lock (R3)**: while a save is in flight a disabled
fieldset covers every control — every field, the key inputs, the profile
select, New/Duplicate/Delete, Save (which reads "Saving…", with a "Saving…"
status) and Back — and Escape and Back are refused; a failed save unlocks the
form with the draft and typed keys intact. **Revisions (R5, ADR 016)**: Save
sends `expectedRevision` = the revision the form was seeded from. A
`settings_conflict` keeps the draft and the typed keys, disables Save and
shows `role="alert"` "Settings changed elsewhere — reload. Your unsaved edits
are kept." with a Reload button; Reload fetches the committed view and
rebases the draft (every field the user changed keeps the user's value,
untouched fields and untouched profiles take the committed ones), then says
"Reloaded. Your unsaved edits were kept; review them and Save." A newer view
arriving while the form is open is followed silently by an untouched form and
raises the same banner on an edited one (undoing the edits follows the view
and clears the banner). When the lock lifts, focus returns to Save. The App installs a settings view
only when its revision is at least the one held, so responses arriving out
of order never reinstall an older view. A non-null `storageWarning` shows as
a note at the top of Settings and once in the main error box at launch.
**Dirty guard**: Escape or Back on a form that differs
from the loaded view (or has a typed key) shows an inline `role="alertdialog"`
"Unsaved changes" asking "Discard unsaved changes?" with Discard / Keep
editing (focus lands on Keep editing) instead of closing; a clean form still
closes on the first Escape.

---

## 10. Markdown answer rendering (security-critical)

The answer panel renders model output as a markdown subset. Model output is
UNTRUSTED. Non-negotiable rules:

- Supported: paragraphs, headings (rendered demoted: model `#`→`h3` …
  capped at `h6` — the page owns h1/h2), bullet and `n.`/`n)` numbered lists
  (with start number, lazy continuation of wrapped items, loose lists —
  a blank line ends the list only if no item follows), fenced code blocks
  (``` or ~~~, info string dropped, unterminated fence at EOF = open block),
  thematic breaks, bold, italic (with CommonMark-ish flanking rules —
  `snake_case` must not italicize), inline code (backtick runs, exact-length
  closers, one space of padding stripped), backslash escapes.
- **Links are deliberately NOT parsed** — `[text](url)` stays literal text.
  There is no href, so there is nothing to sanitize and no `javascript:` to
  smuggle.
- **Every string reaches the DOM as a text node** — never
  `dangerouslySetInnerHTML`, never an attribute value derived from model
  text.
- **Streaming**: the renderer is called with a growing source string many
  times per second. Requirements: (a) rendering every prefix must never
  throw; (b) the final DOM must be byte-identical to rendering the full text
  once; (c) completed blocks keep their DOM nodes across updates; (d) an
  unchanged source is a no-op; (e) don't do O(document) work per frame if you
  can diff/parse from the last stable block boundary — but a wrong
  incremental parse is worse than an honest O(doc) one, so only optimize
  behind the invariant test.
- Frontend CSP: `default-src 'self'; style-src 'self'` (adjusted minimally
  for Tauri's needs) — the renderer must not require inline script/style for
  model content.

---

## 11. Production hardening

- **Single instance** (§2). **Content protection** always on.
- **Crash logging**: panics and unexpected errors in the core append a
  timestamped line to `crash.log` in the app data dir. A failure in one
  answer pipeline must never take the process down — each session is
  isolated; the worst case is one failed answer and a structured error event.
- **Frontend crash recovery**: if the webview crashes, reload it (at most
  once per 10 s, so a boot-crash doesn't flicker forever).
- **Window geometry** persistence + off-screen recovery (§8).
- Log nothing sensitive: never keys, never profile text (resume, job
  description, focus, extra instructions), never transcripts.

---

## 12. Testing (a first-class deliverable)

- **No network, no audio devices, no live providers in any test.** Core:
  trait-injected fakes; frontend: Vitest + Testing Library against the real
  components with a mocked command/event bridge.
- Cover, at minimum: every invariant in §5 (including the races) · both
  providers' error-mapping matrices and the retry policy matrix · the SSE
  parser under hostile chunking · Deepgram frame parsing under hostile
  input · prompt byte-stability, the cache-split rule, the verbatim
  call-type/section strings and the interview byte-identity rule (§7) ·
  settings validation/fallback/atomicity/secret semantics, profile
  normalization (caps, id repair, determinism), the v3 migration and the
  switch-never-rewrites rule (§8) · bounds sanitization and dock geometry
  cases · the local provider (NDJSON under byte-split chunks, the 7 KB
  refusal) and the loopback speech client against a scripted local server
  (finalize joins, a disconnect is never a partial success) · the markdown
  streaming-vs-batch invariant and the XSS suite · the full frontend flows
  driven through the mocked bridge with assertions on real DOM state.
- **Document every test** in `docs/TESTING.md`: one bullet per test — what it
  verifies and *why it exists* (the failure mode it guards). Test counts live
  ONLY there; every other document points at it.

---

## 13. Definition of done

1. `cargo test` and the frontend test suite fully green; Rust `clippy` clean;
   TypeScript strict typecheck clean.
2. `tauri build` produces a working Windows installer; `tauri dev` runs.
3. Manual QA script passes (in the README).
4. README documents setup (keys and call profiles), the latency
   architecture, the caching honesty note, the Groq model-pinning note, and
   an approximate cost note.
5. No TODOs on the critical path; comments explain WHY (invariants, races,
   failure modes), not what.

# Architecture

How the code delivers the contract in [SPEC.md](SPEC.md). The spec says *what*
must be true and cites the bug that made each rule; this document says *where*
each rule lives and *why the mechanism is shaped the way it is*. Every claim
below is checkable against a file and, where it matters, a line.

The one number that shaped everything: **stop-to-first-word of roughly one
second** (§1). Every structural decision here is either protecting that number
or making being fast safe.

---

## 1. The system at a glance

Three layers, two crates, one process:

- **`app-core`** (`src-tauri/core/`) — the entire pipeline: capture,
  resampling, the Deepgram WebSocket (or the loopback Moonshine one), the
  LLM HTTP streaming (Anthropic and Groq over SSE, Ollama over NDJSON), the
  session state machine, settings/secrets/profiles/geometry math. It knows
  nothing about Tauri; every external dependency enters through a trait
  (`SttConnector`/`SttStream`, `LlmProvider`/`LlmSink`,
  `AudioCapture`/`AudioSink`, `EventSink`), which is why
  `cargo test -p app-core` runs with no network and no device (count in
  TESTING.md).
- **Tauri shell** (`src-tauri/src/`) — exactly the glue: IPC envelopes
  (`commands.rs`), event emission (`events.rs`), window lifecycle and
  docking (`window.rs`), the one global hotkey (`hotkey.rs`), the local
  voice services' probe/launch (`local_voice.rs`), the crash log
  (`logging.rs`), the process state (`state.rs`). Unit tests cover its pure
  seams (count in TESTING.md).
- **React frontend** (`src/`) — a thin view. All decisions live in a pure
  reducer (`src/state/reducer.ts`); the only Tauri contact point is one
  `Bridge` object (`src/bridge.ts`). Settings and the practice library are
  lazy chunks (`views/SettingsView.tsx`, `components/PracticeLibrary.tsx`)
  behind one error boundary (`components/DeferredView.tsx`). Test count in
  TESTING.md.

The full file tree, including the Python speech service under `local-voice/`
and the setup scripts under `scripts/`, lives in the root README
(“Architecture”) and nowhere else, so it cannot drift twice.

```mermaid
flowchart LR
    subgraph FE["Frontend (src/)"]
        UI["views + components"]
        RED["state/reducer.ts<br/>pure state machine"]
        HOOK["state/useSession.ts<br/>effects: bridge, timer"]
        MD["markdown/<br/>in-repo renderer"]
        BR["bridge.ts<br/>the one Tauri seam"]
        UI --> HOOK --> RED
        UI --> MD
        HOOK --> BR
    end

    subgraph SHELL["Tauri shell (src-tauri/src/)"]
        CMD["commands.rs<br/>invoke + Envelope"]
        EVT["events.rs<br/>TauriEventSink"]
        ST["state.rs<br/>AppState, capture slot"]
        WIN["window.rs (restore, dock) / hotkey.rs / logging.rs"]
        LV["local_voice.rs<br/>probe + launch the local services"]
    end

    subgraph CORE["app-core (src-tauri/core/)"]
        SM["session/machine.rs<br/>SessionManager"]
        AUD["audio/<br/>WASAPI loopback + resample"]
        STT["stt/<br/>deepgram.rs · local.rs · frame.rs"]
        LLM["llm/<br/>anthropic · groq · local · retry · warm · sse · http"]
        STORE["store/<br/>settings + profiles + revision, effects reconciler, DPAPI, bounds + dock math"]
    end

    BR -- "invoke (Result envelope)" --> CMD
    BR -- "local_voice_status / prepare_local_voice" --> LV
    EVT -- "Tauri events, id-tagged" --> BR
    CMD --> SM
    CMD --> STORE
    SM --> STT
    SM --> LLM
    AUD -- "frames via ForwardingAudioSink" --> SM
    SM -- "SessionEvent" --> EVT

    STT -- "wss: tokio-tungstenite" --> DG[("Deepgram<br/>nova-3 listen")]
    STT -- "ws: loopback, local mode only" --> MS[("Moonshine service<br/>127.0.0.1:8765")]
    LLM -- "https: one shared reqwest::Client" --> AN[("Anthropic<br/>claude-haiku-4-5")]
    LLM -- "https: same client, same pool" --> GQ[("Groq<br/>openai/gpt-oss-120b")]
    LLM -- "http: own no_proxy client, local mode only" --> OL[("Ollama<br/>127.0.0.1:11434 qwen3.5:2b")]
    LV -. "spawns, probes" .-> OL
    LV -. "spawns, probes" .-> MS
```

The provider wires are deliberately asymmetric in ownership:

- The **Deepgram wire** is owned by a single background task per stream
  (`src-tauri/core/src/stt/deepgram.rs:1-12`) — one owner means one
  classification site for every failure. The **Moonshine wire**
  (`src-tauri/core/src/stt/local.rs`) copies the shape: one spawned driver
  per stream, PCM pushed through a channel so the audio callback never waits
  on inference, every failure classified once as `stt_error`.
- The **cloud LLM wire** is owned by whichever session is answering, but
  always through the process-wide `shared_client()`
  (`src-tauri/core/src/llm/http.rs:87-92`), because the connection pool inside
  that client is what pre-warming warms (§6.4).
- The **local LLM wire** (`src-tauri/core/src/llm/local.rs:26-36`) uses its
  own loopback client on purpose — `no_proxy`, no redirects, a 2 s connect
  timeout — and has no pre-warm: there is no TLS handshake to hide, and
  loading the model is the explicit "Start and warm" gesture in Settings
  (ADR 005, Scope). The choice of speech and answer wire is a *capability of
  the provider* (`LlmProviderKind::uses_deepgram()` / `needs_cloud_keys()`,
  `core/src/llm/mod.rs:66-90`), not a scattered `== Local` check.

Crossing rules at the boundaries:

- Frontend → core: commands return `{ok:true,value}|{ok:false,error}` — both
  outcomes travel the resolve lane, so a *rejected* invoke can only mean the
  shell itself broke — or, the one deliberate exception, that a
  `set_settings` patch carried a value outside a closed enum
  (`llmProvider`, `answerStyle`, `launchPlacement`, `streamFollow` are typed
  on the Rust side, `core/src/store/mod.rs:302-316`) and failed argument
  deserialization before the command body ran. The bridge folds any
  rejection into `{ code: "internal" }` (`src-tauri/src/commands.rs:1-4`,
  `src/bridge.ts:41-50`). Lenient parsing (`parse_or_default`) is reserved
  for the untrusted settings *file*, never the wire (§4).
- Core → frontend: every event carries `sessionId`, and the frontend drops
  anything not belonging to the session it currently tracks
  (`src/state/reducer.ts:157-160`). This one mechanism is what makes
  supersession safe end to end (§4).
- Key material never crosses: `SettingsView` carries only
  `hasDeepgramKey`-style booleans (§8).

---

## 2. The latency pipeline: Record → Stop → first word

The design principle is that **nothing serializable is serialized**. Every
piece of setup that could sit inside the stop-to-first-word window is instead
overlapped with a phase the user cannot perceive.

```mermaid
sequenceDiagram
    participant U as User
    participant FE as Frontend
    participant SM as SessionManager
    participant CAP as WASAPI capture
    participant DG as Deepgram driver
    participant LLM as LLM provider

    U->>FE: Record
    FE->>SM: start_session()
    Note over SM: slot claimed (id) BEFORE any await
    par three things at once
        SM->>DG: connect() — returns handle instantly,<br/>handshake resolves in background
    and
        SM->>LLM: prewarm() — TLS handshake into the shared pool
    and
        SM->>CAP: open loopback device (~100 ms, off the invoke lock)
    end
    CAP--)DG: frames buffer in channel while socket opens<br/>(15 s cap, drop oldest)
    DG--)DG: socket opens: buffered frames flush in order
    loop while recording
        CAP--)SM: push_audio(id, pcm, rms)
        SM--)DG: binary frame
        DG--)FE: stt:partial (live transcript)
    end

    U->>FE: Stop & Answer
    FE->>SM: stop_session(id)
    Note over SM: THE CLOCK STARTS: Instant::now()<br/>sent under the slot lock
    par overlapped with the finalize
        SM->>LLM: prewarm() again — handshake overlaps the flush
    and
        SM->>DG: finalize(): flush queued audio, send CloseStream
        DG--)SM: server flushes held-back tail, closes (5 s cap)
    end
    SM->>LLM: stream_answer() — fires the instant the transcript is final
    LLM--)FE: llm:delta (first token: disarms 10 s timer,<br/>timestamps firstTokenMs)
    LLM--)FE: llm:done { metrics }
```

What overlaps what, and where:

- **Record press.** `SessionManager::start` claims the slot and returns the
  id before any network round-trip
  (`src-tauri/core/src/session/machine.rs`, `SessionManager::start`), then spawns the driver.
  The Deepgram connector also returns its stream handle *before* the
  handshake resolves (`src-tauri/core/src/stt/deepgram.rs:100-105`), so
  capture starts the instant the user clicks. First words land while the
  socket is opening; they buffer in the audio channel and flush at open in
  order, capped at ~15 s with oldest-dropped — the newest audio is the speech
  the user is asking about, and it is the connect that is slow, not the
  speech that is wrong (`deepgram.rs:47-51`, `deepgram.rs:228-246`).
- **Prewarm at Record.** The TLS+TCP handshake to the answer origin overlaps
  the whole recording (`machine.rs` (`SessionManager::start`), plus a second one in
  `src-tauri/src/commands.rs:180`), so by Stop the pool holds a live
  connection — and TCP keepalive probes from 20 s idle keep a NAT or VPN
  gateway from forgetting it during a long question (`http.rs:47-60`).
- **The device open is off the runtime** (RS-2): the ~100 ms WASAPI open
  runs on tokio's blocking pool (`commands.rs:200-210`). On a runtime worker
  it used to park the STT dial that `start` had just spawned into the
  worker's non-stealable LIFO slot, so the socket did not begin dialling
  until the device was open. A panic in the open folds into the same cancel
  path as a device error, so the machine session never ends in a misleading
  `no_speech`.
- **Stop press.** The stop *instant* is captured where the request lands —
  inside the slot lock, sent over a oneshot to the driver
  (`machine.rs` (`SessionManager::stop`)) — so the driver can never observe `Finishing`
  without the timestamp, and metrics never depend on when a task happens to
  resume. `stop()` prewarms again *before* returning
  (`machine.rs` (`SessionManager::stop`)): the handshake refresh overlaps the STT flush
  instead of landing inside the latency window. Ask does **not** prewarm
  (RS-1, `machine.rs` (`SessionManager::ask`)): its answer request fires microseconds later
  from the spawned task, so a warm could only race it for the pool.
- **The finalize.** The drain flushes audio still queued at stop (the final
  words of the question), sends `CloseStream`, and reads the server's
  held-back tail until its close (`deepgram.rs:334-398`). §6.1's deliberate
  omission of `endpointing`/`no_delay` lives here as a pinned test: this
  client stops when the user clicks, never when an endpointer speaks, so
  those knobs only cost smart_format quality (`deepgram.rs:33-37`).
- **The answer.** `run_answer` fires the moment the transcript is final
  (`machine.rs` (`drive_recording`)). The first delta both timestamps `firstTokenMs` and
  disarms the 10 s first-token timer (`machine.rs` (`SessionLlmSink::on_delta`),
  `machine.rs` (`run_answer`)). Deltas are pushed to the sink the moment they are
  decoded, never batched (`src-tauri/core/src/llm/anthropic.rs`, `apply_event`).

All limits live in one place — `src-tauri/core/src/session/mod.rs:131-146`:
STT connect 5 s, STT finalize 5 s, LLM first token 10 s, LLM total 60 s,
recording cap 120 s — plus the free local pair, 90 s first token / 300 s
total, because a 2B model on a laptop CPU can spend longer than the whole
cloud first-token cap just ingesting the prompt. Which pair applies is a
capability of the provider, not a kind check: `LlmProvider::answer_limits()`
defaults to `AnswerLimits::CLOUD` and the local provider overrides it with
`AnswerLimits::LOCAL` (`session/mod.rs:162-175`, `llm/local.rs:176-181`);
`run_answer` arms its deadlines from that (`machine.rs` (`run_answer`)). All are
enforced in the core so the frontend cannot drift.

Metric honesty rules (§3) are enforced in `Metrics::finish`
(`session/mod.rs:43-49`): a provider that returns a full answer without ever
streaming a delta reports `firstTokenMs = totalMs`, never 0 — 0 renders as
"instant" and lies about the one number the app is judged on. Typed questions
report `sttFinalizeMs: 0` because there was no STT stage
(`machine.rs` (`SessionManager::ask`)).

---

## 3. The session state machine

`src-tauri/core/src/session/machine.rs` — the crown jewels (§5). One live
pipeline at a time, encoded as `Mutex<Option<Active>>` rather than a map
(`machine.rs` (`SessionManager`)): the slot *type* makes supersession impossible to forget
on any path.

### The core's three phases

The phase lives inside the slot's lock, never in the driver task — the driver
learns about transitions strictly after they happened (`machine.rs` (`Phase`)).

```mermaid
stateDiagram-v2
    [*] --> Connecting: start() claims slot,<br/>spawns drive_recording
    Connecting --> Recording: install_stream —<br/>only if still the slot owner
    Connecting --> [*]: lost the slot / connect failed<br/>(silent or one error)
    Recording --> Finishing: stop() Taken —<br/>stop instant sent under the lock
    Recording --> Finishing: 120 s cap —<br/>begin_finishing(), core-internal
    Recording --> [*]: STT death / device death<br/>(one error, torn down)
    Finishing --> [*]: llm:done / session:error /<br/>silent abort
    note right of Finishing
        ask() sessions are born here:
        typed questions were "stopped"
        the instant they were submitted
    end note
```

### The invariants and their mechanisms

| § | Invariant | Mechanism | Where |
|---|---|---|---|
| 5.1 | Supersession: start/ask aborts the active session; its late events — including its `done`, including the socket death our abort caused — are dropped | `Inner::replace` swaps the slot, then `teardown()` kills the **gate first**, then cancels, then aborts the stream — so nothing the teardown itself provokes can reach the UI. `complete()` re-checks slot ownership before emitting `done` | `machine.rs` (`teardown`), `machine.rs` (`Inner::complete`) |
| 5.2 | Latest-start-wins: a slow connect that resolves after losing must not install itself | The id is claimed **before** the connect await; `install_stream` is keyed by id + `Connecting` phase, and a loser gets `false` and aborts its own stream, silently. The same rule is re-applied at the shell for the capture swap via `is_active()` | `machine.rs` (`Inner::install_stream`), `machine.rs` (`drive_recording`), `src-tauri/src/commands.rs:156-169` |
| 5.3 | Stop contract: took / not-took, and NotTaken emits nothing | `stop()` only takes in `Recording` phase; every other case returns `NotTaken` without touching anything. The shell converts NotTaken into an error envelope — the one channel that can tell the UI nothing is coming | `machine.rs` (`SessionManager::stop`), `commands.rs:188-209` |
| 5.4 | Audio routing: frames only for the live, not-yet-stopped session | `push_audio` matches id **and** `Phase::Recording`; post-stop and stale-id frames vanish (they would race the CloseStream flush) | `machine.rs` (`SessionManager::push_audio`) |
| 5.5 | One honest `stt_error` for a mid-recording/mid-finalize death; a *late* death must not kill a streaming answer | Mid-stream: error → abort → `fail()`. During finalize, a `biased` select polls the STT channel **before** the finalize future, so a death that queues its error and resolves the finalize with truncated text in the same instant always loses to the error. After finalize: `drop(rx)` makes a late socket close structurally unreachable | `machine.rs` (`drive_recording`), `machine.rs` (`drive_recording`), `machine.rs` (`drive_recording`) |
| 5.6 | One error per stream, never after abort; pre-registration errors are queued | `SessionSttSink.errored: AtomicBool` enforces at-most-once even against a misbehaving stream; the unbounded channel queues an error fired inside `connect()` itself until the driver listens. `Inner::fail` takes slot ownership exactly once and never emits `aborted` | `machine.rs` (`SessionSttSink`), `machine.rs` (`Inner::fail`) |
| 5.7 | Empty transcript → `no_speech`, never an LLM call on an empty prompt | Trim-and-check after finalize, canonical message pinned as a constant | `machine.rs` (`drive_recording`), `core/src/error.rs:99` |
| 5.8 | Ask validates before superseding; typed and spoken questions share one event shape | Validation runs before `replace()`; the session is born in `Finishing`; the trimmed question replays as one final `stt:partial`; `sttFinalizeMs` is 0 | `machine.rs` (`SessionManager::ask`) |
| 5.9 | Timeout interplay; nothing paints after an error | Deadlines are `stopped_at + limit` (`sleep_until`), so the clock is the stop instant, not task-resume time. The first delta disarms the first-token arm; on any timeout `fail()` kills the gate **before** emitting, so racing deltas are suppressed | `machine.rs` (`run_answer`), `machine.rs` (`Inner::fail`), `machine.rs` (`SessionLlmSink::on_delta`) |
| 5.10 | Cancel is silent | `cancel()` takes the slot and runs `teardown()` — no done, no error; stale ids are a no-op | `machine.rs` (`SessionManager::cancel`) |
| 5.11 | Slot released exactly once, on every outcome | Every terminal path funnels through `release_with(id, outcome)`, keyed by id — a second call, or a call after supersession already emptied the slot, is a no-op | `machine.rs` (`Inner::release_with`) |
| 5.12 | Recorded outcome, consistent with the slot (ADR 015) | `Slot { active, outcomes }` share ONE mutex: `replace` records the new id `active` and the displaced one `cancelled`; `release_with`, `cancel` and `fail`/`complete` record the terminal outcome in the same critical section; `Slot::settle` only changes an `active` entry (settle once). The log keeps the last `OUTCOME_RETENTION` = 16 ids. `Gate` records what it let through under its own lock, so `failed.partial` is exactly the text shown | `machine.rs` (`Slot`, `Gate`, `Inner::fail`) |
| 5.13 | Supervised driver: a crash is an error, not a hang | `DriverGuard` lives in every driver task; its `Drop` (run on return, early return, and panic unwind) calls `fail(id, internal MSG_DRIVER_STOPPED)` — a no-op unless the session still owns the slot — cancels the token, and aborts the STT stream it still owns. Poison-tolerant `lock()` everywhere so the guard cannot panic mid-unwind. `OwnedStream::abort` makes every abort path tear the socket down once | `machine.rs` (`DriverGuard`, `OwnedStream`) |

Device deaths get §5.5's treatment applied to audio:
`SessionManager::device_error` is phase-aware — fatal while `Connecting` or
`Recording`, ignored once `Finishing`, because a capture death after stop must
not kill an answer that is already streaming (`machine.rs` (`SessionManager::device_error`)). The shell
routes device errors *through* the machine rather than emitting directly,
precisely to inherit phase-awareness, once-only, and stale-id dropping
(`src-tauri/src/state.rs:63-70`).

### The frontend machine, and how the two relate

`src/state/reducer.ts` runs `idle → starting → recording → finalizing →
answering → idle` (§9) as a pure reducer; `src/state/useSession.ts` only
decides *when* to dispatch.

```mermaid
stateDiagram-v2
    direction LR
    idle --> starting: Record
    starting --> recording: "record/started (id adopted)"
    starting --> idle: abort (silent) / start failed
    recording --> finalizing: Stop pressed, or local 120 s tick
    finalizing --> answering: first llm:delta
    answering --> idle: llm:done / session:error
    answering --> starting: Record again (supersedes)
    idle --> answering: Ask (typed)
```

The mapping is deliberately loose — the core is authoritative, the frontend
follows:

- **Adoption (ADR 015).** The hook awaits listener readiness (`bridge.on`
  returns `{ ready, unsubscribe }`) before any `start_session`/`ask`; a
  failed registration fails the attempt visibly and the next attempt
  re-subscribes. `activeId` is null while the call is in flight and the
  reducer still drops every event it cannot match (`reducer.ts`), but the
  HOOK holds the in-flight attempt's session events in a bounded
  `PendingBuffer` (deltas merged, partials replaced per id, audio levels not
  held, 64 entries). `adopt()` dispatches `record/started`/`ask/accepted`,
  replays only the adopted id's held events, then reconciles: a terminal
  `outcome` in the `SessionStart` envelope is dispatched as
  `session/outcome`; an `active` one triggers one `session_outcome` lookup.
  `session/outcome` is guarded by attempt key AND id and is idempotent with
  the live `llm:done`/`session:error`. A resolution arriving for a superseded
  attempt is detected by `attemptRef` key mismatch; its held events are
  discarded and the orphan session is cancelled, never adopted
  (`useSession.ts`).
- **Per-entry outcome (R2).** `HistoryEntry.status` (`pending · completed ·
  incomplete · cancelled · limited`) and `reason` are set when the live entry
  settles: `llm:done` → `completed`/`limited` by `stopReason`; an error →
  `incomplete` with its message; `aborted`, cancel or supersession →
  `cancelled`. The AnswerPanel head shows the tag and a caption shows the
  reason; `metrics` stays timing only.
- **The stop race.** The Stop button and the global hotkey can fire in the
  same tick; `stopIssuedFor` ensures the core sees exactly one stop
  (`useSession.ts:50-93`). A rejected stop (NotTaken) tears down locally —
  waiting in "Finalizing…" would hang forever (`reducer.ts:216-221`).
- **The 120 s cap.** The core's `MAX_RECORDING` timer is armed when recording
  starts (`machine.rs` (`drive_recording`)) and, on firing, behaves exactly like a user stop:
  `begin_finishing()`, prewarm, answer normally — *emitting no event for the
  auto-stop itself* (`machine.rs` (`drive_recording`)). The frontend's wall-clock tick
  (WebView2 throttles timers in background windows, so the clock counts
  `performance.now()` deltas, not fires — `useSession.ts:181-197`) crosses
  the cap strictly later, flips to `finalizing` **locally**, keeps
  `activeId`, and issues no stop — because that stop would come back "not
  taken", and tearing down on the refusal is the double-stop race that used
  to drop every capped recording's answer (`reducer.ts:260-277`). As belt and
  braces, `llm:delta`/`llm:done` are accepted even from the `recording`
  state: an answer delta can only exist after the recording ended, so it is
  itself the authoritative "the core auto-stopped" signal
  (`reducer.ts:297-339`).
- **Delta coalescing.** Every `llm:delta` used to be one IPC crossing *and*
  one full App-tree render; on a fast provider that is dozens of renders a
  second inside the window the app is judged on. The hook now runs a
  leading+trailing coalescer (`useSession.ts:55`, `useSession.ts:225-290`):
  the first delta of a burst dispatches synchronously (the first word paints
  immediately), later deltas inside a 16 ms window are merged per
  `sessionId` and flushed by `setTimeout` — not `requestAnimationFrame`,
  which WebView2 throttles when the window is minimized, i.e. exactly the
  global-hotkey flow. Event ORDER is preserved because every other handler
  (`stt:partial`, `llm:done`, `session:error`, `hotkey:toggle`), the two
  supersede paths (Record over a streaming answer, Ask/Regenerate) and the
  effect cleanup flush the buffer first; a buffered token therefore always
  lands on the entry it belongs to before that entry is settled or retired.
- **The hotkey gate is an option, not a wrapped bridge.** While Settings is
  open the global hotkey must be ignored (the user may be typing the hotkey
  itself into the hotkey field); `useSession({ hotkeyEnabled })` reads that
  predicate at dispatch time (`App.tsx:34`, `useSession.ts:274-277`) instead
  of the old component effect that swapped a hand-copied bridge wrapper in
  and out (R4).

---

## 4. The audio path

`src-tauri/core/src/audio/` — from the render device's mix format to the
16 kHz mono i16 frames Deepgram is opened at.

**WASAPI loopback.** cpal 0.15's WASAPI backend enables loopback implicitly:
building an *input* stream on a *render* device sets
`AUDCLNT_STREAMFLAGS_LOOPBACK` — which is why `open_loopback_stream` asks for
the default **output** device and records from it
(`capture.rs:1-9`, `capture.rs:131-141`). The stream must be opened at the
device's own output mix format (`default_input_config` returns
`StreamTypeNotSupported` for render devices); the resampler absorbs whatever
that turns out to be (`capture.rs:142-152`). f32 is the shared-mode reality
on effectively every Windows machine, but i16/u16 are wired up rather than
trusted away (`capture.rs:173-201`).

**The `!Send` stream thread.** cpal's `Stream` is deliberately `!Send`, so it
is built, owned, and dropped on one dedicated `audio-loopback` thread; the
handle only signals that thread (`capture.rs:248-288`). Stopping means
dropping the stop sender — the thread's `recv` wakes, the stream drops,
WASAPI releases the device; `Option::take` makes stop and `Drop` idempotent
(`capture.rs:321-340`).

**Realtime-callback constraints.** The data callback runs on WASAPI's
realtime thread, where blocking or locking glitches every audio app on the
system. So the callback does exactly: format-convert, resample, frame, and a
`try_send` into a bounded channel (`FRAME_QUEUE_DEPTH = 32`, ~4 s) — if the
consumer stalls, the price is a dropped frame, never a stalled audio engine
(`capture.rs:27-30`, `capture.rs:93-105`). Scratch buffers are reused so the
steady state does no per-callback allocation (`capture.rs:74-79`). RMS and
the sink call happen on the `audio-frames` forwarder thread instead
(`capture.rs:312-319`).

**Resampler** (`resample.rs`) — streaming linear interpolation whose entire
design goal is: chunked output ≡ one-shot output, bit for bit.

- **Integer fractional position.** The read position is `idx` plus
  `frac_num`, an exact integer numerator over `SAMPLE_RATE`, advanced by the
  rational `src_rate/SAMPLE_RATE` per output sample
  (`resample.rs:53-59`, `resample.rs:139-142`). Floating-point accumulation
  loses a little on every buffer and drifts measurably out of sync with
  Deepgram's clock within minutes; integers cannot.
- **The carry sample.** The last mono sample of each push becomes index 0 of
  the next, so an interpolation window straddling a buffer boundary has both
  endpoints instead of snapping — an audible click, every buffer
  (`resample.rs:60-63`, `resample.rs:145-148`).
- **Speech-channel downmix.** Mono/stereo average all channels (conference
  apps often pan the far end mostly into one channel — taking channel 0
  alone would drop or halve the person being transcribed). On 5.1/7.1, only
  FL/FR/FC — the first three in WAVEFORMATEXTENSIBLE order, where dialog
  lives — are averaged: an equal average over six or eight channels divides
  the voice by 6–8 because LFE and surrounds contribute near-silence
  (`resample.rs:98-120`).
- **Conversion honesty.** i16→f32 divides by 32768 so a 16 kHz i16 stream
  round-trips bit-exactly; u16 subtracts the 32768 midpoint or the whole
  stream carries a DC offset that pegs the meter; f32→i16 clamps because
  Windows APOs can legally exceed ±1.0 and a wrapping cast turns a slightly
  hot sample into a full-scale polarity flip (`resample.rs:9-39`).

**FrameAccumulator** (`capture.rs:35-62`) cuts the 16 kHz mono stream into
exactly `FRAME_SAMPLES = 2048` samples (128 ms, §3;
`audio/mod.rs:12-14`). A short frame is never padded — padding injects a
click and shifts everything after it.

**The hand-off chain.** Realtime callback → bounded channel →
`audio-frames` thread (computes `rms`, `audio/mod.rs:37-46`) →
`ForwardingAudioSink.on_frame` → `SessionManager::push_audio`
(`state.rs:51-61`) → `DeepgramStream::send_audio`, which is one allocation
and an unbounded push, no lock shared with the driver
(`deepgram.rs:140-146`). Nothing in that chain can make the capture wait on
the network.

**The device watcher.** WASAPI keeps capturing from the *original* device
after the user switches defaults (plugs in a headset), so the app would
silently record silence for the rest of the call — and cpal surfaces no event
for a default switch. The `audio-loopback` thread therefore polls the default
device's identity every 2 s while parked and surfaces one honest error when
it moves (`capture.rs:218`, `capture.rs:260-286`). The decision is pure and
device-free: report only when a current default *exists and differs* — no
default at all means the device died, and the stream's own error callback
owns that report; duplicating it would double-toast
(`capture.rs:220-226`). The error callback itself is latched so an unplug
storm reports once (`capture.rs:154-171`).

---

## 5. The Deepgram driver

`src-tauri/core/src/stt/deepgram.rs` — one background task owns the socket
from dial to death (`run()`, `deepgram.rs:184`). A single owner means every
failure has exactly one classification site, so `on_error` cannot fire twice
and an abort cannot race a report. The handle communicates with the driver
only through flags plus a `Notify` (`Shared`, `deepgram.rs:123-130`) and a
`watch` channel for completion (`deepgram.rs:92-98`).

**"Established" semantics.** A successful WebSocket handshake proves nothing
about the key: Deepgram accepts the upgrade and then rejects bad keys by
*closing* (1008 / `DATA-xxxx`), usually with no Error frame at all. Only a
first frame from the server separates "connected" from "about to be
rejected", so a close before that first frame is classified as a connect
failure (with the close code and reason quoted — the only diagnostics a
reject-by-close ever offers), and a close after it as a mid-call drop
(`deepgram.rs:254-260`, `deepgram.rs:465-478`).

**Keepalive rules** (§6.1). Deepgram kills sockets ~10 s after the last audio
and silence during a call is normal, so a KeepAlive goes out every 8 s — but
the first tick is at +8 s, not immediately (a KeepAlive right after open says
nothing), and the tick re-checks the close/abort flags before sending,
because a KeepAlive written to a CLOSING socket errors and fabricates a "lost
connection" out of a stop that is succeeding (`deepgram.rs:44-45`,
`deepgram.rs:249-252`, `deepgram.rs:283-297`). The interval is dropped before
the drain starts, making a post-CloseStream KeepAlive structurally
impossible (`deepgram.rs:331-333`).

**The drain** (`deepgram.rs:334-398`), in order:

1. Flush audio still queued between the pump's last poll and the stop — those
   frames carry the final words of the question, and dropping them silently
   truncates the transcript's tail.
2. Send `CloseStream` — the server flushes held-back text (smart_format holds
   entities like numbers until it is sure of them) and closes from its side.
3. Read until the server's close. An `Error` frame *during* the drain is a
   failed flush and surfaces (`deepgram.rs:366-381`); the clean Close that
   follows must not launder it into a successful finalize.
4. Both the driver and `finalize()` independently enforce the 5 s cap
   (`deepgram.rs:395-398`, `deepgram.rs:148-163`).

`finalize()` is idempotent by construction: it sets a flag rather than
performing the close itself, so however many callers race, the driver crosses
into the drain exactly once and everyone joins the same `watch` completion
(`deepgram.rs:148-163`). A stream that has already died — or can no longer
open — resolves instantly with whatever the accumulator holds: a stop must not
wait out the cap for a tail that cannot arrive. A dial still in flight is
deliberately NOT in that category (§6.1): the pre-open channel holds the
user's entire captured question, so a stop waits the connect out (bounded by
the connect timeout and the finalize cap) and flushes it, rather than
discarding the recording into a false `no_speech`. Only `abort()` abandons a
dial immediately — the caller is tearing down on purpose.

**Two report gates**, each matching a phase's definition of "our own doing":

| Gate | Silenced by | Why |
|---|---|---|
| `report` (dial failures + every pump-phase death) | abort **only** | At every call site CloseStream has not been sent yet, so a death can never be our own close's doing. Gating on `close_requested` too would swallow a genuine death that raced a Stop — a truncated transcript with no error (the §5.5 failure) after open, and before open an abandoned question laundered into `no_speech` where an honest `stt_connect` names the real culprit |
| `finalize_death` (drain-phase deaths) | abort **only** | `close_requested` is by definition set during a drain; a death mid-flush means the tail was cut and must be reported |

Frame parsing is a separate, pure, total module
(`src-tauri/core/src/stt/frame.rs`): every input, including deliberately
hostile JSON, maps to a value — `is_final` must be *literally* `true`
(truthy imposters commit text Deepgram then revises, duplicating phrases),
and the accumulator keeps `committed + latest interim` with O(1) appends per
final rather than re-joining the whole recording per message
(`frame.rs:47-70`, `frame.rs:92-138`).

---

## 6. The LLM layer

`src-tauri/core/src/llm/` — two cloud providers and one loopback provider,
four shared mechanisms.

**The shared client** (`http.rs:87-92`). One process-wide `reqwest::Client`
behind a `OnceLock`, rustls compiled in, `pool_idle_timeout` 120 s (the
Record→Stop gap is a human-length pause; a short pool timeout reaps the
warmed connection in exactly the window it exists for — `http.rs:23-30`), and
deliberately **no** client-wide timeout: that would be a ceiling on the whole
request including the streamed body, cutting long answers off mid-sentence.
Per-stage timeouts live in the state machine instead (`http.rs:76-81`). Two
transport guards do bound what a warm cannot fix (RS-6, `http.rs:38-60`): a
3 s `connect_timeout` covers only the TCP+TLS connect — an origin
black-holing SYNs would otherwise sit in the ~21 s Windows OS timeout, twice
the first-token budget, before the retry policy got a look — and TCP
keepalive (20 s idle, 1 s interval, set explicitly because socket2 passes an
unset interval to Windows as 0) keeps NAT/VPN gateways from silently
dropping the idle warmed socket mid-recording. The singleton is enforced
(pointer-identity tested) rather than left as convention, because a
per-request client strands the warmed connection in a pool nobody reads —
pre-warm silently does nothing while looking fully implemented, visible only
as ~300 ms of extra latency in production (`http.rs:1-12`).

**Prewarm** (`warm.rs`). Fired on Record, on Stop before the finalize await,
and on the 120 s auto-stop — **not** on Ask (RS-1: the answer request is
microseconds away, so a warm only races it for the pool) and not at launch
(RS-4: the pooled socket is reaped long before a typical first question, and
it would be an unauthenticated request on every start). An unauthenticated
`GET <origin>/v1/models` with a 3 s per-request timeout — unauthenticated
because a 401 completes the TCP+TLS handshake just as well as a 200, and the
key has no business on a fire-and-forget request nobody reads
(`warm.rs:69-75`). The body is read to completion because dropping a
response mid-body makes reqwest close the connection instead of pooling it,
defeating the warm while looking like it worked (`warm.rs:14-17`,
`warm.rs:91-96`). Throttled to one warm per origin per 2 s, with denials
*not* refreshing the window (rapid-fire presses would otherwise starve
warming forever); origins are throttled independently so warming Anthropic
never suppresses warming Groq (`warm.rs:31-66`). Outside a tokio runtime,
`prewarm` is a silent no-op — a best-effort optimization must never take the
caller down (`warm.rs:81-84`). The local provider's `prewarm` is a no-op by
design (`local.rs:171-173`).

**The local provider** (`local.rs`, §6.5). `POST http://127.0.0.1:11434/api/chat`
to Ollama with `qwen3.5:2b`, `think: false` (reasoning text is never
emitted), `keep_alive: "10m"`, an 8 192-token context and 512 generated
tokens; the stream is NDJSON, one JSON object per line, decoded by a small
line splitter with a 256 KB frame cap rather than the SSE decoder. It refuses
before dialling when the joined system prompt plus the user message exceeds
`MAX_INPUT_BYTES` (7 000 UTF-8 bytes — bytes, not characters, so non-English
profiles fit the context the model actually has), with a message that names
every profile field that counts (`local.rs:16-24`, PROF-08). The count is
`prompt::request_input_bytes`, and the same function backs
`prompt::local_prompt_budget` (R4, ADR 016). That function builds the request
for an unsaved profile draft + style + question and returns used, limit,
remaining, fixed-overhead, profile and question bytes plus
`ok | tight | over`. The `local_prompt_budget` command serves the Settings
preview. The shell's `local_budget_check` refuses a local recording whose
active profile leaves no room for any question, and a typed ask whose actual
text will not fit, before anything starts and with the gate's own error. The
gate itself stays the authority for the real transcript. No byte is counted
in TypeScript: the Settings note and the UI's typed-Ask refusal only format
the core's figures. No key, no
prewarm, no Deepgram: `needs_cloud_keys()`/`uses_deepgram()` are both false,
and the shell pairs it with the Moonshine connector (`stt/local.rs`, a
loopback WebSocket on port 8765 whose service refuses browser `Origin`
headers). Deadlines are the local pair (`answer_limits()`).

**Retry-once** (`retry.rs`). One shared helper, one provider-supplied
predicate. The rules, each mapping to a way a naive retry makes things worse:

- Never retry an HTTP status — the server heard us and said no; an instant
  repeat burns first-token budget on a known failure.
- **Never retry after a delta reached the UI** — the concatenation guard. The
  UI appends deltas; a second attempt would stream a second full answer onto
  the first's partial text. The guard is the `Attempt` flag the provider's
  sink path flips on first delta and the helper reads after the attempt
  completes (`retry.rs:34-60`, `retry.rs:107-111`). This is also what makes
  the one aggressive case safe: a 200 whose stream dies before *any* delta is
  silently retried, because nothing is at risk of duplication.
- Never retry after an abort; cancellation is checked before any attempt,
  after any failure, and always wins as `aborted` — never as an HTTP-flavored
  error for work the user cancelled (`retry.rs:86-104`, `retry.rs:117-127`).
- The retried body must be byte-identical: the helper hands the closure only
  an attempt index; callers build the body once, outside
  (built once at the top of `stream_answer`, `anthropic.rs`). A rebuilt body can differ and silently miss the
  Anthropic prompt cache, turning the retry into the slowest request of the
  day (`retry.rs:21-25`).

Both providers define retryable by equality against two pinned message
constants — connect-failed and stream-dropped — rather than substring
matching (`anthropic.rs:33-37` and the `is_retryable` closure in
`stream_answer`; the Groq twin at
`groq.rs:33-35`, `groq.rs:225`).

**SSE decoding** (`sse.rs`). Shared by both providers; correct under hostile
chunking by construction:

- Lines are buffered as **raw bytes** and decoded only at a terminator —
  `\r`/`\n` cannot occur inside a multi-byte UTF-8 sequence, so a line
  boundary is always a safe decode point; a multi-byte character split across
  chunks reassembles structurally (`sse.rs:12-15`).
- `pending_cr` exists for the one split a naive splitter gets wrong: between
  `\r` and `\n`, where the trailing `\n` reads as a blank line — and a blank
  line in SSE means "dispatch", cutting an event in two (`sse.rs:7-11`,
  `sse.rs:62-72`).
- `finish()` flushes a final unterminated `data:` line — otherwise the
  answer's last words silently vanish, reading as the model trailing off
  (`sse.rs:85-95`).
- The decoder is framing only: `data: [DONE]` is framed like any event and
  the decoder never stops on it; the Groq provider decides it terminates
  the answer (R2).
- Memory is bounded while reading: a line over `MAX_LINE_BYTES` or an event
  over `MAX_EVENT_BYTES` (1 MiB each) fails `feed` at the crossing byte with
  `SseOverflow` (R2).

**Completion vs stop reason (R2, ADR 015).** `stream_answer` returns
`Answer { text, stop_reason }`. Each provider keeps a `StreamProgress`
(answer text, stop reason, `finished`, `saw_event`) and `conclude()`s once:
only the protocol terminator (`message_stop`, `[DONE]`, Ollama `done`) sets
`finished`, and reading stops there; `stop_reason`/`finish_reason`/
`done_reason` only feed `StopReason::from_provider` (`max_tokens`/`length` →
`TokenLimit`). No terminator → `ended_early` (`llm_http`, streamed text kept
on screen); terminator without usable text → `Answer::from_terminal` error;
non-JSON or text-less `text_delta` frames → `malformed_frame`; Groq `error`
frames → `llm_http` with the detail. Error bodies go through
`http::read_error_body`, capped at 16 KiB while streaming.

**The providers.** Each pins its model in exactly one constant:
`claude-haiku-4-5` (`anthropic.rs:28`) and `openai/gpt-oss-120b`
(`groq.rs:27` — Groq retires models on short notice, so the 404 message
names the constant as the likely fix, `groq.rs:265`). Groq sends
`reasoning_effort: "low"` and `include_reasoning: false` because gpt-oss is a
reasoning model and reasoning is the enemy of time-to-first-word; the
Qwen-only `reasoning_format` is deliberately absent and pinned so by test
(`groq.rs:66-67`, §6.3). Both providers check for an empty key before dialing
anything, map statuses to the closed error-code set naming the *actual*
status (§6.2/§6.3), treat a 200 with no events, or a finished stream with no
text, as an error rather than a silent blank answer (`StreamProgress::conclude`),
and check abort first at every
await via `biased` selects so a user's own cancel never surfaces as a scary
network error (`anthropic.rs`, the biased selects in `stream_once`).

**Prompt cache split** (`prompt.rs`, §7, ADR 007/014). `SystemPrompt` is
two strings: `cached_prefix` (role instructions + the whole ACTIVE call
profile — the large, stable-per-call part) and `style_suffix`
(`prompt.rs:141-159`). The prefix is assembled in a fixed order by
`build_system_prompt` (`prompt.rs:175-228`): role · a call-type line
(nothing for an interview) · resume · JD · the grounding note iff resume or
JD is present (focus alone does not trigger it) · focus · extra
instructions; the header set per call type comes from `sections_for`, a
straight-line `match` (`prompt.rs:165-173`). Interview keeps v3's exact
headers, so a migrated profile with empty focus/extra builds a
byte-identical prefix (pinned) and the upgrade cost nobody a cache write.
Anthropic gets the two strings as two system blocks with `cache_control` on
the **first only** — a breakpoint on the style block would invalidate the
profile cache on every style flip (`anthropic.rs`, `request_body`); Groq and
the local provider get `joined()` as one string. Byte-stability across calls
(no timestamps, no unordered joins, and a pure deterministic profile
normaliser upstream in the store) is a tested property, because caching is
a byte-prefix match and any nondeterminism silently costs a cache write per
call. Switching the active profile is the one *deliberate* prefix change — a
between-calls cache write. The prompt is built once per session from a
settings snapshot (`src-tauri/src/commands.rs:358-360`), so a switch
mid-recording applies to the next session, like a style change. The honesty
note lives in the code where the marker is set: Haiku's minimum cacheable
prefix is 4096 tokens, so a typical profile makes the marker a silent no-op;
`usage.cache_read_input_tokens` — parsed from `message_start`/`message_delta`
and exposed for logging — is the only thing that tells the truth about
whether it engaged (`anthropic.rs`, `last_cache_read_input_tokens` and the
usage parse in `apply_event`).

---

## 7. Trust boundaries

Three classes of input are untrusted, each with a distinct defense.

**The settings file** (`src-tauri/core/src/store/settings.rs`). User-writable
and survives upgrades, so:

- Every field validates and falls back *individually* — the question each arm
  answers is "what does the user lose if only this value is garbage?", and
  the answer must be "only this value" (`settings.rs:290-360`). An
  unparseable file loads as first-run defaults, never a crash. Profile
  fields are capped by characters (not bytes — a byte cut can split a UTF-8
  sequence into invalid data), corrupt geometry drops as a unit, and an
  explicit object check defeats serde's willingness to read a struct out of
  a JSON array.
- The same rule holds *inside* a call profile (`settings.rs:364-380`): a bad
  `callType` reads as interview, a bad string as `""`; only a non-object
  entry in `profiles` is dropped, because there is nothing in it to save. A
  `profiles` value that is not an array at all — a v3 flat file, a missing
  key, or garbage — loads as ONE Default interview profile carrying the
  top-level `resume`/`jobDescription`, with keys, hotkey and bounds
  untouched; that legacy shape is read once and never written back
  (`settings.rs:298-318`, `settings.rs:422-433`). Whatever reaches memory
  then passes `normalize_profiles`, the single pure, deterministic writer of
  the profile invariant (`settings.rs:186-253`, ADR 014).
- A file that is not settings is preserved before it is replaced, and a file
  that cannot be read is never replaced (ADR 016). `read_settings` sorts the
  load into four cases:
  - **missing**: first run;
  - **parsed**: loaded with per-field fallback;
  - **invalid**: bytes were read but hold no JSON object. The file is renamed
    to `settings.json.corrupt-<unix-seconds>[-n]`, never over an existing
    backup, and `storageWarning` names the copy;
  - **unreadable**: permission, sharing or transient error.

  If the rename fails, or the file is unreadable, the store runs on defaults
  with a `write_block`: every `apply_patch` returns the reason, and
  `save_window_bounds` does nothing.
- Revisions (R5, ADR 016). The store keeps `revision` beside the settings
  behind its one `RwLock`. `apply_patch` checks `expectedRevision` (when
  present), writes, and bumps the revision in one critical section, so a stale
  full form fails with `settings_conflict` and a failed write advances
  nothing. Geometry saves never bump it.
- Writes are atomic: full write + fsync to `settings.json.tmp`, then rename;
  the in-memory cache updates only after the write lands, so a failed write
  leaves memory matching disk (`settings.rs:152-170`). A truncated
  settings.json would read as "defaults" and silently destroy every setting
  including the keys.
- Secrets (`secrets.rs`) are DPAPI-encrypted as `enc:<base64>`, with a
  *marked* `plain:<base64>` fallback when the keystore is unavailable —
  honestly labeled rather than pretending. Decoding dispatches on the
  **stored** prefix, never on current keystore availability, and anything
  undecodable (file copied from another machine, unknown prefix, mangled
  base64) reads as *unset* — failing closed beats handing ciphertext to a
  provider as if it were a key (`secrets.rs:1-49`).
- Keys are write-only across the UI boundary: `SettingsView` carries
  presence booleans only (`store/mod.rs:234-250`); a patch that omits a key
  leaves it, an empty-after-trim value clears it (`settings.rs:76-130`).
- Saved window bounds go through a pure sanitizer: size clamped up to the
  minimum *first*, position kept only if ≥40 px lands on a live display on
  both axes at the clamped size, corrupt values dropped as a unit
  (`store/bounds.rs:101-137`, applied in `src-tauri/src/window.rs:42-58`).
  The size is re-applied only when it came from the file (`from_saved`) —
  the first-run fallback is the builder's *logical* 460×700, which tao has
  already scaled, and re-applying it as physical pixels shrank every hi-DPI
  window (RS-7). A position the sanitizer cannot prove is replaced by the
  dock target (top-centre of the current display, `bounds.rs:147-195`,
  ADR 013), and by the centre only if even that fails.

**Model output.** Rendered by the in-repo markdown subset in `src/markdown/`
(§10): every string reaches the DOM as a text node — no
`dangerouslySetInnerHTML`, no attribute ever derived from model text — and
links are deliberately not parsed: `[text](url)` stays literal, so there is
no href to sanitize and no `javascript:` to smuggle. The streaming renderer
is held to "every prefix renders without throwing, and the final DOM is
byte-identical to a one-shot render" by `src/markdown/streaming.test.tsx`;
the XSS suite is `src/markdown/xss.test.tsx`.

**Wire bytes.**

- Deepgram frames: `parse_frame` is pure and total — malformed JSON,
  wrong-typed fields, and 5000-deep nesting (serde_json's recursion limit)
  all map to `Ignored`, never a panic mid-call (`frame.rs:1-45`).
- SSE: hostile chunking handled structurally (section 6); invalid UTF-8
  inside a line is lossily replaced rather than dropping the delta
  (`sse.rs:97-101`).
- Error bodies are truncated before reaching the UI
  (`anthropic.rs`, `map_status`).

**Process-level boundaries** (`src-tauri/src/`):

- The webview never navigates: the window builder's `on_navigation` hook
  (`window::handle_navigation`, `window.rs:291-301`) allows only the bundled
  origins, bounces https to the default browser, silently drops everything
  else — file:, javascript:, http:. That hook is the *only* way out of the
  app: the former `open_external` command was dead code and is gone. An
  external open is https-only, rejected if it contains whitespace, control
  or quote characters — the characters that make one argument stop being one
  argument (`is_safe_external_url`, `window.rs:277-289`) — and handed to
  `explorer.exe` as a single argv entry, never through cmd.exe
  (`open_in_browser`, `window.rs:303-316`).
- Capture exclusion is requested with `set_content_protected(true)` and then
  VERIFIED: `window::verify_capture_exclusion` reads the affinity back with
  `GetWindowDisplayAffinity` and requires `WDA_EXCLUDEFROMCAPTURE` (0x11,
  Windows 10 version 2004+); anything else fails setup with a message naming
  that requirement, which the panic hook writes to crash.log. The read-back
  exists because tao discards `SetWindowDisplayAffinity`'s result, so the
  request alone can never report a refusal. The pure value mapping is
  `capture_exclusion_verdict` (unit-tested). Whether a given conferencing app
  honours the exclusion depends on its capture method and is only known from
  recorded tests. The window is built hidden and shown only after geometry,
  docking and the verified exclusion (`lib.rs`, `setup`).
- The crash log carries only timestamp, panic location, and
  developer-authored panic text — never keys, profile text, transcripts, or
  answers (`logging.rs:1-7`). Panic unwinding is kept (not `panic="abort"`)
  so a panicking answer pipeline kills one tokio task, not the process
  (`logging.rs:16-20`). The startup stage timings (RS-9) are a
  `#[cfg(debug_assertions)]` `eprintln!` of durations only, a no-op in
  release, and never touch crash.log (`lib.rs:28-34`).
- A crashed renderer reloads at most once per 10 s so a boot-crash does not
  flicker forever (`window.rs:350` onward).
- The local voice services are separate processes the shell only probes and
  spawns (`local_voice.rs`): the install location comes from
  `%LOCALAPPDATA%\AI Call Assistant\local-voice.json`, parsed by a pure
  function that strips PowerShell's UTF-8 BOM and refuses a relative
  `dataDir` — every executable is resolved under it, and "relative to the
  current directory" is not a place to run binaries from
  (`local_voice.rs:109-127`). Their stderr goes to `ollama.log` /
  `speech.log` in that folder; nothing of ours reads it back.

---

## 8. Threading model

Everything that exists, at rest and during a recording, and what each is
allowed to block on.

**At rest** (idle, window open):

| Thread / task | Owns | May block on |
|---|---|---|
| Main thread | Tauri event loop, window, webview, hotkey callbacks, the sync commands (`get_settings`, `cancel_session`, `hotkey_status`, `dock_to_camera`) | OS message pump only. Command handlers take short mutexes (`state.rs`, poisoning-recovering `lock()`); no command does file I/O here any more. Hotkey re-registration after a settings change hops *back* here via `run_on_main_thread` + a oneshot that the effect step waits on from the blocking pool (`apply_hotkey_on_main_thread` in `commands.rs`): RegisterHotKey binds a hot key to the calling thread's window, and getting that wrong reports every changed hotkey as `registered: false` — exactly the dead key `hotkey_status` exists to prevent |
| Tauri async runtime (tokio workers) | `start_session`/`stop_session`/`ask`/`set_settings`/`local_voice_*` command futures, debounced bounds-save tasks (`window.rs:216` onward) | Session machine awaits; never a `std::Mutex` across an await |
| tokio blocking pool | `set_settings`'s lock + `apply_patch` (the fsync'd write, DPAPI re-encryption of every stored key, re-serialization of every profile — 5–50 ms that used to freeze paint per style-chip click, RS-3); then, after every committed save, the effect step `reconcile_os_state` (ADR 016): `EffectReconciler` serializes runs under its own mutex, reads the CURRENT committed settings inside it (a short settings lock, released before any OS call), and applies only what differs from the last applied OS state: the hotkey via the main-thread hop (a blocking wait, which is why this runs here and never on the event loop), always-on-top, and the dock on a transition into `camera`. `start_session`'s ~100 ms WASAPI open (RS-2) | Disk, the device, and the effect step; the settings mutex is taken and released entirely inside each task, and never held across an OS call |
| WebView2's own process/threads | Rendering | Not ours; its crashes come back via `ProcessFailed` → `recover_webview` |

**During a recording**, added:

| Thread / task | Created by | Work | May block on |
|---|---|---|---|
| WASAPI realtime callback thread | cpal, inside `open_loopback_stream` | convert → resample → frame → `try_send` | **Nothing.** No locks, no allocation in steady state, `try_send` never blocks; a full queue drops the frame (`capture.rs:97-105`) |
| `audio-loopback` thread | `capture.rs:249` | Owns the `!Send` cpal `Stream`; parks on `stop_rx` with a 2 s timeout as the device watcher heartbeat | `recv_timeout`; a ~ms COM enumeration per poll — not realtime (`capture.rs:260-286`) |
| `audio-frames` thread | `capture.rs:244-245` | Blocking `recv` from the bounded channel; computes RMS; calls `push_audio` | The channel; the machine's slot mutex briefly (`machine.rs` (`SessionManager::push_audio`)) — held only to clone Arcs, `send_audio` itself is lock-free toward the driver |
| `drive_recording` task | `machine.rs` (`SessionManager::start`) | connect → record loop → finalize → answer; every await raced against cancel; detached, so its lifetime is bounded by the gate and slot, not the caller | STT connect (5 s cap), the stop oneshot, the STT event channel, finalize (5 s), the LLM future (10 s / 60 s cloud, 90 s / 300 s local) |
| Deepgram driver task | `deepgram.rs:92-98` | Owns the socket: dial, pre-open flush, pump, keepalive, drain | The socket and the audio channel; every exit path signals the `watch`, so a `finalize()` can never hang on a dead driver |
| LLM stream future | inside `run_answer` | POST + SSE decode; deltas pushed inline from the poll loop | The HTTP response stream, raced against cancel with `biased` selects (`anthropic.rs`, `stream_once`) |
| Prewarm task(s) | `warm.rs:87-99` | One GET, body drained | 3 s request timeout; fire-and-forget, nothing awaits it. Cloud providers only — Record, Stop and auto-stop, never Ask (RS-1) |
| Moonshine driver task (local mode) | `stt/local.rs`, inside `connect` | Owns the loopback WebSocket; PCM from a bounded channel | The socket; the finalize cap; every exit signals its `watch` |

Cross-cutting rules the table encodes:

- **The realtime thread never meets a lock.** The only mutation it performs
  outside its own state is a bounded `try_send`.
- **The capture never waits on the network.** The chain from callback to
  Deepgram socket is two channels; backpressure turns into dropped frames at
  the bounded stage and buffered bytes at the unbounded stage (bounded in
  practice by the 120 s recording cap; the pre-open portion by the explicit
  15 s cap).
- **The shell holds no lock across a blocking call.** The superseded capture
  is stopped under a short lock released before the ~100 ms WASAPI open —
  holding it across the open (or any event emission) was a real deadlock:
  `TauriEventSink::emit` re-enters `release_capture_if` on terminal events,
  which takes the same mutex on the same thread (`commands.rs:183-189`,
  `events.rs:37-52`).
- **Nothing blocks the event loop.** The settings write and the device open
  both moved to the blocking pool; window operations (`set_always_on_top`,
  `dock_to_camera` from the settings hop) dispatch through tauri's event-loop
  proxy and are safe from any thread — except hotkey registration, which is
  the one call that deliberately hops to the main thread (see the table).
- **The frontend paints at most once per 16 ms window per streaming
  answer** — the delta coalescer in `useSession` (§3) — with the first token
  of a burst exempt so the first word never waits.
- **Detached tasks cannot outlive their relevance.** The driver task's
  events die with the gate; the Deepgram task dies when its handle drops
  (`Drop` = abort, `deepgram.rs:175-181`); the slot's id-keyed release means
  none of them can ever confuse the next session.

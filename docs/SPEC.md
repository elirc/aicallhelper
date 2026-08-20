# AI Call Assistant v3 — product specification

This is the specification the app was built against. Code comments cite its
section numbers (§5.5, §6.4, …). Where this document pins an exact string,
timeout, or ordering rule, treat it as load-bearing product behavior, not a
suggestion.

---

## 1. What the product is

A push-to-record **interview copilot** for Windows. During a live call
(Zoom/Teams/Meet/phone-through-speakers), the user presses **Record** while
the other person is asking a question. The app:

1. captures **system audio** (what the other person is saying — loopback, not
   the microphone),
2. streams it to a speech-to-text service so a **live transcript renders
   while they are still speaking**,
3. when the user presses **Stop & Answer**, finalizes the transcript and
   streams an **AI-suggested answer** (grounded in the user's saved resume and
   the job description) into the answer panel,
4. reports the **measured stop-to-first-word latency** in the UI.

The single product promise is **stop-to-first-word latency of roughly one
second**. Every architectural decision below exists to protect that number or
to make being fast safe. The app is also **invisible to screen sharing**
(content protection), because the user is on a call.

Secondary flows: a typed **Ask** box (same answer pipeline, no audio), a
**Regenerate** button (re-ask the viewed question), a 6-entry **history**, an
**answer style** toggle (brief / balanced / detailed), and a **global hotkey**
that toggles record/stop from any app.

This is a personal productivity tool: single user, their own API keys, no
telemetry, no server component of ours. Providers: **Deepgram** (STT) and, for
answers, **Anthropic Claude** (default) or **Groq** (user-selectable).

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
- **HTTP**: `reqwest` (rustls, streaming). ONE shared `Client` for all LLM
  traffic — its connection pool is what makes pre-warming work (§6.4).
  Configure a generous `pool_idle_timeout` (≥ 90 s).
- **Secrets**: Windows DPAPI (`CryptProtectData`/`CryptUnprotectData` via the
  `windows` crate), stored base64 in the settings file (§8).
- **Global shortcut**: `tauri-plugin-global-shortcut`.
- **Single instance**: `tauri-plugin-single-instance` (second launch focuses
  the running window).
- **Content protection**: `window.set_content_protected(true)`.
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
    src/stt/          Deepgram WS client + frame parser (pure parse fns)
    src/llm/          anthropic.rs, groq.rs, retry.rs, warm.rs, prompt.rs, sse.rs
    src/session/      the session state machine (§5) — trait-injected deps
    src/store/        settings + DPAPI secrets + window-bounds persistence
  src/                Tauri shell: commands, events, window, hotkey, crash log
src/                  React frontend (views, history, markdown renderer, format helpers)
docs/TESTING.md       every test documented: what it verifies and why it exists
```

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
STT finalize **5 s** · LLM first token **10 s** · LLM total **60 s** ·
recording hard cap **120 s** (auto-stop, then answer normally).

**Metrics** — measured in the core, from the moment stop was requested:

- `sttFinalizeMs` — stop → final transcript in hand. Exactly `0` for typed
  (Ask) questions: there was no STT stage, and billing one would be a lie.
- `firstTokenMs` — stop → first answer token. If a provider returns a full
  answer without ever streaming a delta, report `firstTokenMs = totalMs` —
  never 0, because 0 renders as "instant" and lies about the one number this
  app is judged on.
- `totalMs` — stop → answer complete.

**Prompt caching** (Anthropic): the system prompt is TWO blocks with the cache
breakpoint after the resume+JD block; the style policy sits AFTER it (§7).
Caching is a byte-prefix match, so prompts must be byte-stable across calls —
no timestamps, no unordered joins — and a style flip must never invalidate the
cached resume+JD. Honesty: Haiku's minimum cacheable prefix is 4096 tokens, so
a typical 1–2 K-token profile makes the marker a silent no-op; it starts
paying at roughly 16 K+ characters of profile (writes 1.25×, reads 0.1×,
5-minute TTL). `usage.cache_read_input_tokens` in the response tells the truth
about whether it engaged.

---

## 4. Frontend ↔ core interface

Commands (Tauri `invoke`), all returning a `Result`-shaped envelope
`{ ok: true, value } | { ok: false, error: { code, message } }` rather than
throwing across the boundary (validation errors included):

- `get_settings() -> SettingsView`
- `set_settings(patch) -> SettingsView` — validated patch (§8); returns the
  fresh view, which the UI re-renders from (main is the source of truth).
- `start_session() -> sessionId` — starts capture + STT; supersedes any
  active session.
- `stop_session(sessionId) -> ok/err` — MUST return "not taken" (an error
  Result) when the session is unknown/already ended/already stopping: every
  other outcome arrives as an event, so a silently ignored stop leaves the UI
  in "Finalizing…" forever. This return is the only way it learns.
- `ask(text) -> sessionId` — typed question; trimmed, 1..8000 chars.
- `cancel_session(sessionId)` — fire-and-forget; invalid ids do nothing
  (never an error — the caller doesn't await it meaningfully).

Events (Tauri events). The five session events are every one tagged
`{ sessionId }` — the frontend drops any event whose id is not the session it
is currently tracking; `hotkey:toggle` is the one untagged shell event (it has
no session):

- `stt:partial { sessionId, text, isFinal }` — full transcript so far (not a
  delta); `isFinal` true when Deepgram committed the latest segment.
- `llm:delta { sessionId, delta }` — answer text delta.
- `llm:done { sessionId, transcript, answer, metrics }`
- `session:error { sessionId, error: { code, message } }`
- `audio:level { sessionId, rms }` — for the meter (~8/s; may be coalesced).
- `hotkey:toggle` — global shortcut fired.

Error codes (closed set — the UI keys behavior off these):
`no_stt_key · no_llm_key · stt_connect · stt_error · stt_timeout · no_speech ·
llm_auth · llm_http · llm_rate_limit · llm_first_token_timeout · llm_timeout ·
aborted · internal`. Messages are user-facing and actionable ("… Open
Settings (gear icon) and add it."), never raw exception text when avoidable.
`aborted` is special: the UI never shows it — it means the user superseded or
cancelled, which must be silent.

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
   not kill a live session), resolves with the id before any event fires,
   then emits the trimmed question as one `stt:partial` with `isFinal: true`
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

The state machine is a plain module with injected dependencies (an
`SttStream` trait, an `LlmProvider` trait, an event sink) — `cargo test`
exercises every rule above with fakes, no network.

---

## 6. Provider wire protocols

### 6.1 Deepgram (STT)

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
  `choices[0].delta.content`; `data: [DONE]` is a sentinel to skip (not a
  terminator — bytes after it in the same chunk still count). The SSE parser
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

### 6.4 Retry policy and pre-warm (both providers)

- **Retry exactly once**, and ONLY when the initial request failed at the
  connection level BEFORE any delta reached the UI. Never retry an HTTP error
  status (the server heard us and said no — an instant retry burns the
  first-token budget), never after a delta (the UI appends deltas; a second
  attempt would concatenate two answers), never after an abort. The retried
  request must be byte-identical (build the body once). Extract this policy
  into one shared helper both providers use, with the "is this retryable"
  predicate supplied per-provider.
- **Pre-warm**: on Record press, on Ask, and on Stop press (before awaiting
  the stop), fire an unauthenticated fire-and-forget `GET <origin>/v1/models`
  through the SHARED reqwest client with a 3 s timeout, reading the body to
  completion so the connection returns to the pool. Throttle to one warm per
  origin per 2 s. A failed warm costs nothing and must never throw or log
  loudly.

---

## 7. The prompt (exact strings — product behavior)

Build the system prompt as `cachedPrefix` + `styleSuffix` (two blocks for
Anthropic, joined with `\n\n` into one string for Groq). Byte-stable across
calls for identical inputs.

`cachedPrefix` = role instructions, then optional sections:

Role instructions (verbatim):

> You are a real-time call assistant helping the user answer questions asked
> of them during a live interview or call. You are given a transcript of what
> the other person just said. Reply with the answer the user should say,
> written in first person, in natural spoken English. Do not add meta
> commentary, greetings, or quotation marks — output only the answer itself.
> If the transcript contains no real question, briefly suggest what the user
> could say next.

If the trimmed resume is non-empty, append
`\n\n--- THE USER'S RESUME ---\n` + resume (trimmed at the edges only —
interior formatting survives verbatim). If the trimmed job description is
non-empty, append `\n\n--- THE JOB THEY ARE INTERVIEWING FOR ---\n` + jd.
If either was present, append:

> \n\nGround every answer in the resume and target role above. Never invent
> experience the resume does not support.

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
cached prefix stays stable.

---

## 8. Settings, secrets, persistence

One JSON settings file in the app's data directory. Fields: `resume`,
`jobDescription` (strings, ≤200 000 chars each, stored verbatim — NOT
trimmed; profile formatting belongs to the user), `alwaysOnTop` (bool,
default true), `llmProvider` (`"anthropic"` default | `"groq"`),
`answerStyle` (`"brief"|"balanced"|"detailed"`, default balanced), `hotkey`
(accelerator string, ≤100 chars, default Ctrl+Shift+Space; empty string means
"shortcut disabled" and must NOT spring back to the default), `secrets`
(encrypted API keys), `windowBounds` (optional `{x,y,width,height}`).

Rules, each one a lesson:

- **Validation with per-field fallback**: the file is user-writable and
  survives upgrades — untrusted input. Every field falls back to its default
  individually; one corrupt value must never cost the user their resume or
  keys. An unparseable/non-object file loads as first-run defaults, never a
  crash.
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
  close. Saving geometry must NEVER throw during shutdown — swallow write
  failures (it is cosmetic data). On launch, restore through a pure
  sanitizer: clamp size up to the window minimum, round to integers, and
  keep the position only if at least **40 px** of the window (judged at its
  CLAMPED size) lands on some display's work area on BOTH axes — otherwise
  drop the position and let the OS center it. Negative coordinates are valid
  (displays left of primary). Corrupt bounds are dropped as a unit while the
  rest of the file survives.

---

## 9. Window, UX, and exact behavior

**Window**: default 460×700, minimum 380×520, dark background `#16181d`,
always-on-top per setting, menu bar hidden, content-protected (invisible to
screen capture — this is a moat feature). Single instance: a second launch
focuses/restores the first. External links open in the default browser
(https only); the app never navigates.

**Main view** (top to bottom): header with a status dot + app title + gear
(Settings) button · status line · big Record button (label cycles
Record / Starting… / Stop & Answer / Record) with the formatted hotkey shown
as a chip when registered · a notice line when the hotkey is TAKEN by another
app ("<hotkey> is already taken by another app, so the shortcut is off —
record from this window, or pick a different one in Settings.") — register
honestly: if registration fails, say so rather than leaving a dead key ·
level meter + mm:ss timer while recording · Ask form (text input +
Ask button) · style chips (Brief/Balanced/Detailed, aria-pressed reflects the
PERSISTED style returned by the save, not the clicked chip) · "Question
heard" panel (live transcript; placeholder "The live transcript will appear
here while you record."; "Listening…" while recording with nothing heard yet;
a "live" tag while recording) · "Suggested answer" panel (markdown-rendered;
placeholder "Your AI-suggested answer will stream here."; "generating…" tag
and `aria-busy` while answering; latency chip "X.Xs to first word" with a
hover title breaking down "First word N ms after Stop · transcript finalized
N ms · full answer X.X s"; Regenerate and Copy buttons in the panel title) ·
error box (`role="alert"`) · history bar (hidden until 2+ entries):
Clear button, prev/next arrows, "n/m" label.

**State machine** (frontend): `idle → starting → recording → finalizing →
answering → idle`. Status lines per state: "Ready — press Record while the
other person is speaking" (idle; append "or <hotkey>" when registered) ·
"Opening the microphone feed…" (starting) · "Recording call audio…" ·
"Finalizing transcript…" · "Generating answer…" · "Done — press Record for
the next question" after completion · at the 120 s cap: "Reached the 120s
limit — answering now". First run with a missing key for the SELECTED
provider (or missing Deepgram key): "First run: open Settings (gear icon) and
add your API keys".

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
- Answer panel autoscroll: stick to the bottom only if already at the bottom
  (within ~28 px) — a user who scrolled up to re-read must not be yanked
  down by each token. Switching history entries resets scroll to top.
- Rendering is coalesced to one paint per animation frame while streaming.

**Accessibility**: `aria-live="polite"` on the answer panel with `aria-busy`
during streaming (announced on completion, not per token) · `role="alert"`
errors · `role="status"` status line · focus moves into Settings heading on
open and back to the gear on close · Escape closes Settings · visible focus
rings · `prefers-reduced-motion` respected · AA contrast on the dark theme.

**Settings view** (replaces main view, not a dialog): Deepgram key ·
provider select ("Claude Haiku 4.5 (recommended)" / "Groq GPT-OSS 120B
(fastest)") · Anthropic key · Groq key (labelled "only for the Groq preset")
· answer style select · hotkey text field with help text and the default as
placeholder · resume textarea · job description textarea · always-on-top
checkbox · a note that keys are stored encrypted and never shown again, and
that the window is hidden from screen sharing · Save + Back buttons ·
"Saved ✓" note for ~1.5 s. Save sends the whole form BUT only the key fields
the user actually typed into. Failures report in a settings-local error box
(the main one is hidden behind this view).

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
- Log nothing sensitive: never keys, never resume text, never transcripts.

---

## 12. Testing (a first-class deliverable)

- **No network, no audio devices, no live providers in any test.** Core:
  trait-injected fakes; frontend: Vitest + Testing Library against the real
  components with a mocked command/event bridge.
- Cover, at minimum: every invariant in §5 (including the races) · both
  providers' error-mapping matrices and the retry policy matrix · the SSE
  parser under hostile chunking · Deepgram frame parsing under hostile
  input · prompt byte-stability and the cache-split rule · settings
  validation/fallback/atomicity/secret semantics · bounds sanitization
  geometry cases · the markdown streaming-vs-batch invariant and the XSS
  suite · the full frontend flows driven through the mocked bridge with
  assertions on real DOM state.
- **Document every test** in `docs/TESTING.md`: one bullet per test — what it
  verifies and *why it exists* (the failure mode it guards).

---

## 13. Definition of done

1. `cargo test` and the frontend test suite fully green; Rust `clippy` clean;
   TypeScript strict typecheck clean.
2. `tauri build` produces a working Windows installer; `tauri dev` runs.
3. Manual QA script passes (in the README).
4. README documents setup, the latency architecture, the caching honesty
   note, the Groq model-pinning note, and an approximate cost note.
5. No TODOs on the critical path; comments explain WHY (invariants, races,
   failure modes), not what.

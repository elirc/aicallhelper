# AI Call Assistant v3

A push-to-record **interview copilot** for Windows. During a live call, you press
**Record** while the other person is asking a question. The app captures the
**system audio** (what *they* are saying — loopback, not your microphone),
streams it to speech-to-text so a live transcript renders while they are still
talking, and when you press **Stop & Answer** it streams an AI-suggested answer —
grounded in your saved resume and the target job description — into the answer
panel.

The single product promise is **stop-to-first-word latency of roughly one
second**, and the app reports the measured number after every answer. It is also
**invisible to screen sharing**, because you are on a call.

Single user, your own API keys, no telemetry, no server of ours.

---

## Setup

1. **Install the prerequisites** (Windows 10/11):
   - [Rust](https://rustup.rs) (stable)
   - Visual Studio Build Tools with the **Desktop development with C++**
     workload (supplies the MSVC linker and the Windows SDK)
   - [Node.js](https://nodejs.org) 20+
   - WebView2 runtime (already present on current Windows 11)

2. **Install dependencies and run:**
   ```sh
   npm install
   npm run tauri dev
   ```

3. **Add your API keys** — launch the app, click the gear icon, and paste:
   - **Deepgram** (required, speech-to-text) — <https://console.deepgram.com>
   - **Anthropic** (default answer provider) — <https://platform.claude.com>
   - **Groq** (only for the Groq preset) — <https://console.groq.com>

   Keys are encrypted at rest with Windows DPAPI and are never shown again and
   never sent to the frontend — the UI only ever learns *whether* a key is
   stored.

4. **Paste your resume and the job description** into Settings. Every answer is
   grounded in them, and the model is instructed never to invent experience the
   resume does not support.

### Build an installer

```sh
npm run tauri build
```

Produces an NSIS installer under `src-tauri/target/release/bundle/`.

---

## The latency architecture

Everything below exists to protect the stop-to-first-word number, or to make
being fast safe.

```
Record pressed    STT WebSocket connect and audio capture start IN PARALLEL.
                  Frames captured before the socket opens are buffered in order
                  (~15 s cap, drop oldest) and flushed the instant it opens.
                  The LLM origin is pre-warmed.

While recording   loopback audio -> 16 kHz mono i16 -> ~128 ms frames (2048
                  samples) -> Deepgram. Interim transcripts render live;
                  is_final marks committed text. RMS per frame drives the level
                  meter. A KeepAlive every 8 s stops Deepgram hanging up during
                  the silences that are normal on a call.

Stop pressed      THE LATENCY CLOCK STARTS HERE. CloseStream is sent; Deepgram
                  flushes its held-back tail. The LLM origin is pre-warmed
                  again, so the TLS handshake overlaps the finalize instead of
                  landing inside the latency window. The answer request fires
                  the instant the transcript is final.

Answer            the first token streams into the panel; on completion the
                  measured stop-to-first-word lands in the panel header.
```

**Timeouts**, all enforced in the Rust core: STT finalize 5 s · LLM first token
10 s · LLM total 60 s · recording hard cap 120 s (auto-stops, then answers
normally).

**Pre-warming** is why a single shared `reqwest::Client` is used for all LLM
traffic: its connection pool is the thing being warmed. A per-request client
would throw the warmed connection away and the feature would silently do
nothing.

**Metrics** are measured in the core from the moment Stop was requested.
`sttFinalizeMs` is exactly `0` for typed questions — there was no STT stage, and
billing one would be a lie. If a provider ever returns a complete answer without
streaming a delta, `firstTokenMs` reports `totalMs` rather than `0`, because `0`
renders as "instant" and would lie about the one number this app is judged on.

### An honest note on prompt caching

The Anthropic system prompt is sent as two blocks with the cache breakpoint
after the resume + job-description block, and the answer-style policy placed
*after* it — so flipping answer style is latency-free and never invalidates the
cached profile. Caching is a byte-prefix match, so the prompt is built to be
byte-stable across calls (no timestamps, no unordered joins), and that is
enforced by test.

**But be honest about when it actually pays:** Haiku's minimum cacheable prefix
is 4096 tokens, so a typical 1–2 K-token profile makes the cache marker a silent
no-op. It starts paying at roughly 16 K+ characters of profile (writes cost
1.25×, reads 0.1×, 5-minute TTL). `usage.cache_read_input_tokens` in the
response is the only thing that tells the truth about whether it engaged.

### A note on the pinned Groq model

The Groq model is pinned in exactly one constant (`openai/gpt-oss-120b`). **Groq
retires models on short notice** — if Groq answers start failing with a 404, that
is almost certainly the cause, and the fix is to update that constant. The error
message says so, so you do not have to remember this.

`reasoning_effort: "low"` and `include_reasoning: false` are sent because gpt-oss
is a reasoning model and reasoning is the enemy of time-to-first-word. These are
the supported knobs for this family; `reasoning_format` is a Qwen-family knob and
is deliberately not sent.

### Approximate cost

The LLM side is roughly **$0.002–0.003 per answer** at Haiku pricing. Deepgram's
per-minute streaming rate dominates the total — you pay for wall-clock recording
time, not per question, so the cost of a session is mostly a function of how long
you hold the Record button.

---

## Architecture

```
src-tauri/
  core/               `app-core` — the entire pipeline, knows nothing about Tauri
    src/audio/        WASAPI loopback capture -> 16 kHz mono i16 frames + RMS
    src/stt/          Deepgram WebSocket client + pure frame parsing
    src/llm/          anthropic, groq, shared retry policy, pre-warm, prompt, SSE
    src/session/      the session state machine (trait-injected dependencies)
    src/store/        settings, DPAPI secrets, window-bounds sanitising
  src/                Tauri shell: commands, events, window, hotkey, crash log
src/                  React frontend (thin view over the core)
  markdown/           in-repo streaming markdown renderer (no library, no sanitizer)
docs/TESTING.md       every test, and the failure mode it guards
```

The core is a separate crate from the Tauri shell on purpose: `cargo test -p
app-core` exercises every invariant with fakes, without a webview, a network, or
an audio device.

---

## Manual QA script

Run this end to end before shipping a build.

1. **First run** — launch with no keys. The status line reads
   *"First run: open Settings (gear icon) and add your API keys"*.
2. **Add keys** — gear icon → paste the Deepgram and Anthropic keys → Save →
   *"Saved ✓"*. Reopen Settings: the key fields are empty with the placeholder
   *"saved — type to replace"*.
3. **Record system audio** — start a video call (or play any speech through your
   speakers) → press **Record**. The transcript appears **while the audio is
   still playing**, not after.
4. **Stop** — press **Stop & Answer**. The answer streams in, and the latency
   chip reads roughly *"1.Xs to first word"*. Hover it for the breakdown.
5. **Ask** — type a question into the Ask box and submit. The answer streams;
   the latency chip shows a `0 ms` transcript-finalize stage in its tooltip.
6. **Regenerate** — press Regenerate. The same question is re-asked as a *new*
   history entry.
7. **Styles** — flip Brief / Balanced / Detailed. Answer length changes; latency
   does not (the cached prefix is untouched).
8. **History** — after 2+ entries the history bar appears. Navigate with the
   arrows, confirm `n/m` tracks, then Clear (only enabled when idle) and confirm
   focus lands on the Record button.
9. **Hotkey from another app** — focus a different window and press the hotkey.
   Recording starts. Press it again; it stops and answers.
10. **Hotkey taken** — set the hotkey to something another app already owns
    (e.g. one your screen-recorder uses). The notice appears and the chip does
    not, rather than leaving a dead key.
11. **Screen share** — share your screen in a call. The window is **invisible**
    in the shared view while remaining visible to you.
12. **Relaunch** — move and resize the window, quit, relaunch. Position and size
    are restored.
13. **Second launch** — start the app again while it is running. The existing
    window focuses and unminimises instead of a second window opening.
14. **Unplug a monitor** — move the window to a second display, quit, unplug it,
    relaunch. The window recenters on a display that exists rather than opening
    off-screen.

---

## Testing

```sh
cargo test --manifest-path src-tauri/core/Cargo.toml   # the core pipeline
npm test                                               # the frontend
npm run typecheck                                      # tsc --noEmit, strict
cargo clippy --manifest-path src-tauri/core/Cargo.toml -- -D warnings
```

No test touches the network, a live provider, or an audio device. The core
injects traits and drives them with fakes and locally-bound scripted servers; the
frontend runs the real components against a mocked command/event bridge.

Every test is documented in [`docs/TESTING.md`](docs/TESTING.md) — what it
verifies and *why it exists*, which is the failure mode it guards.

---

## Security notes

- **Keys** are encrypted with Windows DPAPI and stored as `enc:<base64>`. If the
  keystore is unavailable they fall back to a *marked* `plain:<base64>` — honestly
  labeled rather than silently pretending to be encrypted. Values decrypt by
  their stored prefix, and anything undecryptable (a settings file copied from
  another machine) reads as **unset** rather than being handed to a provider as
  if it were a key.
- **Model output is untrusted.** The answer panel renders a markdown subset
  written in-repo. Every string reaches the DOM as a text node; there is no
  `dangerouslySetInnerHTML` and no attribute is ever derived from model text.
  Links are deliberately **not parsed** — `[text](url)` stays literal text, so
  there is no href to sanitize and no `javascript:` to smuggle.
- **Nothing sensitive is logged** — not keys, not resume text, not transcripts.

---

## Documentation

[`docs/README.md`](docs/README.md) is the index, with a suggested reading
order. The individual documents:

- [`docs/SPEC.md`](docs/SPEC.md) — the product contract the app was built
  against; code comments cite its § numbers, and its exact strings, timeouts,
  and ordering rules are load-bearing behavior.
- [`docs/TESTING.md`](docs/TESTING.md) — every test in the repo, what it
  verifies, and the failure mode it guards; how to run each suite.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — how the pieces fit: the
  crate/module map and the path a question takes from Record press to
  streamed answer.
- [`docs/adr/`](docs/adr/) — architecture decision records: the choices that
  could have gone another way, and why they didn't.
- [`docs/learn/`](docs/learn/) — a guided tour of the codebase, for coming
  back to it cold after months away.
- [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md) — symptom → cause →
  fix for things that break at runtime (audio, keys, hotkey, providers,
  window).
- [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) — the dev loop: building,
  running, testing, releasing.
- [`docs/SECURITY.md`](docs/SECURITY.md) — the threat model and the rules
  that enforce it: DPAPI-encrypted keys, untrusted model output, nothing
  sensitive logged.
- [`docs/IDEAS.md`](docs/IDEAS.md) — the roadmap: ranked future features,
  feasibility sketched against this architecture, and the trap version of
  each one not to build.

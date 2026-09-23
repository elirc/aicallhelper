# AI Call Assistant v3

A push-to-record **interview copilot** for Windows — and, through call
profiles, a copilot for sales, support and meeting calls. During a live call,
you press **Record** while the other person is asking a question. The app
captures the **system audio** (what *they* are saying — loopback, not your
microphone), streams it to speech-to-text so a live transcript renders while
they are still talking, and when you press **Stop & Answer** it streams an
AI-suggested answer — grounded in the active call profile: your resume and the
target job description for an interview, your background and the call context
for anything else — into the answer panel at the top of the window.

The window is built to sit **directly under your webcam**: the answer is the
first thing in the column, and one click (or the default launch placement)
docks the window to the top-centre of the screen, so reading the answer looks
like eye contact.

The single product promise is **stop-to-first-word latency of roughly one
second**, and the app reports the measured number after every answer.

Because you are on a call, the app requests Windows capture exclusion for its
window. Its effect depends on the Windows version and capture method. Check
the recorded compatibility results and test your intended sharing setup
before relying on it — see [tested sharing
configurations](docs/TROUBLESHOOTING.md#tested-sharing-configurations).

Single user, your own API keys, no telemetry, no server of ours — or no keys at
all with the free local mode (see below).

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

4. **Create a call profile** in Settings → *Profile*. A fresh install starts
   with one profile named *Default* (a v3 settings file migrates its resume and
   job description into it automatically). For each profile choose a **call
   type** — Job interview, Sales call, Customer support call, Work meeting, or
   Other — and paste the grounding text: for an interview, your **resume** and
   the **job description**; for any other call, **About you** and the **call
   context** (account, product, agenda). Optional **Focus** (what to emphasise,
   e.g. a tech stack) and **Extra instructions** ride along in the same cached
   prompt. Every answer is grounded in the *active* profile, and the model is
   instructed never to invent experience the profile does not support.

   Keep up to eight profiles (New / Duplicate / Delete). Once you have two or
   more, chips appear at the top of the main screen — switching there changes
   only which profile is active and never rewrites profile text.

5. **Put the answer under your camera.** The window opens docked to the
   top-centre of your display by default (*Settings → Window position at
   launch*). The ⬆ button in the header re-docks it at any time at a wider
   reading size, and the ◎ button (or **Ctrl+Shift+F** while the window is
   focused) toggles **focus mode**, which hides everything but the answer, the
   status line and the Record button.

### Build an installer

```sh
npm run tauri build
```

Produces a per-user NSIS installer under
`src-tauri/target/release/bundle/nsis/`. Before calling a build releasable, run
[`docs/RELEASE_CHECKLIST.md`](docs/RELEASE_CHECKLIST.md) against that exact
installer and record the result in its ledger.

The installer contains the app only. Free local voice setup
(`scripts/setup-free-voice.ps1` and `local-voice/`) is not bundled with it
and still runs from a source checkout; see
[`docs/FREE_VOICE_MODE.md`](docs/FREE_VOICE_MODE.md).

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
normally). In free local mode the answer deadlines are 90 s / 300 s — CPU
inference gets minutes where the cloud gets seconds, and those are ceilings,
not speed promises; the ~1 s promise is a cloud-mode promise.

**Pre-warming** fires on Record, on Stop and on the 120 s auto-stop — not on
Ask (the answer request follows microseconds later, so a warm could only race
it for the pooled connection) and not at launch (the pooled socket would be
reaped long before a first question, and it would be an unauthenticated
request on every start). It is why a single shared `reqwest::Client` is used
for all cloud LLM traffic: its connection pool is the thing being warmed. A
per-request client would throw the warmed connection away and the feature
would silently do nothing. The same client carries a 3 s connect timeout (so a
black-holed handshake fails fast enough for the one retry to matter) and TCP
keepalive from 20 s idle (so a NAT or VPN gateway does not forget the warmed
socket during a long recording), and deliberately no whole-request timeout —
the answer body streams. Local mode has nothing to warm; its pre-warm is a
no-op.

**Metrics** are measured in the core from the moment Stop was requested.
`sttFinalizeMs` is exactly `0` for typed questions — there was no STT stage, and
billing one would be a lie. If a provider ever returns a complete answer without
streaming a delta, `firstTokenMs` reports `totalMs` rather than `0`, because `0`
renders as "instant" and would lie about the one number this app is judged on.

### An honest note on prompt caching

The Anthropic system prompt is sent as two blocks with the cache breakpoint
after the profile block (call framing, resume or background, job description or
call context, focus, extra instructions), and the answer-style policy placed
*after* it — so flipping answer style is latency-free and never invalidates the
cached profile. Caching is a byte-prefix match, so the prompt is built to be
byte-stable across calls (no timestamps, no unordered joins), and that is
enforced by test. Only the *active* profile is in the prompt: switching
profiles is a deliberate, between-calls cache write, and an interview profile
with no focus or extra instructions builds exactly the v3 prompt, byte for
byte, so upgrading cost nobody a cache miss.

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
    src/stt/          deepgram.rs (WebSocket client), frame.rs (pure frame parsing),
                      local.rs (Moonshine speech over loopback, free local mode)
    src/llm/          anthropic.rs, groq.rs, local.rs (Ollama, NDJSON stream),
                      http.rs (the one shared client), retry.rs, warm.rs (pre-warm),
                      prompt.rs (call profile -> cached prefix), sse.rs
    src/session/      the session state machine (trait-injected dependencies)
    src/store/        settings + call profiles, DPAPI secrets, window-bounds
                      sanitising and dock-to-camera math
  src/                Tauri shell: commands (IPC envelopes), events, window
                      (geometry restore, dock, navigation policy, crash recovery),
                      hotkey, local_voice (service readiness/start), logging
                      (crash log), state
src/                  React frontend (thin view over the core)
  views/              MainView (answer-first layout), SettingsView (lazy)
  components/         answer/transcript panels, chips, Ask form, PracticeLibrary
                      (lazy), LocalVoicePanel, small hooks
  state/              the pure reducer + the session hook (bridge, timers, delta
                      coalescing)
  markdown/           in-repo streaming markdown renderer (no library, no sanitizer)
local-voice/          Python Moonshine speech service (127.0.0.1:8765)
scripts/              setup-free-voice.ps1, start-free-voice.ps1, test-free-voice.ps1
docs/TESTING.md       every test, and the failure mode it guards
```

The core is a separate crate from the Tauri shell on purpose: `cargo test -p
app-core` exercises every invariant with fakes, without a webview, a network, or
an audio device.

---

## Manual QA script

Run this end to end before shipping a build.

1. **First run** — launch with no keys. The window opens docked to the
   top-centre of the display, at its default 460×700 size (check on a
   125–150 % DPI display that it is not shrunken). The status line reads
   *"First run: open Settings (gear icon) and add your API keys"*. The
   "Suggested answer" panel is the first thing under the header.
2. **Add keys** — gear icon → paste the Deepgram and Anthropic keys → Save →
   *"Saved ✓"*. Reopen Settings: the key fields are empty with the placeholder
   *"saved — type to replace"*. Pick Groq: the Anthropic field hides and the
   Groq field appears; pick Free local voice: all three hide and the local
   readiness panel appears.
3. **Profiles** — Settings → Profile: rename *Default*, paste a resume and job
   description, Save. Press **New**, name it, set the call type to *Sales
   call*: the textareas relabel to *About you (optional)* and *Call context
   (account, product, agenda)*, and the Interview prep toggle on the main
   screen disappears while that profile is active. Save with the new profile
   selected → back on the main screen two chips appear, the new one pressed.
   Click the other chip: it becomes pressed only after the save returns, and
   its text was not touched (reopen Settings to confirm). Delete is disabled
   with one profile; New is disabled at eight.
4. **Unsaved changes** — edit a field in Settings and press Escape: *"Discard
   unsaved changes?"* appears instead of the form closing. Keep editing, then
   Discard. A clean form still closes on the first Escape. While a save is in
   progress the whole form is locked (Save reads *"Saving…"*, and Back and
   Escape do nothing), so nothing you type can be lost. If settings changed
   elsewhere while the form was open (a profile or style chip click that was
   still saving), Save is refused with *"Settings changed elsewhere —
   reload"*. **Reload** keeps your unsaved edits and takes everything you did
   not touch from the saved settings.
5. **Record system audio** — start a video call (or play any speech through your
   speakers) → press **Record**. The "Question heard" strip expands and the
   transcript appears **while the audio is still playing**, not after. The
   level meter fills a row that was already reserved — nothing above it moves.
6. **Stop** — press **Stop & Answer**. The answer streams into the top panel,
   the latency chip reads roughly *"1.Xs to first word"* (hover it for the
   breakdown), and the transcript strip collapses to a one-line caption of
   the question. Click the caption: it expands; the next recording collapses
   it again on its own.
7. **Ask** — type a question into the Ask box and submit. The answer streams;
   the latency chip shows a `0 ms` transcript-finalize stage in its tooltip.
8. **Regenerate** — press Regenerate. The same question is re-asked as a *new*
   history entry.
9. **Styles** — flip Brief / Balanced / Detailed. Answer length changes; latency
   does not (the cached prefix is untouched).
10. **History** — after 2+ entries the arrows and `n/m` appear in the answer
    panel's head. Navigate, confirm `n/m` tracks, then Clear (only enabled
    when idle) and confirm focus lands on the Record button.
11. **Stream follow** — set *While an answer streams* to *Stay at the opening
    sentence (teleprompter)* and ask a question long enough to scroll: the
    panel stays at the top while text arrives. Switch back to *Follow the
    newest text*: it sticks to the bottom, but only if you have not scrolled
    up.
12. **Focus mode** — press ◎ (or Ctrl+Shift+F with the window focused). The
    chips, Ask row, transcript strip and any local banner disappear; the
    answer text grows; Record, the style chips and the status line stay.
    Ctrl+Shift+F inside the Ask box must NOT toggle it. Relaunch: focus mode
    is off again.
13. **Dock to camera** — drag the window somewhere else and press ⬆. It snaps
    to the top-centre of the display it is on, 8 px under the work-area edge,
    at the wider reading size. On a second display, ⬆ docks to *that*
    display.
14. **Hotkey from another app** — focus a different window and press the hotkey.
    Recording starts. Press it again; it stops and answers.
15. **Hotkey taken** — set the hotkey to something another app already owns
    (e.g. one your screen-recorder uses). The notice appears and the chip does
    not, rather than leaving a dead key.
16. **Screen share** — share your screen in a call and look at the shared
    view from a second device (or record it). The app requests Windows
    capture exclusion; its effect depends on the Windows version (Windows 10
    version 2004 or later is required) and the capture method, so the
    expected result is "not visible in the shared view, still visible to
    you" only for configurations that have been tested. Note the
    conferencing app and version, Windows build and capture mode (whole
    screen or single window), and record the outcome in the [tested sharing
    configurations](docs/TROUBLESHOOTING.md#tested-sharing-configurations)
    table and the [results ledger](docs/RELEASE_CHECKLIST.md).
17. **Relaunch, docked** — with *Window position at launch* on *Dock under the
    camera (top centre)* (the default), resize the window, drag it to a
    corner, quit, relaunch. The size is restored; the position is the
    top-centre dock again, before the window becomes visible.
18. **Relaunch, remembered** — switch to *Remember where I left it*, move and
    resize, quit, relaunch. Position and size are restored. Switch back to
    *Dock under the camera*: the window docks the moment you save.
19. **Second launch** — start the app again while it is running. The existing
    window focuses and unminimises instead of a second window opening.
20. **Unplug a monitor** — with *Remember where I left it*, move the window to
    a second display, quit, unplug it, relaunch. The window docks under the
    camera on a display that exists rather than opening off-screen.
21. **Free local voice** (after `scripts\setup-free-voice.ps1`) — select it,
    press *Start and warm free mode*, Save. The banner *"Free local voice · no
    API fees"* appears on the main screen, no first-run nudge is shown even
    with no keys stored, and Record → Stop & Answer works with the internet
    off. The rest of the local script is in
    [`docs/FREE_VOICE_MODE.md`](docs/FREE_VOICE_MODE.md).

---

## Testing

```sh
npm run typecheck                                      # tsc --noEmit, strict
npx vitest run                                         # the frontend (~6 min: jsdom is slow to boot)
npm run build                                          # index + SettingsView + PracticeLibrary chunks
cd src-tauri && cargo test -p app-core                 # the core pipeline
cd src-tauri && cargo clippy -p app-core -- -D warnings
cd src-tauri && cargo test -p aicallhelper             # the Tauri shell tests
python -m pytest -q local-voice/test_server.py         # the local speech service (needs pytest)
```

The same gates run in CI on every push and pull request
([`.github/workflows/ci.yml`](.github/workflows/ci.yml)). A release also
needs the packaged build and the manual checks in
[`docs/RELEASE_CHECKLIST.md`](docs/RELEASE_CHECKLIST.md).

No test touches the network, a live provider, or an audio device. The core
injects traits and drives them with fakes and locally-bound scripted servers
(the local speech client, too, runs against a scripted loopback WebSocket); the
frontend runs the real components against a mocked command/event bridge.

Every test is documented in [`docs/TESTING.md`](docs/TESTING.md) — what it
verifies and *why it exists*, which is the failure mode it guards. The test
counts per suite live only there, so they have one place to go stale.

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
- **Profiles are plaintext.** Resume, job description, focus and extra
  instructions sit unencrypted in the same settings file as the encrypted keys —
  they are grounding text, not secrets, and the file lives in your own app-data
  folder.
- **The app never navigates.** There is no "open URL" command; an https link is
  bounced to your default browser by the window's navigation policy (handed to
  the OS as a single argument), and anything else is dropped.
- **Screen-share exclusion is requested and verified at launch.** The app requests
  Windows capture exclusion (`WDA_EXCLUDEFROMCAPTURE`, which needs Windows 10
  version 2004 or later) before the window is first shown. Its effect depends
  on the Windows version and capture method. Check the recorded compatibility
  results and test your intended sharing setup before relying on it. After
  the request the app reads the setting back from Windows and refuses to start
  (reason in `crash.log`) if it was not applied. Whether a given conferencing
  app honours it is only known from recorded tests; see
  [tested sharing configurations](docs/TROUBLESHOOTING.md#tested-sharing-configurations).
- **One command starts processes.** *Start and warm free mode*
  (`prepare_local_voice`) launches the local Ollama and speech services from
  the folder named in `%LOCALAPPDATA%\AI Call Assistant\local-voice.json`,
  which the free-voice setup script writes. Anything running as you can edit
  that file; the trust boundary is your Windows account. Details in
  [`docs/SECURITY.md`](docs/SECURITY.md).
- **Nothing sensitive is logged** — not keys, not profile text, not transcripts.

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
- [`docs/RELEASE_CHECKLIST.md`](docs/RELEASE_CHECKLIST.md) — the validation
  list for a release (deterministic gates, packaged build, manual hardware
  checks) and the ledger of recorded runs.
- [`docs/SECURITY.md`](docs/SECURITY.md) — the threat model and the rules
  that enforce it: DPAPI-encrypted keys, untrusted model output, nothing
  sensitive logged.
- [`docs/IDEAS.md`](docs/IDEAS.md) — the roadmap: ranked future features,
  feasibility sketched against this architecture, and the trap version of
  each one not to build.

## Interview preparation

Expand **Interview prep** below the question field to browse 12 practice
questions across Introduction, Behavioral, Technical, and Leadership topics.
Search or filter the library, then select a question to place it in the input.
You can edit it before pressing **Ask**; browsing and selecting a question do
not call an AI provider. The toggle is shown only while the active profile's
call type is *Job interview* — switching to a sales, support or meeting
profile hides it (and closes an open library).

The panel also includes three answer frameworks, a pre-call checklist, and
questions to ask the interviewer. These guides are bundled locally and work
without API keys. Generating answers still requires a configured answer provider.

Settings and the practice library load on demand. Settings starts loading when
you hover or focus its button, and practice styles load with the library.
Unchanged transcript, answer, and question-input panels skip parent-driven
renders during recording updates. A pending typed question disables the input
and Ask action until accepted or rejected, preserving failed drafts for retry.

## Free local voice mode

Select **Free local voice** in Settings to test with Ollama Qwen3.5 2B and
Moonshine English speech, without API keys or usage fees — no Deepgram key
either, and no first-run nudge. Two things differ from cloud mode: the answer
deadlines are 90 s / 300 s instead of 10 s / 60 s, and the whole prompt is
capped at about 7 KB — that cap counts the *active* profile in full (resume,
job description, focus and extra instructions) plus the instructions and the
question. Once local mode is selected, Settings shows how many bytes the
profile you are editing leaves for a question. The app computes this exactly,
with the same code that enforces the cap, from what you have typed but not
yet saved. It warns when little room is left (under 200 bytes). It says so
plainly when a profile is too long for local mode at all. Recording and Ask
are then refused in local mode before anything starts, and you can still
save the profile for a cloud model. See [setup, limitations and test
steps](docs/FREE_VOICE_MODE.md).

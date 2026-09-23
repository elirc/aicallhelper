# Ideas — the roadmap

Future features, each judged against the three things that make this app worth
using instead of a browser tab:

1. **The ~1 s stop-to-first-word promise** (SPEC §3). Anything that adds work
   between Stop and the first token pays for itself or dies.
2. **The single-session invariant** (SPEC §5). Every new state or stream
   multiplies the race matrix that took a full v2 of bugs to pin down.
3. **The privacy posture** (SPEC §1, §11). One user, their keys, three known
   cloud origins (Deepgram, Anthropic, Groq) — or, only while Free local
   voice is selected, two loopback origins on the same machine
   (`127.0.0.1:11434` Ollama, `127.0.0.1:8765` Moonshine) carrying no key
   material — nothing sensitive on disk beyond the plaintext profiles, nothing
   in logs, and a window whose Windows capture exclusion is verified at
   launch (whether a conferencing app honours it is only known from recorded
   tests).

Every idea names the modules it touches, its spec-level risks, an effort guess
(**S** = an afternoon, **M** = a few sessions, **L** = a project), and the trap
version — the way this feature usually gets built that would betray the
product. Traps are listed because the trap is always the *obvious* version.

---

## Do next — top 3

| # | Idea | Why now |
|---|------|---------|
| 1 | [Post-interview debrief export](#2-post-interview-debrief-export--m) | Highest value per unit of risk in the list. The interview's value doesn't end when the call does, and this is frontend-plus-one-save-dialog — it touches zero hot-path invariants. |
| 2 | [Per-question style modifiers](#1-per-question-answer-style-via-modifier-keys--s) | The cache split (§7) makes it latency-free *by construction* — the rare feature the architecture was pre-built for. Smallest effort on the list. |
| 3 | [Latency telemetry panel](#3-latency-telemetry-panel-local-only--s) | The product is judged on one number; start keeping score. Also the evidence base for later decisions — provider fallback (#8) and the free local mode's real numbers (#9) are guesses until this exists. |

The pattern is deliberate: none of the three touch `machine.rs`, the audio
path, or the stop-to-first-word window. Bank the cheap wins before spending
the risk budget.

---

## The ideas

### 1. Per-question answer style via modifier keys — S

**Problem**: `answerStyle` is a persisted setting; changing it mid-interview
means clicking chips between questions. You usually know *at the moment you
press Stop* whether this question wants one line or a STAR story.

**Sketch**: Shift+Stop → brief, Ctrl+Stop → detailed, plain Stop → the saved
default (same modifiers on the Ask submit). `stop_session`/`ask`
(`src-tauri/src/commands.rs`) grow an optional `styleOverride`, which swaps
only the `style_suffix` when the machine fills in the `AnswerRequest`. The
suffix already lives *after* the cache breakpoint
(`src-tauri/core/src/llm/prompt.rs:63-72`), so an override can never
invalidate the cached profile — this is the exact scenario the §7 split was
designed for. The persisted setting is untouched.

**Risks**: §9 pins the style chips' `aria-pressed` to the *persisted* style —
a transient override must render as a badge on the answer entry, not flip the
chips. Modifier detection on the *global* hotkey means registering extra
accelerator variants (`src-tauri/src/hotkey.rs` registers exactly one), and
the "hotkey taken" honesty rule (§9) then applies per-variant.

**Don't build**: a per-question free-text instruction box. Free text headed
for the system prompt breaks byte-stability (§7), and it's the first step of
generic-chatbot drift.

### 2. Post-interview debrief export — M

**Problem**: when the call ends you have nothing. History holds 6 entries
(`src/types.ts:126`) and evaporates on close — but reviewing what was asked
and what you answered is where the *next* interview is won.

**Sketch**: an "Export session" button (idle only) that renders the session to
one markdown file through a save dialog: per entry, the question, the answer
as markdown *source* (the Copy path already preserves it, §9), and the
metrics line. History trimming is the obstacle: `pushLive` trims beyond
`HISTORY_LIMIT` (`src/state/reducer.ts:118-129`), so add an append-only
session log in the reducer that trimming never touches — it's text, memory is
a non-issue. One new shell command for the save dialog; everything else is
frontend.

**Risks**: privacy. Transcripts on disk must be an explicit user gesture,
never automatic — §11's "log nothing sensitive" survives because export is
the user *choosing* to write their own data, not the app logging it. The
trim invariants (the in-flight entry is never trimmed) must not change.

**Don't build**: auto-save of every session, PDF rendering (markdown pastes
into anything), or any "sync". The moment sessions persist without a gesture,
this app has a data-retention story, and it should never need one.

### 3. Latency telemetry panel (local only) — S

**Problem**: the promise is ~1 s, but you only ever see the current answer's
number. You can't answer "does Groq actually beat Haiku *for me*?" or "will
hotel wifi hold up tonight?" with one sample.

**Sketch**: the numbers already exist — `Metrics`
(`src-tauri/core/src/session/mod.rs:25-34`) lands in every `llm:done` and
every history entry. Persist the last ~200 tuples of
`{timestamp, provider, style, sttFinalizeMs, firstTokenMs, totalMs}` to a
`metrics.json` beside `settings.json`, written with the same
tmp-then-rename discipline (`src-tauri/core/src/store/settings.rs`). Panel
shows p50/p95 per provider and a sparkline. Store numbers and provider names
only — no question text — so the file is never sensitive.

One number is already sitting there unclaimed. `AnthropicProvider`
(`src-tauri/core/src/llm/anthropic.rs`) already reads
`usage.cache_read_input_tokens` off every response into
`last_cache_read_input_tokens` and exposes it on a getter that nothing but the
tests calls. It is the only honest answer to "did the prompt cache engage?" —
the §7 breakpoint is a silent no-op below Haiku's 4096-token minimum, so today
that answer is a guess. Carrying it into `Metrics` is the smallest useful step
and the natural home for it: it's a count, never text, so it changes nothing
about what the file may hold.

**Risks**: none to the hot path — write after `llm:done`, debounced. The only
real risk is scope creep into the trap below.

**Don't build**: anything that leaves the machine. And don't store transcripts
next to the metrics "for context" — that silently converts a boring file into
a sensitive one, and #2 already covers the review use case honestly.

### 4. Global push-to-talk: hold vs toggle — M

**Problem**: a toggle requires remembering state mid-call. Walkie-talkie hold
matches the actual gesture: press while they talk, release when they stop.

**Sketch**: the plugin already delivers key state — `apply_hotkey` reacts to
`Pressed` only, deliberately (`src-tauri/src/hotkey.rs:44-50`). Hold mode maps
Pressed→start, Released→stop. Settings grow `hotkeyMode: "toggle" | "hold"`
(per-field fallback like every other field, §8).

**Risks**: a swallowed key-up (focus stolen mid-hold, keyboard quirks) leaves
it recording — the 120 s cap (§3) is the backstop and the UI already handles
that path (`src/state/reducer.ts:260-277`). Sub-250 ms holds need a decision:
treat as a toggle press or you'll generate zero-frame sessions that end in
`no_speech`. Decide it in the reducer where it's testable.

**Don't build**: low-level keyboard hooks to catch keys the plugin can't.
That's an anticheat-shaped rabbit hole, and the plugin's honest
`registered: false` reporting (§9) is a feature worth keeping.

### 5. Question auto-detect (auto-press Record) — L, contested

**Problem**: the fumble for Record while the interviewer is already three
words into the question.

**Sketch**: **not** server endpointing. §6.1 deliberately omits
`endpointing`/`no_delay` — this client never waits on the endpointer, so
those knobs only cost smart_format quality
(`src-tauri/core/src/stt/deepgram.rs:33-37`). Instead, a *local* pre-listener:
the capture path already produces per-frame RMS
(`src-tauri/core/src/audio/capture.rs:312-319`), so an armed idle mode can run
capture with a speech-band energy gate and auto-press Record when sustained
level appears. **Stop stays manual** — you know when the question ended; the
machine doesn't. That asymmetry *is* the product's no-endpointing thesis.

**Risks**: a real posture change — today the device is open only between
Record and Stop. Always-listening (even locally, discarding audio) needs an
explicit arm/disarm control and a visible indicator, or the app becomes the
thing it promised not to be. False triggers on notification sounds. The
auto-start must go through the same supersession path as a human press
(§5.1–5.2) — no side doors into `machine.rs`.

**Don't build**: auto-*stop* / auto-answer via VAD. It fires ~1 s early on
half-questions and answers the wrong thing fast. The deliberate Stop is what
makes the transcript complete and the answer aimed.

### 6. Mic + loopback dual capture — L

**Problem**: the model only ever hears the other side. It can't know what you
already said — which blocks real follow-up context (#7 does it textually) and
coaching (#10) entirely.

**Sketch**: a second cpal stream on `default_input_device` —
`open_loopback_stream` currently asks only for the default *output* device
(`src-tauri/core/src/audio/capture.rs:138-140`). Best wire shape: one
Deepgram socket with `channels=2`, mic and loopback interleaved — speaker
attribution comes free by channel, one keepalive, one finalize. The session
machine's `Phase::Recording` holds one stream
(`src-tauri/core/src/session/machine.rs:51-59`); dual capture means teardown,
abort, and finalize apply to a pair. Your channel feeds *context*; the
question remains the loopback transcript.

**Risks**: 2× Deepgram audio billing. The two capture clocks drift, so
interleaving needs an honest resync strategy, and finalize now flushes both
sides inside the same 5 s cap (§3). Echo: your voice through their speakers
shows up on both channels.

**Don't build**: mixing both into one mono channel. Attribution is the entire
value; a blended channel gives you double cost for garbage context.

### 7. Follow-up mode: thread the previous Q/A — M

**Problem**: "tell me more about that" arrives with no *that*. Every question
answers cold.

**Sketch**: opt-in toggle. When on, the machine builds the user message with
the previous entry's question and answer prepended *inside the user turn* —
`AnswerRequest` already carries the transcript and is filled at finalize time
(`src-tauri/core/src/llm/mod.rs:63-83`). Placement is the whole design: in
the **user message** the cached prefix stays byte-identical and the cache
cost is exactly **zero** (the user turn was never cached, §7). In the system
block it would invalidate the prefix every question — with a big profile
that's a 1.25× cache *write* per question instead of 0.1× reads (§3 honesty
note). Cap at one previous exchange.

**Risks**: input tokens grow by roughly the previous answer's length. A wrong
previous answer poisons the next one — the toggle must be visible and easy to
kill mid-interview.

**Don't build**: full conversation memory. Ten turns of thread makes every
answer slower, blander, and more anchored to its own earlier mistakes. This
is a copilot for discrete questions.

### 8. Provider fallback chain on `llm_http` — M

**Problem**: Anthropic returns a 529 mid-interview while a working Groq key
sits in the settings file.

**Sketch**: on `llm_http`/`llm_rate_limit` **before any delta reached the UI**
— the same predicate discipline the retry helper enforces
(`src-tauri/core/src/llm/retry.rs`, §6.4) — rebuild for the other provider
(`SystemPrompt::joined()` already produces the Groq shape,
`src-tauri/core/src/llm/prompt.rs:74-81`) and fire once, with a visible
"answered by Groq" badge. Implement as a wrapping `LlmProvider`
(`src-tauri/core/src/llm/mod.rs:90-111`) so `machine.rs` doesn't change.
Prewarm both origins on Stop — the throttle is already per-origin
(`src-tauri/core/src/llm/warm.rs:34-60`).

**Risks**: worst-case first-token roughly doubles, and the 10 s first-token
budget (§3) has to absorb attempt one's failure *plus* attempt two. Only arm
it when the second key exists. The after-a-delta rule is absolute — a
fallback answer concatenated onto a half-streamed one is the §6.4 bug in a
new costume.

**Don't build**: silent failover. A different provider is a different voice;
mid-interview the user must know. And never retry-then-fallback (three
attempts) — the budget math stops working.

### 9. Local STT (and answers) — shipped as Free local voice; whisper.cpp remains an option — L

**Status**: the mode exists. **Free local voice** pairs Moonshine Tiny
Streaming (English speech over a loopback WebSocket, `core/src/stt/local.rs`)
with Ollama + Qwen3.5 2B for answers (`core/src/llm/local.rs`), selected as
a provider in Settings, with no keys at all
([FREE_VOICE_MODE.md](FREE_VOICE_MODE.md)). It was built exactly the way
this entry asked: through the trait seams, so `machine.rs` and every §5
invariant test apply unchanged; as a *mode the user picks*, labelled with
its real ceilings (90 s first token / 300 s total, §3); never a silent
fallback. The choice of speech path is a capability of the provider
(`uses_deepgram()`), not a separate `stt` setting.

**What is left of the idea**: whisper.cpp as an alternative speech engine
(a `base.en` model handles accents Moonshine Tiny does not; the
[model survey](LOCAL_MODELS_REPORT.md) already compared them), and a larger
local answer model for machines with the RAM. Both slot into the existing
seams: another `SttConnector` behind the same `LocalConnector` contract,
another model constant behind `LocalProvider`.

**Risks**: unchanged — the latency promise. CPU inference cannot make the
~1 s number; the metrics pipeline reports the honest one (§3). The 7 KB
prompt cap is per active profile, focus and extra instructions included.

**Don't build**: automatic fallback mid-session, or making local the silent
default. Still "offline/cheap", never "the same but free".

### 10. Coaching mode: post-answer critique — L (depends on #6)

**Problem**: the app tells you what to say and never how you actually did.
The gap between the suggested answer and what came out of your mouth is the
most useful data the tool could produce.

**Sketch**: requires mic capture (#6). After each exchange, a second cheap
LLM call: question + what-you-actually-said → two sentences of critique, one
improvement. Runs as a *review queue* after the call (pairs with #2's
export), never live. A separate prompt — byte-stability doesn't matter here,
it's a different call class with no latency promise.

**Risks**: doubles LLM calls per question; mic transcript quality bounds
critique quality; and the standing temptation to surface it live — resist it
(see trap).

**Don't build**: real-time "you're rambling" alerts. Nobody performs better
mid-sentence under a blinking coach; live critique converts a copilot into a
saboteur.

### 11. Configurable answer length tied to speaking pace — S

**Problem**: `MAX_ANSWER_TOKENS` is a constant 1024
(`src-tauri/core/src/llm/mod.rs:25`); "detailed" answers can outrun an
interviewer's patience. You speak ~140 wpm — a 60-second answer is ~140
words, well under the cap, so the cap alone doesn't shape length.

**Sketch**: settings gain `maxAnswerSeconds` (30 / 60 / 90) mapping to (a) a
proportional token cap in both provider bodies
(`request_body` in `src-tauri/core/src/llm/anthropic.rs` and `src-tauri/core/src/llm/groq.rs`)
and (b) one sentence appended to the **style suffix** — after the cache
breakpoint, so it's latency-free (§7).

**Risks**: hard token caps truncate mid-sentence — keep the cap generous and
let the suffix text do the shortening. Both providers' request-shape tests
pin body fields; they move with the change, which is the point.

**Don't build**: WPM calibration flows. Three presets. The style system
already exists; this is one more lever on it, not a new system.

### 12. Remember geometry per display arrangement — M

**Problem**: laptop ↔ dock changes the monitor set; the 40 px rule rightly
drops the now-offscreen position (§8, `src-tauri/core/src/store/bounds.rs`)
and the window re-docks to the camera — so with "Remember where I left it"
you re-place it on every dock/undock cycle. (With the default "Dock under
the camera" launch placement, ADR 013, this problem mostly dissolves: the
window goes to the top-centre of whatever display it is on, every launch.
What is left is the user who wants a *remembered* spot per arrangement.)

**Sketch**: key saved bounds by a display-set fingerprint — the shell already
enumerates monitors and their work areas for sanitizing
(`src-tauri/src/window.rs`, `current_work_areas`); hash the sorted rect list.
`windowBounds` becomes a small map `{fingerprint → bounds}` capped at ~4
arrangements. The sanitizer applies unchanged *per entry*, and a corrupt
entry drops as a unit while the rest of the file survives — the existing §8
discipline, widened. The map must key the *placement* too: a docked laptop
arrangement must not restore a remembered desktop position.

**Risks**: settings migration (scalar → map) must fall back per-field, never
nuke the file (§8). The bounds geometry cases are pinned by tests — extend
the matrix, don't loosen it.

**Don't build**: a general window manager. One window, remembered per place
it lives. Nothing else.

### 13. Import job description from URL — M

**Problem**: getting a JD out of a job board means fighting cookie walls and
markup; most people paste half of it, badly.

**Sketch**: a "Fetch from URL" button in Settings: one new shell command that
GETs the page (https only — same instinct as the §9 external-link guard),
strips it to text (drop script/style/nav, collapse whitespace), and fills the
JD textarea **for the user to trim before saving** — the fetch proposes,
the save decides, which is already the settings model
(`src-tauri/core/src/store/settings.rs:67-103`).

**Risks**: posture change — today the app talks to exactly three known
origins; a user-supplied URL is a fourth, arbitrary one. Keep it strictly
user-initiated and label it. Many boards render JDs client-side; accept that
this fails on some sites and say so in the error.

**Don't build**: a headless-browser scraper or per-board adapters. When the
fetch fails, the fallback is paste — which already works and costs zero
maintenance.

### 14. Pause/resume within a recording — M

**Problem**: the interviewer detours ("let me pull up your CV…") and you
either burn the 120 s cap on dead air or stop and lose the question's
continuity.

**Sketch**: pause = stop forwarding frames, keep the socket open. The
plumbing mostly exists: keepalives already carry silent stretches every 8 s
(`src-tauri/core/src/stt/deepgram.rs:42-45`), and audio routing is already
phase-gated — frames are dropped by phase, not by luck (§5.4,
`src-tauri/core/src/session/machine.rs:51-59`). Add `Phase::Paused` where
`push_audio` drops frames but `stop` still takes. Cost is favorable: Deepgram
bills audio time, and a paused stream sends none. Let the 120 s cap keep
counting wall-clock — it exists to bound the *session*, and a paused session
is still holding a socket and a slot.

**Risks**: a fourth phase multiplies the §5 race matrix — pause during
connect, pause after stop, hotkey-vs-pause — and every new transition needs
the same taken/not-taken rigor and tests. smart_format may format entities
oddly across a long gap.

**Don't build**: buffering paused audio for replay. That reorders time — the
transcript stops corresponding to what was said and when, which is exactly
the "answers the wrong question" failure §5.5 exists to prevent.

### 15. Choose the capture device — S/M

**Problem**: capture is hardwired to the *default* render device
(`src-tauri/core/src/audio/capture.rs:138-140`). If the call plays through a
headset but you want Windows' default elsewhere, you're flipping OS defaults
mid-day.

**Sketch**: settings grow `captureDevice` (name, or empty = default);
`open_loopback_stream` selects by name with an honest fallback to default
when the name is gone; Settings gets a dropdown from cpal's
`output_devices()`. The device-switch watcher changes meaning: for an
explicit choice, watch that *device's* existence rather than the default's
identity (`src-tauri/core/src/audio/capture.rs:217-286` — the
`default_device_moved` predicate is already split out pure for exactly this
kind of edit).

**Risks**: device names are not stable identifiers across replugs on some
drivers — fall back with one honest error, the rule the watcher already
follows (one report per death, never a toast storm).

**Don't build**: per-app audio capture (WASAPI process loopback). It's
Windows-version-gated, wreathed in COM, and ships nothing for weeks while
solving a problem the device picker already covers.

### 16. Re-ask with an edited question — S

**Problem**: the transcript came out 95% right — one mangled name — and
Regenerate would just re-run the wrong words.

**Sketch**: pure frontend. An "Edit & re-ask" affordance on the viewed entry
drops its question text into the Ask box; submitting goes through the
existing `ask()` path with all its validation and supersession behavior
(§5.8). It's Regenerate plus a prefill — a new history entry, same as
Regenerate makes one.

**Risks**: essentially none — no core changes, no new events.

**Don't build**: an edit gate *before* the answer fires. A confirm step in
the stop-to-answer window would tax every answer ~seconds to serve the rare
bad transcript. The 1 s promise is the product; this feature fixes mistakes
*after* the fast path, never inside it.

---

## Anti-goals

Things that would betray the product, no matter how nicely they demo:

- **Cloud accounts, hosted relays, "our" server.** The pitch is single user,
  own keys, three known cloud origins — or two loopback ones in free local
  mode (§1). A server component converts a privacy tool into a trust
  decision, and there is no feature above that needs one.
- **Telemetry, ever.** §11's "log nothing sensitive" extends to "phone home
  never". The latency panel (#3) is the model: measure everything, keep it on
  the machine.
- **Generic chatbot drift.** No conversation UI, no personas, no "chat with
  your resume". Every feature must serve the ~8 seconds between Stop and
  speaking; anything that doesn't is a different product wearing this one's
  window. **Call profiles are not personas** (ADR 014): a profile is
  grounding *data* chosen per call — the user's own resume, the job or
  account, what to emphasise, their own extra instructions — and the role
  instructions never change. The call type is a closed enum with one pinned
  sentence per variant; there is no free-text "who the AI is". A feature
  that lets the app speak as someone other than the user is the drift this
  bullet exists to stop.

**Decided with v3.1 — don't build** (the reasoning is in the design
studies and ADR 013/014; listed here so it does not get re-proposed):

- *Auto-detecting the call type or profile from the transcript.* A wrong
  guess silently grounds the answer in the wrong JD — the exact
  "confidently wrong in one second" failure profiles exist to prevent — and
  it does work inside the stop-to-first-word window. Selecting a profile is
  a deliberate gesture, like Stop.
- *Per-question profile switching* (or modifier-key profile overrides). The
  cache split exists for per-question *style* (#1); a profile is per-call by
  definition, and a per-question prefix change is a cache write on every
  question with a large profile (ADR 007).
- *Per-profile default answer style.* A second source of truth for the
  chips' `aria-pressed` contract (§9). Revisit only if users report flipping
  the style after every switch — and then write through the same persisted
  field.
- *Free-text call types.* Unpinnable prompt text headed for the system
  block.
- *Auto-docking mid-call, a second global hotkey for docking, a frameless
  title bar.* A window that jumps mid-call is worse than a stable one;
  `hotkey.rs` owns exactly one shortcut; native decorations stay (§9).
- **The app never speaks for you.** No TTS autopilot, no auto-submitted
  answers anywhere. The human says the words — the tool's job ends at the
  suggestion.
- **Concealment arms races.** Content protection exists so *your notes* aren't
  broadcast to the call (§9). Escalating into anti-proctoring or
  detection-evasion features is a different product with different ethics,
  and it isn't this one.
- **Auto-stop endpointing.** Restated from #5 because it will keep coming up:
  the machine deciding "the question is over" is the one latency optimization
  this product deliberately refuses (§6.1). It answers half-questions early
  and wrong, and wrong-fast is worse than right-in-a-second.

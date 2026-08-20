# Code Review — AI Call Assistant v3

An independent review of the whole repository, conducted 2026-08-20 against commit
`d18cd33` on a clean Windows 11 machine (nothing preinstalled — Node, Rust, and the
MSVC toolchain were installed from scratch as part of the review).

Six reviewers worked in parallel over separate areas: the session state machine and
audio path, the LLM provider layer, the STT client and secret storage, the Tauri
shell, the React frontend, and the documentation set. Their findings were then
cross-checked, and the two most consequential claims were verified independently —
one by reading the dependency source, one by writing and running a test. Both held.

Nothing here is theoretical. Every defect below was traced to a code path.

---

## Verdict

This is genuinely good code. That needs saying plainly before the list of problems,
because the list is long and would otherwise misrepresent the whole.

The evidence for that judgment is specific, not polite. The SSE decoder is correct
under hostile chunking, including split-UTF-8 and split-CRLF, and proves it with an
all-split-points invariant test. The retry policy's "never retry after a token was
delivered" guard is airtight — the flag is set synchronously before the sink call in
both providers. DPAPI is used correctly, down to the null check before
`from_raw_parts` and the copy-then-`LocalFree` ordering. The write-only key boundary
is enforced by the type system (`Settings` and `SettingsView` are different types),
not by discipline. The audio callback does no allocation and takes no locks. The
capabilities allowlist is minimal, and notably does *not* grant
`allow-set-content-protected`, so compromised frontend JavaScript cannot switch off
the protection. The XSS defense holds — traced end to end, every model-derived string
reaches the DOM as a text node.

The documentation is more accurate than most commercial products'. Of roughly 25
sampled `file:line` citations, every one pointed at the code it claimed. All 271
documented Rust test names exist verbatim. The README's self-critical note about
prompt caching being a silent no-op below 4096 tokens is repeated in the code itself.

So the problems below are not the problems of careless work. They are, with striking
consistency, the problems of *careful work that verified the wrong thing*.

### The pattern worth internalizing

Three reviewers, working on unrelated files and unaware of each other, found the same
bug shape. In each case the author clearly identified a failure mode, wrote a comment
explaining why it must never happen silently, and then guarded a **proxy** for the
condition rather than the condition itself.

| Location | The guard checks | What it needed to check |
|---|---|---|
| `lib.rs:97` content protection | the message reached the event loop | the OS *accepted* the display affinity |
| `groq.rs:150`, `anthropic.rs:188` | any SSE *event* was seen | any *text* was delivered |
| `deepgram.rs:414` finalize drain | send failure, socket error | …and plain timeout expiry |

Each guard is one level of indirection away from the thing that actually matters. The
comments are right; the checks are adjacent to right. If there is one lesson to carry
into future work on this codebase, it is: **assert the postcondition, not the step you
believe produces it.** Where the postcondition is observable — `GetWindowDisplayAffinity`
readback, a `delivered_any_text` boolean, a timeout branch that reports — make the
code observe it.

---

## Findings by severity

### HIGH — Screen-share invisibility fails open, silently, and the docs claim it cannot

**`src-tauri/src/lib.rs:97` · `docs/SECURITY.md:107`**

This is the app's defining feature and its single most damaging possible failure.

The code reads `win.set_content_protected(true)?`, with a comment stating that "if the
OS refuses, launching anyway would silently break the one promise this app makes — so
the launch fails instead." SECURITY.md repeats it: "if the OS refuses, the launch fails
rather than silently breaking the promise."

Neither is true, and the `?` cannot make them true. Verified independently by reading
the vendored dependency sources:

- `tao-0.35.3/src/platform_impl/windows/window.rs:1090` — `pub fn set_content_protection(&self, enabled: bool)`
  returns `()`. It has **no failure channel at all**, and internally executes
  `let _ = SetWindowDisplayAffinity(...)`, discarding the OS result.
- `tauri-runtime-wry-2.11.4/src/lib.rs:2333` — the `Result` returned by
  `set_content_protected` reflects only whether the message was dispatched to the
  event loop, never whether Windows honoured it.

`WDA_EXCLUDEFROMCAPTURE` requires Windows 10 v2004 (build 19041) or later. On anything
older the call fails, affinity stays `WDA_NONE`, the launch succeeds normally, and the
window is fully visible in every screen share. There is no `GetWindowDisplayAffinity`
readback anywhere in the repository.

**Failure scenario.** A user on Windows 10 1909 launches the app. No error appears. They
join a call believing the window is hidden, share their screen, and the interviewer sees
the copilot.

Given what this app is for, being wrongly *confident* of invisibility is worse than
having no invisibility feature at all — a user who knows the window is visible behaves
accordingly; a user falsely assured does not.

**Fix.** Read the affinity back with `GetWindowDisplayAffinity` after setting it and
compare against `WDA_EXCLUDEFROMCAPTURE`. On mismatch, either refuse to launch (as the
comment promises) or show an un-dismissable warning. Then correct SECURITY.md. A debug
assertion on the readback would have caught this at authoring time.

*Related, unverified:* even where the affinity succeeds, behaviour varies by capture
method. Modern capture (Windows Graphics Capture, DXGI duplication — what Zoom, Teams
and Meet use) omits the window. Legacy GDI `BitBlt` paths have been reported to render a
black rectangle instead, which is itself conspicuous. Hardware capture cards, RDP-side
capture, and a phone camera pointed at the screen are untouched — SECURITY.md is honest
about the last of these.

---

### MEDIUM — Confirmed defects

**1. Groq mid-stream error frames are swallowed; the stream completes as a silent empty answer.**
`src-tauri/core/src/llm/groq.rs:166-194`, guard at `:150`

`apply_event` reads only `choices[0].delta.content`. Any other JSON — including the
OpenAI-compatible `data: {"error":{...}}` frame Groq emits when generation fails
mid-stream — is silently ignored. The empty-response guard then checks `saw_event`
rather than whether any text was delivered. A 200 response containing one error frame
yields `saw_event == true`, `answer == ""`, and resolves `Ok("")` — an `llm:done` with an
empty answer and "successful" metrics. This is precisely the failure the code's own
comment ("would render as the model silently saying nothing") claims to prevent.
`anthropic.rs:231-239` *does* handle mid-stream error events; Groq does not.

**2. Same wrong predicate on the Anthropic path.** `anthropic.rs:188-197` — a 200 stream of
only `message_start`/`ping`/`message_stop` passes the `saw_event` guard and resolves to an
empty answer. Rarer, same root cause. Both guards should test "no text delivered".

**3. STT drain timeout silently truncates the transcript tail.**
`src-tauri/core/src/stt/deepgram.rs:414` — `let _ = tokio::time::timeout(limits::STT_FINALIZE, drain).await;`

The drain reports a cut flush on send failure (`:357`, `:376`), on socket error (`:404`),
and on a server error frame (`:389`). But if Deepgram merely takes longer than 5 s to
flush and close after `CloseStream`, the timeout expires, the result is discarded, **no
error fires**, and `finalize()` returns the transcript minus its held-back tail —
which is exactly where `smart_format` holds numbers and dates. Two error paths were
carefully built; the third exit was not. Contradicts the module's own stated invariant
at `:366-369`: "a silently truncated question is answering the wrong question."

**4. `ask` emits its first event before the caller can adopt the session id.**
`machine.rs:313-322` · `src/state/reducer.ts:44-48`

`ask()` spawns a task whose first action emits `SttPartial`, then returns `Ok(id)`. On
Tauri's multi-threaded runtime that emit races the IPC response. The frontend
deliberately **drops** events arriving before id adoption ("every event must be dropped,
not buffered"). SPEC §5.8 pins the rule this breaks: ask "resolves with the id before any
event fires." Result: an intermittently blank "Question heard" panel, and with a hot LLM
connection, a clipped answer prefix that self-heals only at `llm:done`.

The inline tests **cannot** catch this. `#[tokio::test(start_paused = true)]` is a
current-thread runtime, where a spawned task cannot run until the test awaits. The
ordering the tests observe is not the ordering production has.

**5. A panic in the driver task strands the session with no event and an occupied slot.**
`machine.rs:233-235`, `:313`

Both `tokio::spawn` handles are discarded — no `catch_unwind`, no join watchdog. A panic
anywhere downstream (a provider bug, an `unwrap` in SSE handling) is swallowed at the
task boundary: the slot stays occupied, the gate stays alive, no `session:error` is
emitted. The UI sits in "Generating answer…" forever; `stop` returns NotTaken. This
contradicts SPEC §11's promise that "the worst case is one failed answer and a structured
error event." Confining a panic is not the same as reporting it.

**6. `finalizing` is a dead end with no UI escape.** `useSession.ts:111` · `RecordButton.tsx:43` · `MainView.tsx:152`

If `stopSession` resolves `ok` and the core then never emits, the app is stuck
permanently: Record is disabled, `toggleRecord` is an explicit no-op, the ask form is
disabled, and the reducer has no timeout action. `starting` is escapable via abort and
`answering` via supersede — only this state has zero recovery. One dropped event
mid-interview means restarting the app. The design leans entirely on core-side timeouts,
which is a defensible bet, but it is the only state with no floor under it.

**7. CommonMark paragraph-interruption guard is missing on list continuation lines.**
`src/markdown/parse.ts:274-281`, guard defined at `:152`

**Verified empirically during this review**, by writing and running a test against the
real parser:

| Input | Expected | Actual |
|---|---|---|
| `The company took off in\n1997. That was a big year` | one paragraph | one paragraph ✅ |
| `- The company took off in\n1997. That was a big year` | one list | **two lists** ❌ |
| `1. built in\n1997. It grew` | 1 item | **2 items** ❌ |

`canItemInterruptParagraph` exists — per its own comment — to stop a wrapped line like
`…in\n1997.` becoming `<ol start="1997">`. It is consulted on the top-level paragraph
path (`:227`) but never inside `parseList`, where any line matching `matchListItem` is
accepted unconditionally. The bug the guard was written for still occurs, one nesting
level down. No security impact; visible rendering mangling on a very common LLM output
shape. The existing regression test pins only the fixed path.

**8. `open_external` is live IPC surface with no caller.**
`src-tauri/src/commands.rs:283-289`

Registered in the invoke handler, zero call sites anywhere in `src/`. The guard itself is
good (https-only, rejects whitespace and control characters, single-argv `explorer.exe`),
but an unreachable command is pure attack surface. Remove it or wire it up.

**9. No error boundary anywhere in the React tree.**

Zero `componentDidCatch` / `getDerivedStateFromError`. A single uncaught throw in the
hand-written parser unmounts the entire root — a blank window, mid-call. No throwing path
was found and the parser is well tested, but the blast radius of any miss is total. One
boundary around `<Markdown>` would confine it.

---

### LOW — Confirmed

- **`max_tokens` truncation is invisible.** Neither provider reads `stop_reason` or
  `finish_reason`. With `MAX_ANSWER_TOKENS = 1024`, an answer cut mid-sentence renders as
  a complete answer. (`llm/mod.rs:25`)
- **Unbiased select lets the first-token timeout beat a delta that already painted.**
  `machine.rs:700-741` — a one-poll-wide window where `llm_first_token_timeout` is reported
  *after* answer text appeared. A `biased` ordering closes it for free.
- **`firstTokenMs` can report `totalMs` even when deltas did stream** — same unbiased
  select. The error is in the honest direction (overstates, never understates), but the
  README's claim that this happens only when a provider doesn't stream is not strictly
  what the code does.
- **Corrupt settings file plus any save permanently destroys the original.**
  `store/settings.rs:47-53` — an unparseable file loads as defaults; the next save (or the
  automatic shutdown bounds write) overwrites a file whose resume and JD were likely 99%
  recoverable. Per-field fallback protects against a bad *value*, nothing protects against a
  bad *byte*. A `settings.json.corrupt-<ts>` rename before first overwrite would close it.
- **No mid-stream backpressure.** `deepgram.rs:85` uses an unbounded channel; the 15 s cap
  applies only to the pre-open flush. A stalled socket on a long recording accumulates
  unboundedly (~32 KB/s).
- **Surround downmix assumes channels 0/1/2 are FL/FR/FC** (`resample.rs:107-120`) without
  consulting the channel mask. On a 2.1 device the LFE is averaged into the speech mix.
- **No anti-aliasing filter before decimation** (`resample.rs`) — 48→16 kHz linear
  interpolation folds 8–24 kHz content into the speech band. Probably an acceptable trade,
  but undocumented and unmeasured.
- **Input-validation errors are miscoded as `internal`** (`commands.rs:248-260`), which the
  bridge treats as "the shell itself is broken."
- **Focus is dropped at the stop transition** — `RecordButton` becomes `disabled` while
  focused, throwing focus to `<body>`. `aria-disabled` plus a click guard would keep it.
- **The live transcript has no live region**, so a screen-reader user hears "Recording call
  audio…" and never the transcript itself.
- **A freed hotkey stays dead until edited or restart** (`commands.rs:88-91`) — no retry if
  another app releases the combo mid-session.
- **CSP lacks `base-uri`, `form-action`, `frame-ancestors`.** `object-src`/`frame-src` fall
  back to `default-src 'self'`, so the real residual is `form-action`. Defense-in-depth only —
  it requires HTML injection, which the text-node-only renderer prevents. Costs nothing to add.

---

## Where the reviewers disagreed

Worth recording, because the disagreement is itself informative.

The documentation auditor concluded there were **"no actively misleading claims"**, having
verified roughly 25 `file:line` citations and a list of security claims including the
DPAPI scheme, the URL guard, and the CSP. The shell reviewer found SECURITY.md's
content-protection claim to be materially false.

**The shell reviewer is right.** I verified it independently in the dependency source.

The docs auditor was not careless — it was thorough, and every claim it *did* check held
up. It missed this one because the claim is not falsifiable from within this repository.
Confirming it requires reading `tao`'s Windows backend to discover that the function has
no failure channel. Every in-repo signal — the comment, the `?`, the `Result` type in
Tauri's public API — points the other way.

That is the general lesson: **a claim about what the OS did cannot be verified by reading
your own source.** Where a doc asserts something about external behaviour, the assertion
needs a runtime check behind it, or it is a hope with a citation.

---

## Test coverage — an honest assessment

The suites are strong and pass cleanly. Measured on this machine:

| Suite | Result |
|---|---|
| `cargo test -p app-core` | 242 passed |
| `cargo test` (shell crate) | 35 passed |
| `npm test` | 293 passed |
| `npm run typecheck` | clean, `strict` + `noUncheckedIndexedAccess` |

The tests are meaningful, not trivia. The every-cut-point streaming convergence test, the
whole-tree attribute audit in `xss.test.tsx` (which runs against *benign* input too, so the
allowlist cannot rot), the all-split-points SSE invariant, and the pointer-identity proof
that the retry body is built once are all exactly the right properties to pin. Test
comments consistently name the failure mode each test guards.

The gap is structural and worth stating plainly:

**Every state-machine test runs on a paused, current-thread runtime.** `start_paused = true`
gives deterministic *cooperative* interleavings. True parallelism — `push_audio` from the
forwarder OS thread racing `stop`, `on_delta` from a socket task racing a timeout poll — is
never exercised. Defects 4 and the unbiased-select races above are invisible on this
harness *by construction*. The suite cannot catch them no matter how many cases are added,
because the runtime it uses does not have the concurrency the bug requires. There is no
multi-threaded stress test and no loom-style exploration.

Other specific gaps:

- No test for a Groq mid-stream error frame, or a 200 stream with events but zero text
  deltas — either would have failed immediately on defect 1.
- No test for the drain-timeout path — every existing drain test ends with a server close
  or error frame.
- No test injects a panicking `SttConnector` or `LlmProvider` (defect 5).
- No test feeds a wrapped ordinal *inside* a list item — coverage stops exactly where
  defect 7 begins.
- `connect_timeout_surfaces_stt_connect` tests a path production never takes: the real
  connector returns before the handshake resolves, so real dial failures arrive via the
  error channel. The test is green while guarding a near-vacuous branch.
- No verification anywhere that display affinity took effect — the single most important
  untested property in the app.
- No property or fuzz testing. The streaming suite runs over a fixed 16-document corpus;
  random documents compared batch-versus-stream would have found defect 7's whole family.

---

## Documentation

Accurate and unusually honest, with real drift.

**Verified correct:** the parallel connect+capture, the 15 s drop-oldest buffer, the
pre-warm on Record/Ask/Stop, core-enforced timeouts (5/10/60/120 s exactly as documented),
the DPAPI fail-closed scheme, verbatim prompt strings, and all 14 manual QA steps. All 271
documented Rust test names exist; none are undocumented.

**Drift — stale test counts.** TESTING.md correctly says 242 core / 35 shell / 293 frontend
(this matches my measured runs). Five other documents still say 233 / 34 / 290:

- `docs/DEVELOPMENT.md:54-56`
- `docs/README.md:12-13`
- `docs/ARCHITECTURE.md:23,25,29`
- `docs/adr/001-native-rust-core-thin-webview.md:42`
- `docs/adr/003-trait-injected-dependencies.md:43`

**Other inaccuracies:**

- The README's timeout list omits `STT_CONNECT = 5 s` (`session/mod.rs:136`), which SPEC §3
  and ADR 012 do mention.
- SECURITY.md says the URL guard rejects "quotes" (plural); the code rejects only `"`
  (`window.rs:215`).
- `src-tauri/Cargo.toml:8` still reads `authors = ["you"]`.

**Setup gaps found by installing on a genuinely clean machine:**

- **npm 11 blocks esbuild's postinstall script by default**, and no document mentions it. A
  fresh `npm install` leaves esbuild non-functional until `npm approve-scripts esbuild` is
  run. During this review that produced a **version-pinned** entry in `package.json`
  (`"allowScripts": {"esbuild@0.25.12": true}`) — which will silently stop applying the
  moment the lockfile's esbuild version bumps. This belongs in DEVELOPMENT.md's "build
  gotchas" section, which otherwise records exactly this class of lesson.
- **VS Build Tools is a multi-gigabyte install** and dominates setup time. Worth a warning
  next to the prerequisite.
- Otherwise complete: Node 20+ (24 works), Rust stable (MSRV 1.77, correctly documented),
  Build Tools, WebView2.

---

## Build verification

Performed from nothing on Windows 11:

| Step | Result |
|---|---|
| Node 24.19.0 / npm 11.17 | installed |
| `npm install` | required `npm approve-scripts esbuild` (undocumented) |
| `npm run typecheck` | clean |
| `npm test` | 293 passed |
| `npm run build` | 171 KB JS / 7 KB CSS |
| Vite dev server | serves on the fixed port 5173 Tauri expects |
| Rust stable MSVC + VS Build Tools | installed |
| `cargo build -p app-core` | clean, 2m44s |
| `cargo test -p app-core` | 242 passed |
| `cargo build` (shell) | clean, 17m26s → `aicallhelper.exe`, 19.5 MB |
| `cargo test` (shell crate) | 35 passed |

One working-tree change resulted from the build and should be reviewed before committing:
the `allowScripts` addition to `package.json`. (`src-tauri/gen/schemas/*.json` also show as
modified, but `git diff --numstat` reports no content change — the build rewrote them with
identical bytes and different line endings, which `core.autocrlf` normalises away.)

---

## Recommended order of work

1. **Fix the content-protection readback, and correct SECURITY.md.** Everything else is a
   quality issue; this one is a false promise about the feature the product exists for.
2. **Change both empty-response guards to test delivered text**, and handle Groq's
   mid-stream error frame.
3. **Report the STT drain timeout** instead of returning a silently truncated transcript.
4. **Wrap the two spawned tasks in `catch_unwind`** and emit `session:error` on panic.
5. **Give `finalizing` a floor** — a frontend watchdog, or make the state escapable.
6. **Apply `canItemInterruptParagraph` inside `parseList`**, and add the in-list regression
   test.
7. **Add one multi-threaded integration test** on a real runtime. It will not be
   deterministic, and that is the point — the current harness cannot see this bug class at
   all.
8. Remove `open_external`; add an error boundary; refresh the stale test counts; document
   the esbuild hurdle.

---

## A note on the product

The screen-share exclusion is unconditional and has no toggle: the window is hidden from
capture specifically so that someone watching a shared screen cannot see it. That is the
app's defining design choice, and it carries real risk for you — many employers' interview
policies prohibit undisclosed AI assistance, and discovery through any of several channels
(the black-rectangle artifact on legacy capture paths, a proctoring agent, a second camera,
or a policy audit) could cost a candidacy or an offer.

That is your call to make, and this review takes no position on it. It is worth stating
only because it sharpens why the HIGH finding is the HIGH finding: the entire value of the
feature rests on it working, and today it can fail without telling you.

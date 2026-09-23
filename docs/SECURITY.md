# Security

An honest threat model for a single-user tool. This app runs on one person's
machine, holds that person's own API keys and call profiles, and talks to
three cloud origins that person chose (Deepgram, Anthropic, Groq) — or, only
while **Free local voice** is selected, to two loopback origins on the same
machine (Ollama on `127.0.0.1:11434`, the Moonshine speech service on
`127.0.0.1:8765`) that carry no key material at all. There is no server of
ours, no telemetry, no other users. The interesting questions are what sits
where, which inputs are untrusted, and what the mitigations genuinely do and
do not cover.

---

## Assets

All local. Nothing is stored anywhere but this machine.

| Asset | Where it lives | At rest |
|---|---|---|
| API keys (Deepgram, Anthropic, Groq) | `%APPDATA%\com.aicallhelper.app\settings.json` | DPAPI-encrypted, `enc:<base64>` (`core/src/store/secrets.rs:22-32`) |
| Call profiles — up to 8 × {name, call type, resume, job description / call context, focus, extra instructions} | same settings file, `profiles[]` | plaintext JSON (`core/src/store/settings.rs:408-420`) — see residual risks. The v3 top-level `resume`/`jobDescription` are read once for migration and never written again |
| Transcripts, answers, history | process memory only; the 6-entry history is React state (`src/state/reducer.ts`) and is never written to disk | n/a |
| Window geometry, launch placement, stream-follow preference | same settings file | plaintext, cosmetic |
| crash.log | same directory | timestamp + panic location + developer-authored message only (`src-tauri/src/logging.rs:3-7`) |
| `local-voice.json` (free local voice only) | always `%LOCALAPPDATA%\AI Call Assistant\local-voice.json` (`src-tauri/src/local_voice.rs:228-232`) | plaintext `{ "dataDir": "<absolute path>" }`, written by `scripts/setup-free-voice.ps1` (`local_voice.rs:103-121`). The *file* always lives under LocalAppData; the *folder it names* does not have to — setup's `-DataDir` accepts any absolute path (another drive, a shared folder), and that folder is where the app will *run executables from* — see the trust table and "The one process-launching command" below |
| Free-voice data folder (free local voice only) | `dataDir` from `local-voice.json`; default `%LOCALAPPDATA%\AI Call Assistant\local-voice` | the Python venv, `server.py`, the portable Ollama runtime, the Moonshine and Qwen model files — all written by the setup script, none by the app |
| `ollama.log`, `speech.log` (free local voice only) | the install folder from `local-voice.json` | the two services' stderr, appended by the shell when it launches them (`local_voice.rs:240-264`). The speech service does not write audio or transcripts to its log ([FREE_VOICE_MODE.md](FREE_VOICE_MODE.md)); nothing of ours reads either file |

---

## Trust boundaries

| Boundary | Trust level | What enforces it |
|---|---|---|
| `settings.json` | **User-writable, untrusted.** It survives upgrades and hand edits. | Per-field validation with individual fallback — one corrupt value never costs the profiles or the keys. A file that was read but holds no JSON object is first renamed to `settings.json.corrupt-<unix-seconds>` (never over an existing backup), and then the app starts from defaults. A file that cannot be read, or cannot be preserved, is never overwritten: the app runs on defaults and refuses to save until restart (ADR 016). Never a crash. The rule holds *inside* each profile (a bad `callType` reads as interview, a bad string as empty; only a non-object entry is dropped, `settings.rs:364-380`), and a `profiles` value that is not an array — a v3 file, or garbage — loads as one Default profile with the keys intact. Everything then passes one pure normaliser that caps lengths by characters and repairs ids (`settings.rs:186-253`). Secrets decode by stored prefix and **fail closed**: anything undecryptable or unknown-prefix reads as *unset*, never handed to a provider as a key (`core/src/store/secrets.rs:36-49`). |
| `local-voice.json` and the folder it names (free local voice only) | **User-writable, trusted only as far as your own account is.** The file names a folder the shell spawns binaries from, and both the file and (by default) the folder are writable by any process running as you. | Parsed by a pure function that strips PowerShell's UTF-8 BOM and **refuses a relative `dataDir`**: every executable and log is resolved under that folder, and "relative to whatever the current directory happens to be" is not a place to run `ollama.exe` from (`src-tauri/src/local_voice.rs:109-121`, pinned by test). The launch specs are pure too (`launch_spec`, `local_voice.rs:162`), so what gets spawned with which arguments is unit-tested without spawning anything. Nothing checks *what* the executables are: no signature, hash or location allowlist. See "The one process-launching command" below for what that means. |
| Local services (Ollama, Moonshine) — free local voice only | **Separate processes, loopback only.** They receive the active profile and the transcript; they hold no keys. | Both bind `127.0.0.1`; the app dials `http://127.0.0.1:11434` through a client with `no_proxy()` so a system proxy can never see loopback traffic (`core/src/llm/local.rs:26-36`) and `ws://127.0.0.1:8765` for speech (`core/src/stt/local.rs:16`); the speech service rejects browser `Origin` headers. The app never opens either port to the LAN. Their responses are untrusted bytes like any provider's: NDJSON frames are size-capped and JSON-parsed, an error frame maps to a fixed message rather than echoing service internals (`local.rs:104-108`), and `thinking` text is never emitted. |
| Model output (answer text) | **Untrusted.** It is rendered, copied, and read aloud. | In-repo markdown subset; every string reaches the DOM as a text node; links are not parsed at all (`src/markdown/parse.ts:9-11`, `src/markdown/Markdown.tsx:6-14`). See below. |
| Deepgram / LLM wire | **Untrusted bytes.** | TLS via compiled-in rustls (`core/src/llm/http.rs:38-42`, `tokio-tungstenite` with rustls, `core/Cargo.toml:11-13`). Frame parsing never panics — malformed and pathologically nested JSON is ignored (`core/src/stt/frame.rs` tests); error bodies are truncated to a 300-char snippet before display (`core/src/llm/groq.rs:282-296`); provider failures map into a closed error-code set rather than raw exception text (`core/src/error.rs:1-6`). |
| Webview | **Sandboxed by CSP + navigation policy.** | CSP: `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self' ipc: http://ipc.localhost` (`src-tauri/tauri.conf.json`) — no remote script, no remote fetch, no inline anything. The window never navigates: only the bundled origin and the dev server classify as internal; https bounces to the default browser; everything else (`file:`, `javascript:`, `http:`) is silently dropped (`src-tauri/src/window.rs:255-301`). |
| UI ↔ core (IPC) | Key material crosses in **one direction only**; scalar settings cross as **closed enums**. | The frontend can *write* keys but never read them: `SettingsView` carries `hasDeepgramKey`-style booleans and zero key bytes (`core/src/store/mod.rs:234-250`; pinned by `view_never_exposes_key_material`). Key inputs are password fields whose value is always empty. `llmProvider`, `answerStyle`, `launchPlacement` and `streamFollow` are typed on the Rust side of `SettingsPatch` (`store/mod.rs:302-316`): a value outside the set fails argument deserialization and the invoke rejects — the bridge folds that into `{ code: "internal" }` (`src/bridge.ts:41-50`). Lenient parsing is for the settings *file* only. Profile text is user-typed grounding data, same trust class as the resume always was; it reaches the webview only because the user entered it there. |

---

## Mitigations as implemented

**DPAPI at rest, fail-closed decode.** Keys are encrypted with
`CryptProtectData` bound to the current Windows user and stored as
`enc:<base64>` (`core/src/store/secrets.rs:51-88`). If the keystore is
unavailable, the fallback is a *marked* `plain:<base64>` — honestly labeled
rather than silently pretending to be encrypted (`secrets.rs:5-9,27-32`).
Decoding dispatches on the stored prefix, never on current keystore
availability, and everything undecryptable — a settings file copied from
another machine, mangled base64, an unknown prefix, a raw key pasted by hand
— reads as **unset** (`secrets.rs:36-49`). Failing closed beats handing
ciphertext to a provider, which produces a baffling auth error mid-call
instead of a clear "add your key" nudge.

**Atomic settings writes.** The file that holds the keys is written
temp-then-rename with a full flush before the rename
(`core/src/store/settings.rs:152-170`), so a crash or full disk mid-write
cannot truncate it into "defaults" — which would silently destroy every
setting including the keys. Memory is updated only after the write lands.
The write now runs on tokio's blocking pool rather than the event-loop
thread (`src-tauri/src/commands.rs:79-102`); the settings mutex is taken and
released entirely inside that task, so the guarantee is unchanged and no
lock is ever held across an await.

**Write-only key boundary.** See the table row above. Additionally, keys are
never written to disk in plaintext (pinned by
`keys_are_never_written_to_disk_in_plaintext`), cleared keys are *omitted*
from the file rather than written empty (`settings.rs:436-444`), and the
pre-warm request is deliberately unauthenticated — the key has no business
traveling on a fire-and-forget request whose response nobody reads
(`core/src/llm/warm.rs:69-74`). Free local voice needs no key and refuses
none: `needs_cloud_keys()`/`uses_deepgram()` are false for it, so a user can
run it with nothing stored, and selecting it does not erase saved cloud keys
(`src-tauri/src/commands.rs:390-409`).

**No inline allowance in the shipped CSP.** §10 pins `style-src 'self'`,
adjusted minimally if Tauri demands it; nothing did. The one dynamic style in
the app — the level meter's width — is a React `style` prop, which React
applies through the CSSOM (`element.style`) rather than as a style attribute
in markup, and `style-src` does not gate CSSOM writes. So the allowance would
have bought nothing and is gone (`src/components/LevelMeter.tsx`).
`index.html` keeps `'unsafe-inline'` in its `<meta>` because Vite injects the
stylesheet inline during `npm run dev`; a production build extracts CSS to a
linked file instead. That meta tag is copied verbatim into the bundle, but
each delivered policy is enforced independently, so the effective production
policy is the intersection — the stricter `'self'` from the Tauri header.

**Text-node-only rendering.** Model output renders through an in-repo
markdown subset with no library and no sanitizer to misconfigure. Every
string reaches the DOM as a text node; there is no `dangerouslySetInnerHTML`
and no attribute is ever derived from model text — code-fence info strings
are dropped rather than becoming a class (`src/markdown/Markdown.tsx:1-14`).
Links are deliberately **not parsed**: `[text](url)` stays literal visible
text, so there is no href to sanitize and no `javascript:` to smuggle
(`src/markdown/parse.ts:9-11`). Model headings are demoted (`#` → `h3`,
capped at `h6`) so model output can never outrank app chrome. The XSS suite
(`src/markdown/xss.test.tsx`) pins all of this.

**https-only external links, single-argv open.** The only way out of the app
is the window builder's `on_navigation` hook — `window::handle_navigation`
(`src-tauri/src/window.rs:291-301`): a navigation to an in-app origin is
allowed, an `https://` URL is bounced to the default browser, and everything
else (`file:`, `javascript:`, `http:`) is silently dropped. There is no
command for opening links any more: `open_external` was dead surface (the
hook already covered every real navigation) and was removed. That leaves
exactly one IPC command that starts processes, `prepare_local_voice`,
described next; no command opens a URL or runs anything the webview names.
The bounce goes through `is_safe_external_url`, which
refuses anything that is not `https://` and additionally any URL containing
whitespace, control characters, or quotes — the characters that make one
argument stop being one argument (`window.rs:277-289`, pinned by test). The
URL is handed to `explorer.exe` as a single argv entry, never through
`cmd.exe`, so it cannot be split, quoted apart, or chained
(`open_in_browser`, `window.rs:303-316`).

**The one process-launching command.** `prepare_local_voice`
(`src-tauri/src/local_voice.rs:266-299`, registered in
`src-tauri/src/lib.rs:150`) is what **Start and warm free mode** in Settings
invokes. It is deliberately exposed and deliberately narrow:

- **No arguments cross IPC.** The webview can only ask "start whatever is
  missing"; it cannot name an executable, an argument or a folder.
- **It spawns only what is not already answering.** It first probes
  `http://127.0.0.1:11434/api/tags` and `http://127.0.0.1:8765/health`;
  only a service that does not answer is launched (`launch_plan`), so with
  both services up it starts nothing.
- **What it spawns, and from where.** The folder is `dataDir` from
  `%LOCALAPPDATA%\AI Call Assistant\local-voice.json`, which must be an
  absolute path but can be *any* absolute path — the executables need not
  live under LocalAppData. Under that folder it runs at most two programs:
  `<dataDir>\ollama\ollama.exe serve` with `OLLAMA_HOST=127.0.0.1:11434`,
  `OLLAMA_NO_CLOUD=1` and `OLLAMA_MODELS=<dataDir>\models\ollama`; and
  `<dataDir>\venv\Scripts\python.exe -u <dataDir>\server.py --home
  <dataDir>`. Each runs with the folder as its working directory, stdin and
  stdout discarded, stderr appended to `ollama.log` / `speech.log` in the
  folder, and `CREATE_NO_WINDOW` (`local_voice.rs:162-264`). Ollama itself
  may start its own model-runner child processes.
- **Serialized.** A `try_lock` on a static mutex refuses a second concurrent
  call with "Free mode is already starting. Please wait."
  (`local_voice.rs:272-275`).
- **Started, never stopped.** The spawned services run as your user,
  outlive the app, and are not stopped by closing or uninstalling it; stop
  them in Task Manager (see [FREE_VOICE_MODE.md](FREE_VOICE_MODE.md)).

**The trust boundary is your Windows account.** Anything running as you can
rewrite `local-voice.json` to point at another folder, or replace
`ollama.exe`, `python.exe` or `server.py` inside the configured folder, and
the next *Start and warm* will run it as you. The app does not defend
against that and does not claim to: code already running in your account can
run programs as you directly, and can decrypt your DPAPI-protected keys the
same way (see "Explicitly out of scope"). This is the same boundary the key
storage relies on, not a stronger one. Two consequences worth acting on: if
you choose a `-DataDir` outside your profile, keep it a folder only your
account can write to; and treat a `local-voice.json` you did not create as
suspect.

**No sensitive logging.** Nothing logs keys, profile text, transcripts, or
answers — the crash log carries a timestamp, a source location, and a
developer-authored panic message (`src-tauri/src/logging.rs:3-7`). The one
provider-side number kept for diagnostics, `cache_read_input_tokens`, is
parsed and exposed on a getter but read only by tests — it reaches no log and
no UI (`core/src/llm/anthropic.rs`). It is a count, so it would be safe
to surface; the prompt it describes never would be. The startup stage
timings added for profiling are `#[cfg(debug_assertions)]` `eprintln!`
lines carrying only a stage name and a duration, compiled to a no-op in
release and never written to crash.log (`src-tauri/src/lib.rs:28-34`).

**Capture exclusion is requested, then verified at launch.** The window calls
`set_content_protected(true)` unconditionally, with no toggle, before it is
ever shown (`src-tauri/src/lib.rs:125`); on Windows that requests display
affinity `WDA_EXCLUDEFROMCAPTURE` through
[`SetWindowDisplayAffinity`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity).
The window is created hidden and shown only after geometry, the launch-time
dock and that request (`lib.rs:104-135`). Because tao 0.35 discards the Windows call's own result, the shell reads the affinity back with `GetWindowDisplayAffinity` and fails setup unless it is `WDA_EXCLUDEFROMCAPTURE`, so a refusal by Windows stops the launch. Limit:
`WDA_EXCLUDEFROMCAPTURE` is supported only on Windows 10 version 2004 or
later. The app requests Windows capture exclusion. Its effect depends on the
Windows version and capture method. Check the recorded compatibility results
([TROUBLESHOOTING.md](TROUBLESHOOTING.md#tested-sharing-configurations)) and
test your intended sharing setup before relying on it.

---

## Explicitly out of scope

- **Multi-user machines.** DPAPI binds keys to the Windows user account; the
  model assumes the account is yours alone. Anyone who can log in *as you*
  is you.
- **Malware running in the same account.** Same-user code can rewrite
  `local-voice.json` or the binaries in the free-voice folder and have
  *Start and warm* run them (see "The one process-launching command"), and it
  can simply call
  `CryptUnprotectData` on the stored blobs — DPAPI's per-user binding is
  exactly what it claims and nothing more (`core/src/store/secrets.rs:101-103`).
  What DPAPI actually buys here: the settings file is worthless when copied
  off the machine, synced to a backup, or read by another user account. It is
  not a defense against a keylogger you already have.
- **Capture exclusion is anti-embarrassment, not anti-forensics.** Microsoft
  describes display affinity as protection against a specific set of public
  capture APIs, not a guarantee; which conferencing and recording tools it
  covers is a matter of recorded tests, and none has been recorded yet
  ([tested sharing configurations](TROUBLESHOOTING.md#tested-sharing-configurations)).
  It does not hide the process from Task Manager, the window from
  `EnumWindows`, or the screen from a phone camera, and an endpoint agent
  with kernel or driver-level capture is outside this mechanism entirely.
  The feature exists to keep your copilot out of a shared screen where
  Windows honors the request — not to defeat an investigation.
- **A hostile network doing more than denying service.** TLS with pinned-in
  rustls roots is the transport story; certificate pinning per provider is
  not attempted.
- **The local services' own attack surface.** Ollama and the Moonshine
  service are third-party processes with their own code, listening on
  loopback. Any process running as you can talk to them, and they can be
  told to load whatever their own settings say. The app treats their output
  as untrusted bytes and never exposes them beyond `127.0.0.1`; hardening
  the services themselves is their job, not this app's.

---

## Residual risks, stated plainly

- **Keys live in process memory as plain `String`s** for the lifetime of a
  provider instance (`core/src/llm/anthropic.rs:40-41`,
  `core/src/stt/deepgram.rs:54-56`). They are not zeroized on drop and can
  appear in a process dump. Accepted: same-account malware is out of scope,
  and a dump requires exactly that access.
- **The `plain:` fallback is base64, not encryption.** On a machine where
  DPAPI is unavailable, keys land on disk merely encoded — deliberately
  *marked* as such rather than faking safety (`secrets.rs:5-9`). On any
  normal Windows install this path never runs (pinned by
  `on_windows_protect_uses_dpapi_not_the_fallback`).
- **Call profiles are plaintext on disk.** Only keys are encrypted. Every
  profile — resume, job description or call context, focus, extra
  instructions, up to eight of them — is readable by anything running as
  you: the same class of data as the documents it was pasted from, now
  possibly covering several concurrent job searches or accounts in one file.
- **The active profile and the transcript are sent to the provider by
  design.** Every answer request carries the system prompt (the whole
  active profile) and the transcript to Anthropic or Groq; every recording
  streams call audio to Deepgram. That *is* the product. What those
  providers retain is governed by their terms, not by anything this app can
  enforce. The one lever the app has, it uses: no telemetry of ours exists,
  and nothing is sent anywhere except the provider you configured. In free
  local voice mode the same data goes to the two loopback services instead
  and never leaves the machine (the one-time model download during setup
  excepted).
- **Extra instructions can contradict the app's own role text.** A profile's
  "extra instructions" ride inside the cached prefix after the role
  instructions, so the user can steer or override how answers are phrased.
  That is the user's prerogative over their own answers, not a boundary
  crossing: the text is theirs, typed by them, and it never reaches anything
  but the provider they chose.
- **The Deepgram key rides a WebSocket header** (the
  `Sec-WebSocket-Protocol: token, <key>` subprotocol,
  `core/src/stt/deepgram.rs:108-119`) — inside TLS, but it is the documented
  Deepgram auth mechanism, not a choice this app can revisit.
- **The no-sensitive-logging rule is a policy, not a mechanism.** It holds
  because no code path formats user data into a panic or log message
  (`src-tauri/src/logging.rs:5-7`); a future change could violate it
  silently. Review anything that adds a `panic!`/`format!` near user data
  with this in mind.

# Security

An honest threat model for a single-user tool. This app runs on one person's
machine, holds that person's own API keys and resume, and talks to three
providers that person chose. There is no server of ours, no telemetry, no
other users. The interesting questions are what sits where, which inputs are
untrusted, and what the mitigations genuinely do and do not cover.

---

## Assets

All local. Nothing is stored anywhere but this machine.

| Asset | Where it lives | At rest |
|---|---|---|
| API keys (Deepgram, Anthropic, Groq) | `%APPDATA%\com.aicallhelper.app\settings.json` | DPAPI-encrypted, `enc:<base64>` (`core/src/store/secrets.rs:22-32`) |
| Resume + job description | same settings file | plaintext JSON (`core/src/store/settings.rs:236-237`) — see residual risks |
| Transcripts, answers, history | process memory only; the 6-entry history is React state (`src/state/reducer.ts`) and is never written to disk | n/a |
| Window geometry | same settings file | plaintext, cosmetic |
| crash.log | same directory | timestamp + panic location + developer-authored message only (`src-tauri/src/logging.rs:3-7`) |

---

## Trust boundaries

| Boundary | Trust level | What enforces it |
|---|---|---|
| `settings.json` | **User-writable, untrusted.** It survives upgrades and hand edits. | Per-field validation with individual fallback — one corrupt value never costs the resume or the keys; an unparseable file loads as defaults, never a crash (`core/src/store/settings.rs:171-232`). Secrets decode by stored prefix and **fail closed**: anything undecryptable or unknown-prefix reads as *unset*, never handed to a provider as a key (`core/src/store/secrets.rs:36-49`). |
| Model output (answer text) | **Untrusted.** It is rendered, copied, and read aloud. | In-repo markdown subset; every string reaches the DOM as a text node; links are not parsed at all (`src/markdown/parse.ts:9-11`, `src/markdown/Markdown.tsx:6-14`). See below. |
| Deepgram / LLM wire | **Untrusted bytes.** | TLS via compiled-in rustls (`core/src/llm/http.rs:38-42`, `tokio-tungstenite` with rustls, `core/Cargo.toml:11-13`). Frame parsing never panics — malformed and pathologically nested JSON is ignored (`core/src/stt/frame.rs` tests); error bodies are truncated to a 300-char snippet before display (`core/src/llm/groq.rs:282-296`); provider failures map into a closed error-code set rather than raw exception text (`core/src/error.rs:1-6`). |
| Webview | **Sandboxed by CSP + navigation policy.** | CSP: `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self' ipc: http://ipc.localhost` (`src-tauri/tauri.conf.json:15`) — no remote script, no remote fetch, no inline anything. The window never navigates: only the bundled origin and the dev server classify as internal; https bounces to the default browser; everything else (`file:`, `javascript:`, `http:`) is silently dropped (`src-tauri/src/window.rs:191-228`). |
| UI ↔ core (IPC) | Key material crosses in **one direction only**. | The frontend can *write* keys but never read them: `SettingsView` carries `hasDeepgramKey`-style booleans and zero key bytes (`core/src/store/settings.rs:59-66`; pinned by `view_never_exposes_key_material`). Key inputs are password fields whose value is always empty. |

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
(`core/src/store/settings.rs:123-138`), so a crash or full disk mid-write
cannot truncate it into "defaults" — which would silently destroy every
setting including the keys. Memory is updated only after the write lands.

**Write-only key boundary.** See the table row above. Additionally, keys are
never written to disk in plaintext (pinned by
`keys_are_never_written_to_disk_in_plaintext`), cleared keys are *omitted*
from the file rather than written empty (`settings.rs:242-252`), and the
pre-warm request is deliberately unauthenticated — the key has no business
traveling on a fire-and-forget request whose response nobody reads
(`core/src/llm/warm.rs:70-74`).

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
is `open_external`, which refuses anything that is not `https://`
(`src-tauri/src/commands.rs:283-289`) and additionally rejects any URL
containing whitespace, control characters, or quotes — the characters that
make one argument stop being one argument (`src-tauri/src/window.rs:207-216`).
The URL is handed to `explorer.exe` as a single argv entry, never through
`cmd.exe`, so it cannot be split, quoted apart, or chained
(`window.rs:230-243`). The same guard backs the navigation hook.

**No sensitive logging.** Nothing logs keys, resume text, transcripts, or
answers — the crash log carries a timestamp, a source location, and a
developer-authored panic message (`src-tauri/src/logging.rs:3-7`). The one
provider-side number kept for diagnostics, `cache_read_input_tokens`, is
parsed and exposed on a getter but read only by tests — it reaches no log and
no UI (`core/src/llm/anthropic.rs:42-52`). It is a count, so it would be safe
to surface; the prompt it describes never would be.

**Content protection, always on.** The window sets
`set_content_protected(true)` unconditionally, with no toggle, before it is
ever shown — and if the OS refuses, the launch fails rather than silently
breaking the promise (`src-tauri/src/lib.rs:93-97`). The window is created
hidden and shown only after geometry and protection are applied, so no
unprotected frame ever exists (`lib.rs:85-104`).

---

## Explicitly out of scope

- **Multi-user machines.** DPAPI binds keys to the Windows user account; the
  model assumes the account is yours alone. Anyone who can log in *as you*
  is you.
- **Malware running in the same account.** Same-user code can simply call
  `CryptUnprotectData` on the stored blobs — DPAPI's per-user binding is
  exactly what it claims and nothing more (`core/src/store/secrets.rs:101-103`).
  What DPAPI actually buys here: the settings file is worthless when copied
  off the machine, synced to a backup, or read by another user account. It is
  not a defense against a keylogger you already have.
- **Screen-share invisibility is anti-embarrassment, not anti-forensics.**
  Content protection keeps the window out of Zoom/Teams/Meet screen shares
  and standard capture APIs. It does not hide the process from Task Manager,
  the window from `EnumWindows`, or the screen from a phone camera, and an
  endpoint agent with kernel or driver-level capture is outside this
  mechanism entirely. The feature exists so a shared screen doesn't show your
  copilot — not to defeat an investigation.
- **A hostile network doing more than denying service.** TLS with pinned-in
  rustls roots is the transport story; certificate pinning per provider is
  not attempted.

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
- **Resume and job description are plaintext on disk.** Only keys are
  encrypted. The profile is readable by anything running as you — the same
  class of data as the documents it was pasted from.
- **Transcript, resume, and JD are sent to the providers by design.** Every
  answer request carries the system prompt (resume + JD) and the transcript
  to Anthropic or Groq; every recording streams call audio to Deepgram. That
  *is* the product. What those providers retain is governed by their terms,
  not by anything this app can enforce. The one lever the app has, it uses:
  no telemetry of ours exists, and nothing is sent anywhere except the
  provider you configured.
- **The Deepgram key rides a WebSocket header** (the
  `Sec-WebSocket-Protocol: token, <key>` subprotocol,
  `core/src/stt/deepgram.rs:108-119`) — inside TLS, but it is the documented
  Deepgram auth mechanism, not a choice this app can revisit.
- **The no-sensitive-logging rule is a policy, not a mechanism.** It holds
  because no code path formats user data into a panic or log message
  (`src-tauri/src/logging.rs:5-7`); a future change could violate it
  silently. Review anything that adds a `panic!`/`format!` near user data
  with this in mind.

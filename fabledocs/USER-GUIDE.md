# AI Call Assistant v3.1 — setup, deployment and user guide

This guide covers three things: getting the app onto a Windows machine
(build or installer), the one-time setup, and how to use it on a live call.
For the engineering details behind each feature see `REPORT.md`; for the
product contract see `docs/SPEC.md`.

*Last revised 2026-09-23 (answer status tags, capture exclusion verified at
launch, the Settings save lock and conflict reload, damaged settings files,
the local prompt budget readout, and matching troubleshooting rows).
Revised before that on 2026-09-22 (screen-sharing wording, local-mode figures
and the installed-app local-mode section, per the 2026-09-19 reviews). What has been
validated on real hardware is recorded only in the results ledger of
[`docs/RELEASE_CHECKLIST.md`](../docs/RELEASE_CHECKLIST.md).*

## 1. Install or build

### Prerequisites for installing a built installer

- Windows 10 or 11, 64-bit. Windows 10 version 2004 or later is required:
  the app checks at launch that Windows applied screen-capture exclusion to
  its window and does not start if it did not (see §3).
- WebView2 runtime (already present on current Windows 11).
- No administrator rights: the installer is per-user.
- Free local voice additionally needs a source checkout and Python; see §8.

### Prerequisites for building from source

- Rust stable (https://rustup.rs)
- Visual Studio Build Tools with the "Desktop development with C++" workload
- Node.js 20 or newer (CI uses Node 22)
- WebView2 runtime (already present on current Windows 11)
- Disk: a debug build of the Tauri shell needs roughly 4–6 GB free under
  `src-tauri/target`; a release build about 2–3 GB. The audit machine had
  under 1 GB free at times, which is why the shell build could not be run
  there (see `REPORT.md`). Free space first.

### Run from source

```powershell
npm install
npm run tauri dev
```

The first `tauri dev` compiles the Rust shell (10–20 minutes cold); later
runs are incremental. The window opens docked to the top-centre of your
display.

### Build the installer

```powershell
npm run tauri build
```

This runs the frontend type-check and Vite build, then a release build of the
shell, and writes a per-user NSIS installer to
`src-tauri\target\release\bundle\nsis\AI Call Assistant_3.1.0_x64-setup.exe`.
Double-click it to install; it needs no administrator rights. Uninstall from
Windows "Apps & features" (free local voice, if you set it up, is removed
separately; see §8).

### Checks before shipping a build

Follow [`docs/RELEASE_CHECKLIST.md`](../docs/RELEASE_CHECKLIST.md): the
deterministic gates (frontend typecheck, tests and build; Rust core tests and
clippy; Tauri shell tests; the local speech service's Python tests), the
packaged NSIS build, and the manual checks on the installed app (real audio,
local inference, cancellation, hotkey, DPI and monitors, screen sharing).
The deterministic gates also run in CI on every push
(`.github/workflows/ci.yml`). Record each run, with the installer's SHA-256,
in the checklist's results ledger; a build is not validated until it has a
row there.

### Where the app keeps its data

`%APPDATA%\com.aicallhelper.app\` holds `settings.json` (profiles, options,
window bounds, and API keys encrypted with Windows DPAPI) and `crash.log`
(panics only; never keys, transcripts or profile text). Deleting
`settings.json` resets the app to first run. A settings file copied from
another Windows account loads with its keys unset, by design.

If `settings.json` is damaged (not valid JSON, for example after a hand edit
gone wrong), the app does not overwrite it. It renames it to
`settings.json.corrupt-<seconds>` in the same folder (a number is appended
if that name is taken, so an older backup is never replaced) and starts with
default settings. A note explaining this appears in the error box at launch
and at the top of Settings. Copy anything you need (profile text) out of the
backup, set the app up again, and Save; the first successful save clears the
note. Your API keys have to be re-entered. If the file could not be read at
all (another program had it locked) or the backup copy could not be made,
the note says the app **will not save changes**: nothing you change,
including window position, is written, so the original file is left intact.
Close whatever has `settings.json` open (an editor, a sync or backup tool,
an antivirus scan), or move a damaged file out of the folder, then restart
the app.

## 2. First-time setup

1. Launch the app. The status line reads "First run: open Settings (gear
   icon) and add your API keys".
2. Click the gear (or press Escape later to leave Settings).
3. **Keys & model.** Pick the answer model:
   - *Claude Haiku 4.5 (recommended)* — needs a Deepgram key (speech to
     text) and an Anthropic key.
   - *Groq GPT-OSS 120B (fastest)* — needs Deepgram and Groq keys.
   - *Free local voice (Qwen3.5 2B + Moonshine)* — no keys and no per-minute
     fees; English only; slower answers on a CPU. Run
     `powershell -ExecutionPolicy Bypass -File .\scripts\setup-free-voice.ps1`
     once from the project folder, then use "Start and warm free mode" in
     Settings. Setup requires at least 4 GB free on the target drive and
     recommends 8 GB; that is free space, not download size. The announced
     downloads are about 1.5 GB (Ollama runtime, deleted after extraction)
     plus about 2.7 GB (the Qwen3.5 2B model), plus Python packages and the
     speech model, whose size is measured on first setup. See §8 and
     `docs/FREE_VOICE_MODE.md`.
   Only the key fields the chosen model needs are shown. Keys are never
   displayed again; a saved key shows "saved — type to replace".
4. **Profile.** Fill in the Default profile (see §4) — at minimum paste your
   resume and the job description or call context.
5. **Shortcut & window.** Leave the defaults unless you have a reason to
   change them (see §5).
6. Click **Save**, then Back. While a save is in progress the button reads
   "Saving…" and the whole form, including Back and Escape, is locked; it
   unlocks when the save finishes ("Saved ✓"). If a save fails, the form
   unlocks with your edits still in it, so you can try again.

**"Settings changed elsewhere — reload."** If a profile or answer-style
chip on the main view was still saving when you opened Settings, the form
may be based on older settings than the ones now saved. Save is then
disabled and Settings shows "Settings changed elsewhere — reload. Your
unsaved edits are kept." Nothing was saved. Press **Reload**: fields you
edited keep your text, fields you did not touch take the saved values, and
"Reloaded. Your unsaved edits were kept; review them and Save." appears.
Check the form and Save. Moving or resizing the window never causes this.

Test before a real call: play any speech through your speakers, press
Record, watch the transcript appear while the audio is still playing, press
Stop & Answer, and check that an answer streams in with a latency chip like
"1.2s to first word".

## 3. On a call

The window stays above your call window (always-on-top is on by default),
so keep it under your webcam and read from it.

**Screen sharing.** The app requests Windows capture exclusion
(`WDA_EXCLUDEFROMCAPTURE`) for its window and then reads the setting back
from Windows before the window is first shown. If Windows did not apply it,
the app does not start: the window never opens, and `crash.log` records
"Windows did not apply screen-capture exclusion to the window, so the app
will not start. This needs Windows 10 version 2004 or later." Settings
repeats the point: "Windows capture exclusion was verified at launch; test
your sharing app before relying on it."

Verified at launch means Windows accepted the setting, not that every
sharing app honours it. Microsoft documents the setting as covering a
specific set of capture APIs rather than every capture method
([SetWindowDisplayAffinity](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity)),
and a hardware capture device or a phone camera is never covered. Whether a
given conferencing app and capture mode leave the window out is known only
from recorded tests, listed under
[tested sharing configurations](../docs/TROUBLESHOOTING.md#tested-sharing-configurations);
at the time of writing there are none. Before a call that matters, share your
screen the way you will on the call and check the shared view from a second
device.

1. **When the other person starts asking a question**, press the global
   shortcut (default **Ctrl+Shift+Space**) from any window, or click
   **Record**. The app captures what plays through your speakers or
   headphones (system audio), not your microphone. The status dot pulses red,
   a level meter and timer appear, and the "Question heard" panel shows the
   live transcript.
2. **When they finish**, press the shortcut again or click **Stop & Answer**.
   The suggested answer streams into the top panel within about a second.
3. **Read and speak.** The answer panel is the largest element and sits
   directly under the header, which the docked window puts directly under
   the camera. "Question heard" collapses to a one-line caption once the
   recording ends; click it to expand.
4. **Next question:** press the shortcut again. Recording over a streaming
   answer supersedes it.

Other controls:

- **Type a question instead…** — the Ask box sends a typed question; it is
  disabled while recording. **Interview prep** (below it, interview profiles
  only) opens a library of practice questions you can stage into the box.
- **Brief / Balanced / Detailed** — answer length. The lit chip is the saved
  style; changing it never slows the next answer.
- **Regenerate / Copy** in the answer panel head. Copy puts the markdown
  source on the clipboard so bullets survive pasting.
- **History** (‹ n/m ›, also in the answer head) appears after two answers;
  **Clear** is enabled when idle.
- A recording stops itself at 120 seconds and answers normally.

**Answer status tags.** An answer that finished normally has no tag. An
answer that did not shows a small tag in the answer panel head and a
one-line caption under the answer. The tag stays with that answer in
History, even after the next question clears the error box.

| Tag | Meaning | What to do |
|---|---|---|
| **incomplete** | The answer stopped part-way: the connection dropped, the provider sent an error or a malformed response, a time limit expired, or the app hit an internal fault. The text shown is only what arrived before that; the caption gives the reason. | Do not read it as a finished answer; the rest is simply missing. Press **Regenerate** (or ask again) if you need the whole answer. If it keeps happening, look up the caption's message in §6 or `docs/TROUBLESHOOTING.md`. |
| **cut short** | The model reached its length limit. Caption: "Cut short: the answer reached its length limit." Everything shown is real, but the model had more to say. The latency chip still appears. | Usually fine to read as is. For an answer that fits, pick **Brief**, or ask a narrower question. |
| **stopped** | You replaced or cancelled the answer. Caption: "Stopped: a newer question replaced this one." or "Stopped before it finished." | Nothing; it is kept so History stays complete. |

## 4. Profiles: one per job, tech stack or kind of call

A profile is a self-contained grounding bundle. Exactly one is active, and
every answer is grounded in the active one. Use separate profiles when you
are interviewing for different roles (a React/TypeScript front-end role and
a Rust systems role), or when some calls are not interviews at all.

In Settings → **Profile**:

- **Profile** select — which profile you are editing, and which becomes
  active when you Save.
- **New** (up to 8), **Duplicate** (copies everything; the fastest way to
  make "same resume, new job description"), **Delete** (disabled with one
  profile).
- **Profile name** — what the chips on the main view show.
- **Call type** — Job interview, Sales call, Customer support call, Work
  meeting, Other. An interview profile behaves exactly like v3. The other
  types tell the model what kind of call it is and relabel the two text
  fields ("About you" and "Call context").
- **Focus (optional)** — what to emphasise, for example "Rust, tokio, async,
  low-latency audio". This is the tech-stack lever.
- **Resume / About you** and **Job description / Call context** — pasted
  verbatim; formatting is preserved. Each up to 200,000 characters.
- **Extra instructions (optional)** — anything else the model should follow
  for this profile.

Switching between calls: when two or more profiles exist, chips with their
names appear under the header on the main view. Click one to make it active;
nothing else changes. The next recording or typed question uses it. Switching
profiles is deliberately a between-calls action: it changes the cached part
of the prompt, so the first answer after a switch may cost a cache write with
a large profile, and nothing switches automatically.

If the active profile has neither a resume nor a job description, a hint
under the chips says so: answers will still come, but ungrounded.

Free local voice has a hard limit of 7,000 bytes for the whole request:
the app's fixed instructions, every field of the active profile, the answer
style and the question. With *Free local voice* selected, Settings shows,
under the profile, how much room the profile you are editing (unsaved
changes included) leaves for the question, computed by the same code that
enforces the limit:

- "N bytes left for the question in free local mode (U of 7,000 used by the
  instructions and this profile)." — fine.
- "Only N bytes left for the question in free local mode … Shorten this
  profile or use a cloud model." — a warning, shown under 200 bytes; a short
  question may still fit, a longer one will be refused.
- "Too long for free local mode: … so no question fits." — recording and Ask
  are refused in local mode with this profile. You can still save it (for
  example to use with a cloud model).

When a request does not fit, the app refuses it before anything starts: with
no room at all, pressing Record shows the error before recording begins; a
typed question that is too long is refused before it is sent and stays in
the box for you to shorten. The message is "Free local mode supports about
7 KB of combined instructions, profile (resume, job description, focus,
extra instructions) and question. Shorten the active profile in Settings or
use a cloud model." The cloud models have no such limit.

## 5. Window placement and reading modes

- **Dock to camera** (the ⬆ button in the header) moves the window to the
  top-centre of the display it is on, 8 px below the edge, and resizes it to
  a wide reading preset (about 600 logical px wide, 45 % of the screen
  height). Drag it anywhere afterwards; the position is remembered.
- **Window position at launch** (Settings → Shortcut & window):
  *Dock under the camera (top centre)* (default) re-docks on every launch,
  keeping your last size; *Remember where I left it* restores the last
  position instead. A window that would land off-screen (unplugged monitor)
  is docked rather than lost.
- **Focus mode** (the ◎ button in the header, or Ctrl+Shift+F inside the
  window) hides everything except the answer, the status line, errors and
  the Record button, and enlarges the answer text. Press again to leave.
- **While an answer streams** (Settings): *Follow the newest text* (default)
  keeps the panel scrolled to the newest line if you were already at the
  bottom; *Stay at the opening sentence (teleprompter)* leaves the panel at
  the top so you can start speaking while the model is still writing. A
  reader who scrolls up is never yanked back down in either mode.
- **Global shortcut**: Modifier+key, e.g. Ctrl+Shift+Space. It works while
  any app is focused and is ignored while Settings is open. Leave the field
  empty to turn the shortcut off. If another app already owns the combo, the
  main view says so and the shortcut is off until you pick another.
- **Keep this window always on top**: on by default; a docked window that is
  not on top drops behind the call app the moment you click it.

## 6. Troubleshooting (quick reference)

| Symptom | Cause and fix |
|---|---|
| "No speech detected in the recording" | Record captures system audio, not the mic. Make sure the call audio plays through the default output device. |
| "First run: open Settings…" although keys are saved | The selected model needs a key you have not saved (Groq preset needs a Groq key). |
| Answer arrives but is generic | The active profile has no resume or job description, or the wrong profile is active. Check the chips. |
| "Free local mode supports about 7 KB…" | The active profile plus the question does not fit the local 7,000-byte limit; nothing was sent. Check the byte readout in Settings (§4), shorten the profile (resume, job description, focus, extra instructions) or the typed question, or switch to a cloud model. |
| Answer tagged **incomplete** | It stopped part-way; the text is only what arrived. Press Regenerate or ask again. The caption says why (§3, Answer status tags). |
| Answer tagged **cut short** | The model reached its length limit; what is shown is real. Use Brief or ask a narrower question. |
| "The answer stopped unexpectedly. Try again." | An internal fault ended the answer; the app recovered and is ready for the next question. Try again. If it repeats, check `crash.log` in `%APPDATA%\com.aicallhelper.app\` and report it. |
| "Lost track of that answer before it finished. Try again." | The app lost its record of that answer before it finished. Ask again. |
| "The app could not start listening for session events. Restart the app." | Record or Ask could not start within 3 seconds because the window was not yet receiving the app's events. The next attempt retries; if it happens again, restart the app. |
| The window never opens; `crash.log` says "Windows did not apply screen-capture exclusion…" | Windows refused the capture exclusion, and the app will not run without it. Check `winver`: Windows 10 version 2004 or later, or Windows 11, is required. |
| "Settings changed elsewhere — reload…" in Settings, Save disabled | A chip save landed after Settings opened. Nothing was saved. Press Reload, check the form, Save (§2). |
| "Your settings file was damaged and could not be read. It was kept as settings.json.corrupt-…" | The app started with defaults and kept the damaged file as a backup in `%APPDATA%\com.aicallhelper.app\`. Copy what you need from it, set up again and Save (§1, Where the app keeps its data). |
| "Your settings file could not be read (…)" or "…a backup copy could not be made (…)" | The app is on defaults and will not save anything, to protect the file. Close whatever has `settings.json` open, or move the damaged file out of the folder, then restart. |
| Window opens somewhere odd | Click Dock to camera, or set "Window position at launch". |
| Shortcut does nothing | Another app owns the combo (the main view shows a notice), or Settings is open. |
| The window is visible in a screen share | Windows applied the exclusion (the app checks at launch), but that sharing or recording application captures the screen in a way the exclusion does not cover. Do not rely on it for that setup; report the conferencing app and version, Windows build and capture mode. See [tested sharing configurations](../docs/TROUBLESHOOTING.md#tested-sharing-configurations). |

The full symptom → cause → fix list is in `docs/TROUBLESHOOTING.md`.

## 7. Privacy and cost notes

- Audio goes to Deepgram (or stays on the machine in local mode); the
  transcript, your active profile and the question go to the selected
  answer provider. Nothing goes anywhere else; there is no telemetry.
- Keys are stored encrypted with Windows DPAPI and never shown or logged.
- Cloud cost is dominated by Deepgram's per-minute rate for the time you
  hold Record; the answer itself is a fraction of a cent on Haiku.

## 8. Free local voice from an installed app

This section is for someone who installed the app from the NSIS installer
and wants the free local mode. Everything here is true of the current build;
items marked *planned* are not implemented yet.

**Prerequisites.**

- A copy of this repository (a source checkout or a downloaded ZIP). The
  installer contains the app only; `scripts\setup-free-voice.ps1`,
  `scripts\start-free-voice.ps1` and `local-voice\` are not bundled with it.
  Bundling them so setup runs from the installed app is *planned*.
- 64-bit Python 3.11 or newer on `PATH` (or pass `-Python <path>` to the
  setup script).
- Internet access during setup only. No administrator rights, no account,
  no API key.
- RAM: on the 16 GB test laptop the Qwen runner requested about 3 GB for the
  model and context; leave several GB free beyond the app and speech service.

**Download size versus free space.** These are different numbers:

| Figure | Value | Source |
|---|---|---|
| Free space the script requires before a first install | 4 GB on the drive holding the data folder (512 MB when the runtime and model are already installed) | `scripts/setup-free-voice.ps1` |
| Free space the script recommends for a first install | 8 GB | `scripts/setup-free-voice.ps1` |
| Portable Ollama CPU runtime download | about 1.5 GB; the ZIP is deleted after extraction | `local-voice/install_ollama.py` |
| Qwen3.5 2B model download | about 2.7 GB | `scripts/setup-free-voice.ps1` |
| Python packages (moonshine-voice, websockets) and the Moonshine English model | measured on first setup | — |
| Disk used after setup (runtime, models, venv) | measured on first setup | — |

**Where it installs (the configured data location).** By default the data
folder is `%LOCALAPPDATA%\AI Call Assistant\local-voice`. Pass
`-DataDir 'D:\CallHelperLocal'` (any absolute path) to put it elsewhere,
for example on a drive with more space. The chosen folder holds the Python
environment, the Ollama runtime, both models and the `speech.log` /
`ollama.log` files. Its location is recorded in
`%LOCALAPPDATA%\AI Call Assistant\local-voice.json`; that file always stays
under LocalAppData, whatever `-DataDir` you chose. If you pick a folder
outside your user profile, make sure only your account can write to it:
the app runs the programs it finds there (see `docs/SECURITY.md`).

**Setup.** From the repository folder:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\setup-free-voice.ps1
```

Setup creates the Python environment, installs the speech packages, installs
the portable Ollama runtime, downloads the English speech model, writes
`local-voice.json`, starts both services, pulls `qwen3.5:2b` and warms it.
Then in the app: Settings → Keys & model → *Free local voice*, Save.

**Starting it later.** After a restart the services are not running. In
Settings, press **Start and warm free mode**: the app starts whichever of
the two services is not answering (Ollama on `127.0.0.1:11434`, the speech
service on `127.0.0.1:8765`), waits up to 60 seconds for them, then loads
the model, which stays warm for ten minutes after its last use.
`scripts\start-free-voice.ps1` does the same from PowerShell. The app never
downloads anything at this point; if a model is missing it tells you to
rerun setup.

**Cancelling an answer.** Press **Record** (or the global shortcut), or
submit a typed question, while a local answer is streaming: the running
answer is cancelled and the new request replaces it. There is no separate
"stop generating" button yet; one is *planned*. Local answers have a
90-second first-word limit and a five-minute total limit.

**Process ownership, stopping and uninstalling.** The two services run as
separate processes under your Windows account. Whoever started them —
the setup script, `start-free-voice.ps1`, or the app's *Start and warm*
button — they keep running after you close the app, and the app never stops
them. If an Ollama you installed yourself was already listening on
`127.0.0.1:11434`, the app uses it and does not start its own; that
instance, and its model folder, remain yours to manage.

To stop them, end `ollama.exe` (and any Ollama runner processes) and the
`python.exe` whose path is inside your data folder, in Task Manager →
Details (add the *Command line* column to tell them apart from other Python
processes). To remove local mode completely:

1. Stop both services as above.
2. Delete the data folder (default
   `%LOCALAPPDATA%\AI Call Assistant\local-voice`, or your `-DataDir`).
3. Delete `%LOCALAPPDATA%\AI Call Assistant\local-voice.json`.
4. In Settings, switch the answer model back to a cloud provider if you
   keep the app.

Uninstalling the app from Windows "Apps & features" does not do any of
this: the free-voice folder and `local-voice.json` are created by the setup
script, not by the installer, and the uninstaller does not stop running
services.

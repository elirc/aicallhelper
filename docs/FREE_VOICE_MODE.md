# Free local voice testing

Free local voice is a separate choice under **Settings → Answer model**. It uses
Ollama with Qwen3.5 2B for answers and Moonshine Tiny Streaming for English speech.
It requires no API keys, subscription, or metered inference. Electricity, disk
space, RAM and the initial download still use your own resources.

## Setup on Windows

Use 64-bit Python 3.11 or newer. From the project folder:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\setup-free-voice.ps1
```

For another drive:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\setup-free-voice.ps1 -DataDir 'D:\CallHelperLocal'
```

Allow at least 4 GB free; 8 GB is recommended for installation, models and operating
system headroom. Those are free-space figures checked by the script on the drive
that holds the data folder (512 MB when the runtime and model are already
installed), not the download size. The downloads the scripts announce are the
portable Ollama runtime (about 1.5 GB, deleted after extraction) and
`qwen3.5:2b` (about 2.7 GB); the Python packages and the Moonshine speech model
add to that, and their size is measured on first setup. Setup uses an isolated Python environment and the official
portable Ollama CPU runtime. It omits the GPU libraries, which this laptop's Intel
integrated graphics would not use. No administrator install or paid account is
needed. The temporary Ollama ZIP is deleted after successful extraction.

Setup downloads the English model, starts the local services, pulls
`qwen3.5:2b`, and warms it. Downloads need internet; subsequent inference uses
only loopback connections. Serving speech loads explicitly downloaded model files
and never calls the downloader. Ollama is started with cloud features disabled.

The installation location is recorded in
`%LOCALAPPDATA%\AI Call Assistant\local-voice.json`.
Model files live in the selected folder. If you already have Ollama running,
its existing model location applies; close that instance before setup if you
want all models in the new folder.

Select **Free local voice (Qwen3.5 2B + Moonshine)** under **Keys & model →
Answer model** and **Save**; the cloud key fields hide because this mode
needs none. Use **Start and warm free mode** in Settings after a restart or
a long idle period. The services start in the background (Ollama first, then
the speech service, only whichever is missing) and the app waits up to 60 s
for them before warming the model. The readiness indicators are live checks,
not simulated model availability. Starting and warming does not download
models. Runtime problems are listed under "Free local voice mode" in
[TROUBLESHOOTING.md](TROUBLESHOOTING.md).

## Same application flow

| Feature | Free local mode |
| --- | --- |
| Record / Stop & Answer | Same Windows system-audio capture, up to two minutes |
| Live transcript | Moonshine sends revised English transcript lines |
| Streaming answer | Ollama emits answer text; reasoning text is excluded |
| Typed Ask / practice questions | Same question input, without speech recognition |
| Active call profile (resume, job description or call context, focus, extra instructions) | Included in the same prompt; switch profiles from the chips on the main view |
| Brief / balanced / detailed | Same answer-style instructions |
| Regenerate, copy, history | Same existing controls |
| Global shortcut, topmost, screen-capture exclusion request | Existing native behavior (capture exclusion: see [tested sharing configurations](TROUBLESHOOTING.md#tested-sharing-configurations)) |
| Timing measurements | Real stop-to-transcript, first answer token and total time |

Record captures **audio playing through the computer**, including calls or a
practice question played through speakers/headphones. It does not switch to the
microphone. Responses appear as text, matching the existing app; this mode does
not add spoken answer playback.

No cloud key is required or sent for local inference. Selecting this mode
does not erase saved cloud keys. Switching back to a cloud provider restores the
existing cloud speech/answer path.

## Limits and troubleshooting

This configuration targets a 16 GB Windows laptop with an i7-8665U CPU. Real speed
depends on memory available to Ollama and on other running applications.
A small local model will not match larger cloud models' answer quality.
The Qwen runner on this machine requested about 3 GB for model, context and vision
components. Leave several GB of RAM available in addition to the app and speech
service. If warming reports a load failure or std::bad_alloc in ollama.log,
close unused apps and retry. A downloaded model can still fail to load when RAM
or Windows paging space is exhausted. Keep disk space available for Windows too.

- English speech only.
- About 7 KB (7,000 UTF-8 bytes) total for the full prompt, the **active
  profile** and the question. The cap counts every profile field — resume,
  job description or call context, **focus and extra instructions included**
  — because all of them ride in the prompt. While Free local voice is
  selected, the Profile section in Settings shows how many bytes the profile
  you are editing leaves for the question, with the answer style chosen in
  the form. It is computed by the app's core from your unsaved text, with
  the same code that enforces the cap. The fixed instructions alone take
  about 700–1,000 bytes, depending on call type and style (771 for a job
  interview with only a resume and Balanced style). There are three states:
  - room to spare;
  - a warning when under 200 bytes are left (many spoken questions will not
    fit);
  - an error when no question fits at all. Record and Ask are then refused
    in local mode before anything starts, and Save still works, so the
    profile can be kept for a cloud model.

  A typed question is checked at its real size before it is sent, and it
  stays in the box if it does not fit. A spoken question can only be checked
  after Stop. The runtime check stays the authority, and oversized input
  fails with the message
  `Free local mode supports about 7 KB of combined instructions, profile
  (resume, job description, focus, extra instructions) and question. Shorten
  the active profile in Settings or use a cloud model.` instead of silently
  dropping profile content. A profile that fits under a cloud model may not
  fit here; keep a shorter one for local use if you need to.
- 8,192-token model context and up to 512 generated tokens.
- Local first-token limit: 90 seconds; total answer limit: five minutes. Cloud
  limits remain 10 / 60 seconds. Local limits are ceilings, not speed promises.
- Speech finalization retains the five-second ceiling. If inference cannot keep
  up or the speech connection dies, the app reports an error instead of answering
  a silently incomplete transcript.
- Models are kept warm for ten minutes; use the warm button again if necessary.
- The speech service accepts native clients on 127.0.0.1:8765 and rejects browser
  Origin headers. Ollama uses 127.0.0.1:11434. Neither needs LAN exposure.
- Logs are `speech.log` and `ollama.log` in the chosen setup folder. The
  speech service does not write audio or transcripts to logs.
- Services stay running after the UI closes so later tests can reuse loaded models.
  Close their task-specific Python/Ollama processes in Task Manager when finished.

## Installed app versus source checkout

The NSIS installer does not include `scripts\` or `local-voice\`, so free local
voice setup currently needs a copy of this repository; the setup message in the
app ("Run scripts\setup-free-voice.ps1 from the project folder") refers to it.
Bundling the setup with the installer is planned, not done. Uninstalling the app
does not remove the data folder, `local-voice.json` or the running services; the
[user guide](../fabledocs/USER-GUIDE.md#8-free-local-voice-from-an-installed-app)
lists the steps.

## Repeatable validation

Release-level checks, including local inference with the internet off and
cancellation followed by another local request, are listed in
[RELEASE_CHECKLIST.md](RELEASE_CHECKLIST.md) and recorded in its ledger.

See [results from this laptop](FREE_VOICE_TEST_RESULTS.md) for passed checks and
the remaining disk-space and memory blockers.

```powershell
npm run build
npm test -- --pool=threads --maxWorkers=2 src/views/LocalMode.test.tsx
python -m unittest discover -s local-voice -v
$env:CARGO_INCREMENTAL='0'
cargo test --manifest-path src-tauri/Cargo.toml -p app-core
```

For a repeatable test with actual models, run `scripts/test-free-voice.ps1` after setup.
Add `-Loopback` to play the generated practice WAV through the default output and
capture it through WASAPI. The test exercises a streamed voice answer, a detailed
typed answer, cancellation, and another typed answer after cancellation. It prints
real transcript/answer text and latency metrics; it requires both local services.

For a manual voice test, play a spoken practice question, press Record, verify the
level meter and changing transcript, then Stop & Answer. Verify a streamed answer
and recorded timing. Repeat with a second question; inspect the previous history
entry, copy the answer, change style, regenerate, and test the global shortcut.
Cancel mid-answer and confirm a later recording remains usable. Turn off internet
after downloading to check that local inference continues.

## Primary sources

- [Ollama Windows portable distribution](https://docs.ollama.com/windows)
- [Qwen3.5 model library](https://ollama.com/library/qwen3.5)
- [Ollama chat API](https://docs.ollama.com/api/chat)
- [Moonshine transcription API](https://moonshine-voice.readthedocs.io/en/latest/using/transcription/)
- [Moonshine package and license](https://pypi.org/project/moonshine-voice/0.1.5/)

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
system headroom. Setup uses an isolated Python environment and the official
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

Select **Free local voice (Qwen3.5 2B + Moonshine)** and **Save**. Use
**Start and warm free mode** in Settings after a restart or a long idle period.
The services start in the background. Their readiness indicators are live checks,
not simulated model availability. Starting and warming does not download models.

## Same application flow

| Feature | Free local mode |
| --- | --- |
| Record / Stop & Answer | Same Windows system-audio capture, up to two minutes |
| Live transcript | Moonshine sends revised English transcript lines |
| Streaming answer | Ollama emits answer text; reasoning text is excluded |
| Typed Ask / practice questions | Same question input, without speech recognition |
| Resume and job description | Included in the same prompt |
| Brief / balanced / detailed | Same answer-style instructions |
| Regenerate, copy, history | Same existing controls |
| Global shortcut, topmost, screen-share protection | Existing native behavior |
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
- About 7 KB total for the full prompt, profile and question. Oversized input
  fails with a clear message instead of silently dropping resume content.
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

## Repeatable validation

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

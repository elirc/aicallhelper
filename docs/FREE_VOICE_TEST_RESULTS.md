# Free voice mode validation

The separate free mode is implemented in source, with Ollama Qwen3.5 2B for answers
and Moonshine Tiny Streaming for English speech. Both runtimes and model files
were installed locally. End-to-end native validation is **blocked by available
disk space and RAM**; this is not a fully validated desktop release yet.

## Results from this laptop

| Check | Result |
| --- | --- |
| Frontend production build | Passed: 55 modules; initial JavaScript 173.92 KB / 56.49 KB gzip; Settings and practice content load separately |
| TypeScript typecheck | Passed |
| Relevant frontend tests | 98 passed across Settings, MainView, useSession and LocalMode |
| Python service unit tests | 3 passed: transcript revision, PCM decoding, CPU runtime extraction filtering |
| PowerShell script parsing | All setup/start/test scripts parsed without errors |
| Formatting and whitespace | New Rust files parsed with rustfmt; git diff --check passed |
| Actual speech health endpoint | Passed: service identity, protocol, ready flag and a single JSON Content-Type |
| Actual Moonshine speech inference | Passed with a generated 16 kHz mono practice WAV; final transcript: "How do you handle a difficult customer?" |
| Speech finalization time | 3,334 ms in that single test, within the existing five-second limit |
| Local answer model download | qwen3.5:2b downloaded and verified by Ollama |
| Qwen model warm-up | Failed: runner logged std::bad_alloc while available physical memory was about 684 MiB |
| Native workspace tests | Did not reach project tests: compiling the windows dependency failed with Windows error 112, not enough disk space |
| Real native voice/answer smoke test | Did not execute: compiling windows-sys failed with the same disk-space error |
| Updated standalone desktop executable | Not produced; the queued native build was stopped after disk exhaustion |

The speech sample is a functional check, not an accuracy or speed benchmark.
Intermediate hypotheses were revised before the correct final transcript.
The full WASAPI-to-answer flow, actual Qwen answer quality/latency, and live
cancellation/recovery remain unverified. Native unit tests were added for streaming
JSON, speech finalization/disconnection/cancellation, and local settings persistence;
they must be run once compilation can finish.

## Resource findings

This laptop has 16 GB RAM, an i7-8665U CPU and Intel UHD 620 integrated graphics.
Qwen's runner requested about 3 GB for the answer model, context and vision
components. Other applications left less than 1 GB of physical memory available.
The native build subsequently exhausted C: (down to about 6 MB at one check).
Only regenerable project build caches, old static libraries and debug symbols
were removed. Existing app executables, installers, source and downloaded models
were preserved. Unrelated applications and personal files were left alone.

The installed local data is in the project's ignored `.local-voice` folder; its
location is recorded in `%LOCALAPPDATA%\AI Call Assistant\local-voice.json`.
The existing release executable predates this feature and must not be mistaken
for an updated free-mode build.

## Complete validation after freeing resources

Free several GB of RAM and disk space first. About 8 GB of free disk space gives
more room for native builds and Windows paging. Run these from the project root,
sequentially so model inference and compilation do not compete for resources:

```powershell
$env:CARGO_INCREMENTAL='0'
$env:CARGO_PROFILE_DEV_DEBUG='0'
$env:CARGO_PROFILE_TEST_DEBUG='0'
$env:CARGO_BUILD_JOBS='1'
cargo test --manifest-path src-tauri/Cargo.toml --workspace -- --test-threads=2
npm run tauri -- build --debug --no-bundle
powershell -ExecutionPolicy Bypass -File .\scripts\start-free-voice.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\test-free-voice.ps1 -Loopback
```

The standalone build should create `src-tauri\target\debug\aicallhelper.exe`.
In the rebuilt app choose **Settings > Answer model > Free local voice**, save,
and use **Start and warm free mode**. The real-model test checks a voice answer,
a detailed typed answer, cancellation, and a new answer after cancellation.
Then follow the manual history/copy/regenerate/hotkey checks in the
[setup guide](FREE_VOICE_MODE.md). No paid API key is needed for these local tests.

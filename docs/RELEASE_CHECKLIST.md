# Release checklist and results ledger

What has to pass before a build of AI Call Assistant is called dependable,
and the record of every run that actually happened. Added 2026-09-22 for
agreed action R6 in
[PROJECT-REVIEW-2026-09-19.md](../fabledocs/PROJECT-REVIEW-2026-09-19.md).

Two words are kept apart throughout:

- **Implemented** — the code exists and the deterministic tests that cover
  it pass. [TESTING.md](TESTING.md) is the record of what each test pins.
- **Validated on hardware** — someone ran the packaged app on a named
  Windows machine, with real audio, real displays and the real sharing
  application, and recorded the outcome in the ledger in Part B.

A feature can be implemented and still unvalidated. Nothing in the README,
the user guide or the troubleshooting guide may claim a hardware behavior
that has no row in Part B. Historical counts (for example the ones in
`fabledocs/REPORT.md`) are not carried forward as evidence; each release
records its own run.

---

## Part A — the validation list

Run the three stages in order on one commit. Record the commit before you
start (`git rev-parse HEAD`) and note whether the working tree was clean
(`git status --short`); a run on a dirty tree is recorded as such.

### A1. Deterministic gates

No network, live provider or audio device is involved. The same steps run in
CI on every push and pull request
([`.github/workflows/ci.yml`](../.github/workflows/ci.yml), job `gates`).
A green CI run on the release commit satisfies this stage; link it as the
evidence.

From the repository root:

```powershell
npm ci
npm run typecheck                                  # tsc --noEmit, strict
npx vitest run                                     # frontend suite (count in TESTING.md)
npm run build                                      # production frontend bundle
python -m pip install pytest                       # once per Python install
python -m pytest -q local-voice/test_server.py     # local speech service unit tests
```

From `src-tauri/`:

```powershell
cargo test -p app-core                             # core tests (count in TESTING.md)
cargo clippy -p app-core -- -D warnings            # core lint, must be clean
cargo test -p aicallhelper                         # Tauri shell unit tests
```

`cargo test -p aicallhelper` compiles the whole Tauri shell and needs
several GB of free disk under `src-tauri/target` and nothing else running
cargo at the same time (see [DEVELOPMENT.md](DEVELOPMENT.md)). On a machine that cannot build it, the
stage is **not passed**; `cargo check` is a development aid, not a
substitute here.

Pass criterion: every command exits 0 with zero failed tests. Record the
per-suite pass counts in the ledger's *Result* column.

### A2. Packaged build

```powershell
npm run tauri build
```

Output: a per-user NSIS installer under
`src-tauri\target\release\bundle\nsis\` (for version 3.1.0 its name is
`AI Call Assistant_3.1.0_x64-setup.exe`). Record its identity before
installing it anywhere:

```powershell
Get-FileHash -Algorithm SHA256 "src-tauri\target\release\bundle\nsis\*.exe"
```

The installer is unsigned. A build produced by the manually triggered CI job
(`package` in `ci.yml`) is a convenience artifact; it becomes release
evidence only when the checks in A3 are run against that exact file and
recorded with its SHA-256.

Toolchain versions to record with the run:

```powershell
rustc -V; cargo -V; node -v; npm -v; python --version
[System.Environment]::OSVersion.Version; (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').DisplayVersion
```

### A3. Manual checks on the installed app

Install the file from A2 (not a `tauri dev` build) on the machine named in
the ledger. The step numbers refer to the
[README "Manual QA script"](../README.md#manual-qa-script), which has the
exact expected strings; this list adds what the script does not cover.

Installation and upgrade

1. **First run from the installer** on an account with no
   `%APPDATA%\com.aicallhelper.app\` folder — README step 1.
2. **Upgrade in place** over the previous installed version with existing
   profiles and saved keys: profiles, active profile, keys ("saved — type to
   replace") and window settings survive.
3. **Keys and profiles** — README steps 2–4.

Real audio and the answer path (cloud provider)

4. **Record → Stop → answer** with real system audio from a call or media
   player — README steps 5–7. Record the latency chip values.
5. **Immediate failures** — Record with a wrong Deepgram key, Ask with a
   wrong answer-provider key, and Record with no active output device: each
   shows one actionable error and the Record button is usable again.
6. **Rapid supersession** — press Record repeatedly and quickly, and press
   Record while an answer is streaming: exactly one session remains active,
   the older answer stops, and no stale error appears.
7. **Regenerate, styles, history, stream follow, focus mode** — README
   steps 8–12.

Hotkey

8. **Hotkey from another app** — README step 14.
9. **Hotkey taken** — README step 15.

Display, DPI and monitors

10. **DPI scaling** — repeat README steps 1, 13 and 17 at 100 %, 125 % and
    150 % display scaling.
11. **Dock and relaunch** — README steps 13, 17, 18 and 19.
12. **Monitor removal** — README step 20.

Screen sharing

13. **Capture exclusion** — README step 16, once per sharing configuration
    you intend to rely on (conferencing application and version, whole
    screen versus single window, and any recording tool). Record each
    configuration in the
    [tested sharing configurations](TROUBLESHOOTING.md#tested-sharing-configurations)
    table as well as in the ledger. The app requests Windows capture
    exclusion (`WDA_EXCLUDEFROMCAPTURE`, Windows 10 version 2004 or later);
    a pass for one application and capture mode says nothing about another.

Free local voice (only if the release advertises it)

14. **Setup** — `scripts\setup-free-voice.ps1` from a source checkout,
    including once with a `-DataDir` path that contains spaces. Record the
    measured download volume and the disk space used under the data folder.
15. **Local inference** — Start and warm free mode, then README step 21:
    Record → Stop & Answer with the internet disconnected.
16. **Cancellation, then another local request** — press Record (or submit
    an Ask) while a local answer is streaming, then complete another local
    answer. `scripts\test-free-voice.ps1` automates a version of this against
    the running services; record its output as evidence when used.

Pass criterion: every applicable check behaves as described. Any failure
is recorded with the symptom and whatever log applies (`crash.log`,
`speech.log`, `ollama.log`) — never with profile text, keys or transcripts.

---

## Part B — results ledger

One row per run. Add rows; never edit a recorded result. *Procedure* names
the stages and checks that were run (for example "A1 all; A2; A3 1–13").
*Result* separates what passed from what failed or was skipped. *Evidence*
is a link to the CI run, a log excerpt or a note stored in the repository.

| Date | Commit | Artifact identity (installer filename + SHA-256) | Toolchain (rustc / cargo / node / npm / python) | Machine / OS build | Procedure | Result | Evidence |
|---|---|---|---|---|---|---|---|
| 2026-09-23 | uncommitted working tree on 4289f16 | `AI Call Assistant_3.1.0_x64-setup.exe` (2,530,459 bytes), SHA-256 `4C9C9015CB025D3535122E420E5BFA12AD59548E4D32425C7863009959C5AA5D` | rustc 1.97.1 / cargo 1.97.1 / node 22.16.0 / npm 10.9.2 / Python 3.13.3 | Windows 11 Pro, `ver` 10.0.26200.9457; one 1920x1080 display at 150 % (144 DPI) | A1 all; A2 (NSIS build with `CARGO_BUILD_JOBS=2`); partial live check of the built `aicallhelper.exe` (not installed), no API keys: first run, capture exclusion read-back, camera dock, Settings, profiles, focus mode, Ask without keys, second launch | **Passed:** typecheck; vitest 426/426 (16 files); vite build; pytest 3/3; app-core 374 + 1; app-core clippy clean; shell 58/58; shell clippy clean; NSIS build. Live: display affinity 0x11, top-centre dock 8 px below the work area, first-run status, Settings, two profiles, focus mode, actionable no-key error, single instance. **Failed:** at 150 % on a 1080p display the default 460x700 window (1097 px tall) is taller than the 1008 px work area, so its bottom sits behind the taskbar; the dock button's preset size fits. **Not run:** installer install/upgrade/uninstall (A3 1–2), real call audio and cloud answers with keys, hotkey from another app, DPI 100/125 % and multi-monitor, screen sharing per conferencing app, free local voice inference (the runtime is installed but only ~0.6 GB RAM was free; the model needs about 3 GB). | [fabledocs/READINESS-REPORT-2026-09-22.md](../fabledocs/READINESS-REPORT-2026-09-22.md) |

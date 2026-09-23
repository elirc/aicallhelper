# Readiness report: AI Call Assistant 3.1.0

*Work started 2026-09-22 and finished 2026-09-23. The file name keeps the start date because [`README.md`](README.md) links to it.*

- **Scope:** the uncommitted working tree on `4289f16`, on one Windows 11 machine.
- **Evidence:** the gates were run on the tree, a packaged NSIS build was made, and the built app was checked live without API keys.
- **Ledger:** the ledger row is in [`docs/RELEASE_CHECKLIST.md`](../docs/RELEASE_CHECKLIST.md), Part B.

---

## 1. Verdict

**You can install and use it now for real calls with cloud providers (Deepgram + Anthropic or Groq), with caveats.**

**What was proven today:**
- Every automated gate is green.
- The packaged installer builds.
- The built app launches and verifies Windows capture exclusion (display affinity 0x11).
- It docks under the camera and saves profiles.
- It fails clearly instead of hanging when keys are missing.

**What is not proven yet:**
- Answering real call audio with real keys.
- Screen-sharing behaviour in your conferencing app.
- The hotkey from another app.
- Other DPI and monitor setups.

Run the short pre-call checklist in §7 once before a call that matters.

**Found today:**
- One visible defect: at 150 % scaling on a 1080p screen, the first-run window is taller than the usable screen. Pressing the dock button (⬆) fixes it (§3, §6).
- Free local voice is installed on this machine, but it was not run today because only about 0.6 GB of RAM was free.

## 2. What was tested today and results

"Before" is the baseline taken on 2026-09-22, before any change. "After" is the final run on 2026-09-23. Each gate was run once, one at a time.

| Gate | Before (2026-09-22) | After (2026-09-23) | Time |
|---|---|---|---|
| `npm run typecheck` | clean | clean (`tsc --noEmit`, exit 0) | 23 s |
| `npx vitest run --maxWorkers=2` | 375 passed, 16 files | `Test Files 16 passed (16)`, `Tests 426 passed (426)` | 114 s (vitest reports 102.7 s) |
| `npm run build` | not recorded | `✓ built in 5.85s`, exit 0 | 29 s |
| `python -m pytest -q local-voice/test_server.py` | 3 passed | `3 passed in 0.52s` | 10 s |
| `cargo test -p app-core` | 302 + 1 passed | `374 passed` (unit), then `1 passed` (integration), then 0 doc-tests | 19 s |
| `cargo clippy -p app-core -- -D warnings` | clean | clean (`Finished`, no warnings) | 4 s |
| `cargo test -p aicallhelper -j 2` | **failed to compile** (Windows DLL-init / memory error, 0xc0000142) | `58 passed; 0 failed` | 77 s |
| `cargo clippy -p aicallhelper -j 2 -- -D warnings` | not reached | clean after one fix (below) | 31 s |
| `npm run tauri build` (`CARGO_BUILD_JOBS=2`) | **failed** ("failed to build app", same memory error) | succeeded: `Finished release profile in 20m 12s`, NSIS bundle produced | 1305 s total |

**Clippy fix made today:** the lint was an unnecessary pointer cast in the capture-exclusion read-back, at `src-tauri/src/window.rs:71`. The fix drops `as *mut std::ffi::c_void`, since `hwnd.0` already has that type. Behaviour is unchanged.

**Build artifact**
- Installer: `src-tauri\target\release\bundle\nsis\AI Call Assistant_3.1.0_x64-setup.exe`.
  - Size: 2,530,459 bytes.
  - SHA-256: `4C9C9015CB025D3535122E420E5BFA12AD59548E4D32425C7863009959C5AA5D`.
- Executable: `src-tauri\target\release\aicallhelper.exe`.
  - Size: 8,740,352 bytes.
  - SHA-256: `E16591B4E6A27EA1600CC6CCB54D94347FBF9B9094E56E62F6B57D6614BFEA8B`.
- Toolchain: rustc 1.97.1, cargo 1.97.1, node 22.16.0, npm 10.9.2, Python 3.13.3.
- OS: Windows 11 Pro, `ver` = 10.0.26200.9457.
- Display: one 1920x1080 screen at 150 % (144 DPI). Its work area is 1920x1008.

TESTING.md already had the right counts (374 + 1, 58, 426), so it was not changed.

## 3. Live check results

**Method**
- The built `aicallhelper.exe` was run directly. It was not installed.
- WebView2 remote debugging was turned on so the page could be inspected with CDP. CDP attached without problems.
- Window facts came from Win32 calls: `GetWindowRect`, the DWM frame, `GetWindowDisplayAffinity`, `GetDpiForWindow`, and the monitor work area.
- No API keys were entered, and no paid provider was called.

**Settings**
- `%APPDATA%\com.aicallhelper.app\` did not exist before the test, so this was a real first run.
- Both folders the test created were removed afterwards, so the user's state is unchanged:
  - `%APPDATA%\com.aicallhelper.app\` (settings)
  - `%LOCALAPPDATA%\com.aicallhelper.app\` (WebView data)

Screenshots are in the session scratchpad's `live\` folder and are not part of the repository: `01-main-first-run.png`, `02-settings-local.png`, `03-discard-dialog.png`, `04-main-two-profiles.png`, `05-focus-mode.png`, `06-ask-no-keys.png`, `07-local-readiness.png`.

| # | Check | Result | Evidence |
|---|---|---|---|
| a | Launch: one process, window visible, title "AI Call Assistant" | PASS | 1 `aicallhelper` process; the main window title is "AI Call Assistant"; the window is foreground and topmost |
| b | Capture exclusion verified (display affinity 0x11) | PASS | `GetWindowDisplayAffinity` = 17 (0x11) on both launches. The app's own launch check passed too, or setup would have failed |
| c1 | Docked top-centre, 8 px below the work area's top edge | PASS | Visible frame at x 613–1307 (centre 960 on a 1920-wide work area), top y = 8. Checked again on a second launch with saved settings: same position |
| c2 | Sized sensibly for the DPI | **FAIL** | At 144 DPI the default 460x700 logical window is 1097 px tall (outer frame) in a 1008 px work area. The bottom edge is at y 1105, so about 97 px (the lower part of the "Question heard" panel) sits behind the taskbar. The dock button's preset size (600x520 logical, frame 904x827 at top 8, centred) does fit |
| d | First-run status line and "Suggested answer" panel | PASS | Status line: "First run: open Settings (gear icon) and add your API keys". Panel titles: "Suggested answer", "Question heard" |
| e1 | Provider switch hides and shows key fields | PASS | Anthropic shows the Deepgram and Anthropic fields. Groq shows the Deepgram and Groq fields. Free local voice shows none |
| e2 | Free local voice shows the readiness panel and the local budget readout | PASS | The panel "Free local voice setup" is shown. The budget line reads "6,368 bytes left for the question in free local mode (632 of 7,000 used by the instructions and this profile)." |
| e3 | Escape on a dirty form asks before discarding | PASS | The dialog text is "Discard unsaved changes?"; "Keep editing" returns to the form |
| f | Second profile of type Sales, save, two chips, switch chip | PASS | "Sales test" (Sales call) was saved without keys, with no error. The main view shows chips `Default`, `Sales test` (pressed). Clicking `Default` moved the pressed state to it. The chips were still there after a relaunch |
| g | Focus mode toggle | PASS | On: `aria-pressed=true`, and the profile chips and Ask box are hidden. Off: both are back |
| h | Ask with no keys gives a clear, actionable error | PASS | The error box appeared 38 ms after Ask: "AI key missing — No Anthropic API key set, so answers can't be generated. Open Settings (gear icon) and add it." The Ask button came back at once. Minor: the same text also shows inside the answer panel, and the empty entry is tagged "incomplete" |
| i | Second launch focuses the existing window | PASS | Checked twice. The second process exited with code 0 within 4 s. One `aicallhelper` process remained, and its window was foreground |
| j | settings.json on disk has the revision and the profiles | PASS (as designed) | The file has both profiles, `activeProfileId`, `launchPlacement: "camera"` and `windowBounds`. The revision is not persisted, by design (ADR 016: it starts at 1 each launch). The running app reported `revision: 2` after one form save |
| — | Dock button (⬆) | PASS | It resized to the preset and re-centred at top 8 px |
| — | State restored and app closed | PASS | The test instance was closed with a normal window close, both created folders were removed, and no `aicallhelper` process is running |

**Totals:** 14 PASS, 1 FAIL (c2), 0 NOT RUN, for the listed checks (c and e split into parts).

**Incident during testing, not attributed**
- During the first session, the window moved from x 604 to x 393 and later closed.
- There was no crash.log, and there was no Windows Application Error event.
- The exit looks clean, and it may have been a manual move and close on the desktop.
- A relaunch docked correctly and every later check passed.
- It is recorded here because it could not be explained from the app's side.

### Free local voice on this machine

| Item | Result |
|---|---|
| Config file | `%LOCALAPPDATA%\AI Call Assistant\local-voice.json` → `dataDir` = `C:\Users\Owner\Desktop\aicallhelper\.local-voice` |
| Runtime on disk | Present: Ollama runtime (73 MB), `qwen3.5:2b` manifest with 2.6 GB of models, Moonshine speech model (45 MB), Python venv, `server.py` |
| In-app readiness (Check status) | "Ollama: Not running", "Qwen3.5 2B: Not found" (the model list cannot be read while Ollama is stopped), "English speech: Not running" |
| `scripts\test-free-voice.ps1` | **NOT RUN.** No download was needed. Only about 0.6 GB of RAM was free while the app was running, and the local model needs about 3 GB, so the run would have paged heavily on this machine. Ollama and the speech service were last active on 2026-09-12/13 (their logs). |

## 4. What changed today, by package

### R1/R2: session and streaming reliability ([ADR 015](../docs/adr/015-session-outcomes-and-adoption.md))
- Every question now ends in one recorded state: completed, failed, cancelled, or unknown. The UI asks the core for that state if it missed the live events, so an answer can no longer be left hanging on "answering…".
- Early text is no longer lost. Events that arrive before the app has adopted the session are held (bounded to 64) and replayed in order.
- A start now waits until the event listeners are ready. That wait is capped at 3 s and then shows "The app could not start listening for session events. Restart the app."
- An answer counts as complete only when the provider's end marker arrives.
  - A cut-off stream, a malformed frame or an oversized frame is shown as **incomplete**, and the text is kept.
  - A token-capped answer is tagged **cut short**, and a cancelled one is tagged **stopped**.
- Read limits: stream lines are capped at 1 MiB, and error bodies at 16 KiB.
- A crash inside an answer now ends that answer with an error. It no longer takes the app down or leaves the session stuck.

### Capture exclusion is verified at launch (a real defect found today)
- tao 0.35 calls `SetWindowDisplayAffinity` and **discards the Windows result**. So the claim that "the launch fails if Windows refuses content protection" was false.
- The shell now reads the affinity back with `GetWindowDisplayAffinity` and requires `WDA_EXCLUDEFROMCAPTURE` (0x11). Anything else fails setup with a message naming Windows 10 version 2004 or later.
- Verified live today: the read-back was 0x11.
- The Settings note now says: "Windows capture exclusion was verified at launch; test your sharing app before relying on it."

### R3/R5: settings save lock, revisions, OS effects, damaged-file quarantine ([ADR 016](../docs/adr/016-settings-revisions-and-effects.md))
- **Save lock:** the whole Settings form locks while saving. A failed save keeps your draft and typed keys, and focus returns to Save.
- **Revisions:** settings carry a revision.
  - A stale form cannot overwrite newer settings. It shows "Settings changed elsewhere — reload. Your unsaved edits are kept."
  - Reload keeps the fields you edited.
  - Moving the window never makes an open form stale.
- **OS effects:** hotkey, always-on-top and dock are re-applied from the newest saved settings, in any order.
  - A hotkey that Windows refused (for example, taken by another app) is retried on the next save.
- **Damaged settings file:**
  - A `settings.json` that cannot be parsed is renamed to `settings.json.corrupt-<time>`, and the app starts from defaults with a visible warning.
  - A file that cannot be read is left untouched, and saving is blocked for that session.

### R4: local prompt budget
- In free local mode, Settings shows how many bytes are left for the question. The check uses the same code as the real gate, so the two cannot disagree.
- An oversize local request is refused **before** recording starts, or before a typed Ask is sent. The typed text stays in the box.

### R6: release checklist and CI
- New [`docs/RELEASE_CHECKLIST.md`](../docs/RELEASE_CHECKLIST.md) with three parts: gates, the packaged build, and 16 manual checks. The results ledger now holds its first real row (this run).
- New `.github/workflows/ci.yml`:
  - Gates run on Windows on every push and pull request.
  - A manual-only job builds the installer.
  - A CI build alone is not release evidence.

### R7 and documentation corrections
- Screen-sharing claims are softened everywhere to "requests Windows capture exclusion; test your setup". This covers the README, SPEC, TROUBLESHOOTING, SECURITY, FREE_VOICE_MODE, IDEAS, ADR 001 and the USER-GUIDE.
- New TROUBLESHOOTING section, "The window shows up in a screen share or recording", with a tested-configurations table that is still empty.
- SECURITY.md now describes the one command that launches processes (`prepare_local_voice`) and its same-user trust boundary.
- The USER-GUIDE has a new §8 on local mode from an installed app. The disk figure is corrected: 4 GB free is required and 8 GB recommended, against downloads of about 1.5 GB and 2.7 GB.
- The fabledocs index is rewritten. Historical documents carry a banner.

## 5. Review findings and their disposition

### Reliability review (R1/R2), 6 findings, no High severity

| ID | Severity | Finding | Disposition |
|---|---|---|---|
| F1 | Medium | The driver guard emitted events while a panic was unwinding; a second panic would abort the process | **Fixed.** The guard settles synchronously, and the emit is deferred to a fresh task. Two tests: a sink that panics once, and one that panics on every emit |
| F2 | Medium | Waiting for listener readiness had no bound, so Record/Ask could hang silently | **Fixed.** 3 s timeout with a visible error, and the next attempt retries. Only the four session events gate a start |
| F3 | Low | An SSE overflow dropped events already decoded in the same chunk | **Fixed.** Decoded events are applied before the failure. Test added |
| F4 | Low | A hold-buffer overflow could leave a text fragment | **Fixed.** Other sessions' events are evicted first, and the reducer accepts the core's full partial text. Tests added |
| F5 | Low | Race in the device-error handling (check and settle were separate) | **Fixed.** Check and settle now run in one critical section. No deterministic test is possible |
| F6 | Low | Doc drift (a TESTING header count, stale line references) | **Fixed.** Line references were replaced with symbol names |

### Settings review (R3/R5/R4), 8 findings

| ID | Severity | Finding | Disposition |
|---|---|---|---|
| F1 | Low | A refused hotkey was recorded as applied and never retried | **Fixed.** Only a registration that took counts as applied. Startup seeding uses the real result. Four tests |
| F2 | Low | The conflict banner outlived its cause after the user undid their edits | **Fixed.** Test added |
| F3 | Low | A failed budget call left "Checking…" on screen forever | **Fixed.** It now says the check failed and that the limit is still enforced. Test added |
| F4 | Low | SPEC/ARCHITECTURE misdescribed the typed-Ask refusal | **Fixed** (docs) |
| F5 | Low | Focus dropped to the page body when the save lock engaged | **Fixed.** Focus returns to Save. Test added |
| F6 | Info | The effect step blocks on the main thread while holding the reconciler lock | **Not fixed.** No current caller can deadlock, and the doc comment and ADR warn against future main-thread callers |
| F7 | Info | Check-then-rename window in quarantine | **Not fixed.** Not reachable in practice; single-instance runs first |
| F8 | Info | The budget note shows the previous figure for about 250 ms while typing | **By design** |

The one clippy lint that this review left open (`window.rs:71`) was fixed today (see §2).

## 6. Known limits and items not yet validated

"Implemented" and "validated on hardware" are separate. Everything below is implemented and covered by automated tests. None of it has been validated on hardware.

**Not validated**
- **Real call audio with keys.** Record → Stop → answer through Deepgram and Anthropic or Groq was never run, because no keys were used.
- **Cloud providers in general**, including the retry and error paths against the live services.
- **The global hotkey pressed from another app.**
- **DPI 100 % and 125 %, and multi-monitor**, including monitor removal. Only 150 % on a single 1080p display was seen.
- **Screen sharing, per conferencing app and capture mode.** Windows reported the exclusion, but no Zoom, Teams or Meet share was tested. The tested-configurations table is still empty.
- **Installer install, upgrade and uninstall** (checklist A3 1–2). The app was run from `target\release` and was not installed.
- **Free local voice inference today** (see §3). It needs about 3 GB of free RAM.
- **The effect step's native behaviour:** changing the hotkey twice quickly, toggling always-on-top, and choosing camera placement from Settings.

**Known limits and defects**
- **Window too tall at 150 % on 1080p (found today).** The first-run 460x700 logical size exceeds the 1008 px work area, so the bottom of the window is behind the taskbar.
  - Workaround: press ⬆ (dock) once, or resize the window. The size is remembered after that.
  - Suggested fix: clamp the height to the work area in the launch dock and restore paths.
- **The missing-key error shows twice** (in the answer panel and the error box), and the empty entry is tagged "incomplete". This is cosmetic.
- **Local mode setup is not bundled** with the installer. It needs a source checkout and `scripts\setup-free-voice.ps1`, and uninstalling does not remove local data or services.
- **No Stop generating button yet.** A new Record or Ask supersedes a running answer.
- **Machine limits:** the disk and RAM are tight. Shell builds need `-j 2`, and the release build took about 22 minutes.

## 7. Steps to start using it now

1. **Install.** Run `src-tauri\target\release\bundle\nsis\AI Call Assistant_3.1.0_x64-setup.exe`.
   - Check that its SHA-256 matches `4C9C9015CB025D3535122E420E5BFA12AD59548E4D32425C7863009959C5AA5D`.
   - Or run `src-tauri\target\release\aicallhelper.exe` directly without installing.
2. **Add keys.** Open Settings (gear icon).
   - Keep "Claude Haiku 4.5 (recommended)", or pick Groq.
   - Paste your Deepgram key and your Anthropic key (or Groq key), then press Save.
   - Keys are stored encrypted and are not shown again.
   - Or, for free local voice: from the project folder run `powershell -ExecutionPolicy Bypass -File .\scripts\setup-free-voice.ps1` (already done on this machine). In Settings choose "Free local voice", then press "Start and warm free mode". You need about 3 GB of free RAM.
3. **Create a profile.** In Settings → Profile → New:
   - name it and choose the call type;
   - paste your resume and the job description (or your sales or support notes);
   - press Save.
   - With two or more profiles, chips on the main screen switch between them.
4. **Dock.** Press ⬆ on the main screen. The window moves under your webcam at a size that fits. Optionally set "Window position at launch" to "Dock under the camera".
5. **Pre-call checklist** (5 minutes, before a call that matters):
   - Play a practice question (for example a YouTube interview clip) through your speakers or headset, press **Record**, then **Stop & Answer**. Confirm that the transcript and the answer appear.
   - Type a question in Ask and confirm that an answer streams.
   - Focus your meeting app and press **Ctrl+Shift+Space**. Confirm that recording starts. If the main screen says the shortcut is taken, pick another one in Settings.
   - Start a test share in your conferencing app, with the screen shared, and have someone confirm the assistant window is **not** visible. Record the app, its version and the capture mode.
   - Check that the whole window is visible above the taskbar (see §6).
6. **Manual checks still owed** before a release claim (from [`docs/RELEASE_CHECKLIST.md`](../docs/RELEASE_CHECKLIST.md), Part A3):
   - Installer first run and upgrade in place (1–2).
   - Keys and profiles (3), and real-audio Record → Stop → answer (4).
   - Immediate failures with wrong keys (5), and rapid supersession (6).
   - Regenerate, styles, history, stream follow and focus mode (7).
   - Hotkey from another app, and hotkey taken (8–9).
   - DPI 100/125/150 % (10), dock and relaunch (11), monitor removal (12).
   - Capture exclusion per sharing configuration (13).
   - Local setup, local inference offline, and cancel-then-request (14–16).
   - Record each run as a new ledger row.

## 8. Recommended next steps

These come from [FINAL-REVIEW-AND-ADDITIONS-2026-09-19.md](FINAL-REVIEW-AND-ADDITIONS-2026-09-19.md), "What to add next":

| Order | Addition | Status |
|---|---|---|
| 1 | Answer status and context labels | **Status labels done** (incomplete / cut short / stopped). The profile, provider and style caption per entry is still open |
| 2 | Edit & re-ask | Open |
| 3 | Stop generating | Open |
| 4 | Pre-call readiness check (profile/provider, device, credentials, local readiness, prompt capacity) | Open. Its parts exist separately: the local readiness panel and the budget line in Settings |
| 5 | Reliable local setup from the installed app | Open. Required before claiming turnkey installer support |
| 6–8 | Markdown debrief export, local performance view, output-device picker and reading preferences | Open |

Also:
- Fix the first-run window height at high DPI (§6).
- Run the A3 manual checks with real keys, and fill in the tested-sharing table.
- Add the small synthetic answer-quality evaluation set that the final review recommends.

**Nothing is committed.** The working tree has 87 changed or new paths on top of `4289f16`: `git diff --stat` reports 74 tracked files changed (+13,220 / −1,993), plus untracked files. Examples of the untracked files are `.github/`, `docs/RELEASE_CHECKLIST.md`, ADRs 013–016, `store/effects.rs`, `fabledocs/` and several new components. Review them with `git status` and `git diff --stat`, then commit. Add the untracked files deliberately. `.local-voice/` is already ignored by `.gitignore`.

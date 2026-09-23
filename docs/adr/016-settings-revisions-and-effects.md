# ADR 016 — Settings revisions, reconciled OS effects, the save lock, and the local prompt budget

**Status**: accepted (2026-09-22; implements review actions R3, R5 and R4)

## Context

Four problems shared the settings path.

- **R3, lost edits.** Only the Save button was disabled during a save. Text
  typed while the save was in flight was replaced by the returned snapshot,
  and a typed key was cleared.
- **R5, ordering.** The store applied each patch under its lock, but nothing
  ordered the responses or the OS side effects. Two responses could arrive in
  the opposite order to their commits, and `applySettings` installed whichever
  came last. A Settings form seeded from an old view could later submit its
  whole `profiles` array over a newer commit. The shell computed hotkey,
  always-on-top and dock effects from each patch's own before/after pair,
  after the lock was released. So saves A then B could run effects B then A.
  "Skip older effects" is no fix either: if A changed the hotkey and B only
  the style, skipping A leaves the old hotkey registered.
- **R4, the local budget.** Settings warned at 6,500 raw profile bytes, but
  `local.rs` refuses the whole request (system prompt plus user turn) above
  7,000 bytes. An interview profile with a 6,500-byte resume and the question
  "Hi?" is 7,274 bytes, and Settings showed no warning.
- **Damaged settings file.** A missing, unreadable or unparseable file all
  loaded as defaults, and the next write (even a window-geometry save)
  replaced the file with those defaults.

The final review (FINAL-REVIEW §3 to §5) refined the fixes. The revision
compare must be atomic with the commit. A failed write must not advance the
revision. Geometry needs an explicit rule. Effects must reconcile the latest
desired state. The budget must preview the unsaved draft, separate "little
room" from "over the limit", and keep the backend gate authoritative. Only
invalid content may be quarantined, and only after a copy is preserved.

## Decision

**Revision.** `SettingsView.revision: u64` is the committed revision of the
editable settings. It starts at 1 on each launch (it orders views within one
run and is not persisted). `SettingsStore` keeps it next to the settings
behind the store's one `RwLock`. `apply_patch` does all of this in one
critical section:

1. refuse if writes are blocked (see the damaged-file rule below);
2. if the patch carries `expectedRevision`, compare it with the committed
   revision and fail with the new error code `settings_conflict` when they
   differ;
3. build the next settings, persist them, and only after the write succeeds
   install them and add 1 to the revision.

So a failed disk write leaves the settings and the revision unchanged, and no
other patch can land between the compare and the write.

**Who sends a revision.** The full Settings form always sends the revision it
was seeded from. The main-view chips send single, distinct fields
(`{ answerStyle }`, `{ activeProfileId }`) with no revision. Those merge with
anything committed meanwhile, so they keep working without one.

**Geometry does not bump the revision.** `save_window_bounds` writes through
the same store and the same disk-then-cache rule, but never changes the
revision. Moving the window therefore never makes an open form stale. The
form's own save keeps the bounds because it patches the committed settings.

**Frontend.** `App.applyView` installs a view only when
`view.revision >= held.revision`. It compares inside a functional
`setSettings` update, so it compares against the state React holds when the
update applies. A value captured when the request was sent could be stale,
and racing handlers can never reinstall an older view this way.

`SettingsView` keeps `base`, the view it was seeded from:

- The dirty check compares the draft with `base`.
- A save sends `expectedRevision: base.revision`.
- When a save returns `settings_conflict`, the draft and typed keys are kept,
  Save is disabled, and a banner reads "Settings changed elsewhere — reload.
  Your unsaved edits are kept."
- **Reload** fetches `get_settings` (through the same guard) and rebases the
  draft onto it. Every field the user changed since `base` keeps the user's
  value, and every untouched field takes the new committed value. Profiles
  merge per id: an edited profile stays as typed, and an untouched one takes
  its committed version.
- If a newer view reaches the open form through props, an untouched form
  follows it silently. A form with edits shows the banner at once; if the
  user then undoes those edits, the form follows the newer view and the
  banner clears. Neither happens during a save.

**Effects are reconciled, not replayed** (`core/src/store/effects.rs`). After
every committed save, the shell runs
`EffectReconciler::reconcile(current, os)` on the blocking pool:

- One mutex serializes runs.
- Inside that mutex, each run reads the CURRENT committed settings
  (`DesiredOsState::of(store.get())`).
- Each effect is compared with what the OS was last told, not with the
  patch.
- The hotkey is re-registered only when the desired accelerator differs from
  the last one the OS accepted. A refused registration (combination taken or
  unparseable) is not recorded as applied, and `lib.rs` seeds a refused
  startup hotkey the same way, so the next save retries it: a combination
  another app has since released starts working without a restart. While a
  hotkey is refused, App re-reads `hotkey_status` after every save.
  `HotkeyState` is written inside the run, so `hotkey_status` reports the
  newest attempted registration.
- Always-on-top is set when it differs.
- Dock-under-the-camera fires on the transition into `Camera` only, so an
  unrelated save never moves the window.

Every commit is followed by a run that starts after it. The last run to take
the lock therefore reads a state at least as new as every commit, and the OS
ends on the committed state whatever the interleaving. Earlier runs are
idempotent steps toward it. `lib.rs` applies the startup state before
showing the window and seeds the reconciler with exactly that state (the
hotkey only if the OS accepted it).

**Save lock (R3).** A borderless `<fieldset disabled={saving}>` wraps the
whole form. It covers every field, the key inputs, the profile select,
New/Duplicate/Delete, the call-type select, Save (which reads "Saving…"),
Back and the discard dialog. Escape and Back are refused while saving. A ref
guards against a second submit in the same tick. Success re-seeds the form
from the response. Failure unlocks the form with the draft and the typed keys
intact. Disabling the focused Save button drops focus to `<body>`, so when
the lock lifts focus goes back to Save.

**Local prompt budget (R4)** (`core/src/llm/prompt.rs`):

- `request_input_bytes(system, transcript)` is the count the local gate in
  `local::request_body` enforces. The gate now calls it, so a preview built on
  it cannot drift from the gate.
- `local_prompt_budget(profile, style, question)` builds the request exactly
  as the answer path does (same trimming, style suffix and wrapper). It
  returns `{ usedBytes, limitBytes, remainingBytes, fixedBytes, profileBytes,
  questionBytes, reserveBytes, status }`.
- `status` is `over` when the request exceeds the limit. With an empty
  question it is `over` when not even a one-byte question fits. It is `tight`
  when fewer than `QUESTION_RESERVE_BYTES` (200) remain, and `ok` otherwise.
- The 200-byte reserve is a usability line, not a limit.
- The fixed overhead for interview/Balanced/resume-only is 771 bytes, pinned
  by test.

Where the budget is used:

- **Settings preview.** The new `local_prompt_budget(profile, answerStyle,
  question)` command takes the UNSAVED draft. Settings calls it for the
  profile being edited and the style chosen in the form, only when the local
  provider is selected, debounced by 250 ms. A per-request sequence number
  discards obsolete answers, and a result is shown only for the profile id
  and style it was computed for. `tight` is a warning. `over` is an error note
  saying local recording and Ask are refused, but Save stays enabled so the
  profile can still be kept for a cloud model.
- **Before recording.** `start_session` refuses before the device opens when
  the active profile is `over` with no question. The refusal is the gate's own
  error, verbatim.
- **Typed Ask.** App checks the actual question before `submitAsk`, so a
  question that cannot fit supersedes nothing and stays in the box. `ask`
  repeats the check in the core before claiming a session.
- **After Stop.** The gate in `local::request_body` checks the real
  transcript. Its message and its authority are unchanged.

**Damaged settings file** (`core/src/store/settings.rs`). The load result is
classified:

| Load result | Action |
| --- | --- |
| Not found | Defaults. Saving works. |
| Read OK but not a settings JSON object (bad syntax, not an object, invalid UTF-8, empty) | Rename the file to the first free `settings.json.corrupt-<unix-seconds>[-n]` (never overwriting a backup), then use defaults. `storageWarning` names the copy. Saving works, and the first successful save clears the warning. |
| Invalid content, but the rename failed | Defaults in memory only. Every write, geometry included, is refused, and the original is left byte-identical. `storageWarning` says so. |
| Any other read error (permission, sharing violation, transient) | Same as a failed rename: the file is not known to be damaged, so it is neither renamed nor replaced. |

A parseable object keeps the per-field fallback. App shows `storageWarning`
in the main error box at launch, and Settings shows it as a note.

## Consequences

- A Settings form open while a chip save lands must reload before it can
  save. The rebase keeps typed edits, so the cost is one click.
- The effect step now takes the reconciler lock and hops to the main thread
  for hotkey changes only. Chip saves normally find nothing to do.
- A locked or unreadable settings file makes the app run on defaults without
  saving until restart. That is deliberate: losing profiles and keys is worse
  than an unsaved session.
- Local typed Ask costs one extra IPC round trip before sending. Cloud
  providers skip it.
- `ErrorCode` has fourteen members (ADR 010).

## What would change the call

- Multiple windows or a second client editing settings would call for pushing
  settings-changed events instead of relying on save responses and reloads.
- If the save lock ever became visible in practice (slow disks), a
  per-field draft revision merge could replace it.
- A local model with a larger context would change only
  `local::MAX_INPUT_BYTES`. The budget and every check follow it.

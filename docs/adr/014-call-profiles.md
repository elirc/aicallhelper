# ADR 014 — Call profiles: one active grounding bundle, migrated from the flat resume/JD, additive to the pinned prompt

**Status**: accepted

## Context

v3 held exactly one resume and one job description. A person running two
searches at once (a React role on Monday, a Rust role on Tuesday) had to
re-paste the JD into Settings before every call — and the failure mode when
they forgot was the worst one this product can have: an answer confidently
grounded in the **wrong** job description, delivered in about a second.
Non-interview calls (sales, support, a work meeting) got an interview-shaped
prompt: "THE JOB THEY ARE INTERVIEWING FOR" (audit PROF-01).

Three existing rules shaped the answer:

- **ADR 007**: the resume + JD block is the stable-per-call part of the
  prompt, cached by byte-prefix; every pinned string is product behaviour and
  must not change; the prefix must be byte-stable and deterministic.
- **The anti-goal "no personas"** (IDEAS.md): the app tells the user what to
  say as themselves. Free-text "who the AI is" headed for the system prompt is
  unpinnable and the first step of generic-chatbot drift.
- **SPEC §8**: per-field fallback, atomic write, disk-then-cache, and a
  settings file that survives hand edits and upgrades.

## Decision

- **A profile is a self-contained grounding bundle**, not a persona:
  `{ id, name, callType, resume, jobDescription, focus, extraInstructions }`
  (`src-tauri/core/src/store/mod.rs:41-96`, `src/types.ts`). At most 8;
  exactly one is active. `callType` is a closed enum
  (`interview | sales | support | meeting | other`, unknown → interview) with
  one pinned sentence per non-interview variant; `focus` and
  `extraInstructions` are capped at 2 000 characters each; resume and JD keep
  the v3 200 000-character cap and are stored verbatim (never trimmed).
- **One pure function writes the invariant**:
  `settings::normalize_profiles(profiles, requested, previous)`
  (`core/src/store/settings.rs:186-253`) — truncate to 8, never empty (an
  empty list becomes the Default profile), names trimmed/capped/never blank
  ("Untitled"), text fields capped by characters, an invalid or duplicate id
  repaired to the smallest unused `p<n>` in list order, and the active id
  resolved as *requested if known, else previous if known, else the first*.
  It is deterministic (a `BTreeSet` for membership only, no clock, no random
  ids) and idempotent — pinned by run-twice and normalize-twice tests —
  because it sits on the path from settings to the cached prefix and ADR 007
  forbids nondeterminism anywhere on that path. Both the disk loader and
  `apply_patch` go through it and nothing else touches the list.
- **The v3 flat file migrates on read and is never written back**
  (`settings.rs:298-318`, `422-433`): a file whose `profiles` is not a JSON
  array (v3, missing, or corrupt) loads as ONE `Default` interview profile
  carrying the top-level `resume`/`jobDescription`; keys, hotkey and bounds
  are untouched; the first save writes only the new shape. Inside a profile
  the §8 rule holds per field (a bad `callType` reads as interview, a bad
  string as `""`); only a non-object entry is dropped, because there is
  nothing in it to save.
- **Two patch shapes, one contract** (`store/mod.rs:277-316`): the Settings
  form sends the WHOLE array plus the selected id (it owns the draft, and the
  core repairs ids and trims names — the form re-seeds from the returned
  view); the main-view chips send `activeProfileId` ALONE, and
  `apply_patch` re-runs the normaliser over the *stored* list so a switch
  never rewrites a byte of profile text. A switch to an id that no longer
  exists keeps the current active profile — the chips mirror the persisted
  id, never the click (SPEC §9), so the UI tells the truth either way.
  Profile entries deserialize leniently (every field `#[serde(default)]`),
  while the scalar enum fields are typed and reject an invalid wire value
  outright (SPEC §4).
- **The prompt change is additive** (ADR 007, `core/src/llm/prompt.rs`):
  nine new constants beside the pinned ones — four call-type lines, the
  `ABOUT THE USER` / `CONTEXT FOR THIS CALL` headers and their grounding note
  for non-interview calls, `WHAT TO EMPHASIZE`, `ADDITIONAL INSTRUCTIONS FROM
  THE USER`. `sections_for(call_type)` is a straight-line match; the prefix
  order is role · call line · resume · JD · grounding (iff resume or JD) ·
  focus · extra. An interview profile with empty focus/extra builds a prefix
  **byte-identical to v3** — pinned by
  `migrated_v3_profile_yields_a_byte_identical_prefix` — so no existing user
  pays a cache write on upgrade and no pinned string moved.
- **The prompt is built per session from a settings snapshot**
  (`src-tauri/src/commands.rs:358-360`, `system_prompt_for`): a switch during
  a recording applies to the next session, exactly like a style change.
  Nothing on the stop-to-first-word path changed.
- **Local mode counts the whole profile**: focus and extra instructions ride
  in the prefix, so they count toward the 7 KB `MAX_INPUT_BYTES`; the
  oversize message names every field and says "active profile" (PROF-08,
  `core/src/llm/local.rs:19-24`), and Settings shows a live UTF-8 byte
  counter for the profile while local mode is selected, warning above
  6 500 bytes (PROF-02).
- **UI**: `ProfileChips` on the main view (hidden with fewer than two
  profiles, `aria-pressed` mirrors the persisted id, click on the active chip
  is a no-op); Settings gets a Profile select with New / Duplicate / Delete
  editing the selected profile in place, call-type-dependent labels
  ("Resume" vs "About you", "Job description" vs "Call context"), and a
  dirty guard so a stray Escape cannot discard several profiles of edits
  (PROF-03). The Interview-prep library hides for non-interview profiles
  (PROF-05); a hint says when the active profile has neither resume nor JD,
  so answers are knowingly ungrounded (PROF-06).

## Consequences

- One click switches every grounding input for the next call; Duplicate
  covers "same resume, new JD" in one gesture, which is the 90 % case, so a
  shared-resume/many-JD graph was not needed.
- Every prompt byte stays product-owned and verbatim-tested: the only user
  text is grounding text (resume, JD, focus, extra), and the only new prompt
  sentences are enum-selected constants.
- **A profile switch is a deliberate cache write** (ADR 007): a different
  prefix costs at most one 1.25× write on the next question, only for
  profiles above Haiku's 4 096-token minimum, and it happens between calls by
  design. That is exactly why per-question profile switching is not built.

Costs, honestly:

- **Every save and switch round-trips every profile's full text over IPC**
  (worst case 8 × ~400 KB; realistic profiles are tens of KB on a local
  WebView2 channel). Accepted for v1 (PROF-07); the escape hatch is a light
  `SettingsSummary` for switches if it ever measures.
- **Downgrade is lossy**: after the first save the file has no top-level
  `resume`/`jobDescription`, so an older build reading it sees an empty
  profile. Acceptable for a single-user app; the keys survive.
- **Extra instructions can contradict the app's own role text.** That is
  the user's prerogative over their own answers, not a trust boundary —
  profile text is user-typed, same trust class as the resume, never logged
  (SPEC §11).
- Test churn was large by design: every `Profile { .. }` literal, the
  settings corrupt-field matrix, and every frontend fixture moved with the
  shape. The pinned prompt tests did not change meaning.

Explicitly not built: auto-detecting the call type or profile from the
transcript (a wrong guess silently re-grounds the answer, and it works
inside the latency window — selection is a deliberate gesture, like Stop);
per-question or modifier-key profile switching (a cache write per question);
a per-profile default answer style (a second source of truth for the chips'
`aria-pressed` contract); free-text call types (unpinnable prompt text);
cloud sync, CRM, ATS import, team profiles (anti-goals); per-profile keys,
providers or hotkeys (those are per-user); JD-from-URL inside the editor
(IDEAS #13, its own posture change).

## If revisited

If switch latency ever measures, return a summary view for switch/style
patches and keep the full view for Settings. If users report flipping the
style after every switch, a per-profile default style is the smallest
addition — but it must write through the same persisted `answerStyle` the
chips mirror, never a second field. If a call type ever needs more than one
pinned sentence, add constants, not free text: the enum is what keeps the
prompt testable.

> **Historical material: archived proposal (2026-09-18).** A design study, not a description of the shipped app. The decisions it fed are in `../DESIGN.md` and `../AUDIT.md`; runtime profile-budget validation for local mode was still unfinished as of 2026-09-22.

=== verdict
build-lite

=== recommendation
Build the lean version of call profiles ("build-lite"): a profile is a named, self-contained bundle {name, callType, resume, jobDescription, focus, extraInstructions}; exactly one is active; the main view gets a chip row (StyleChips pattern) that patches ONLY activeProfileId; Settings gets a Profile select + New/Duplicate/Delete and edits the selected profile in place, saving the whole array plus the selected id on Save. The v3 flat resume/jobDescription file is read once into a "Default" interview profile and never written back. The prompt gains only ADDITIVE constants (call-type line, background/context headers, call grounding note, focus and extra-instructions sections); a migrated interview profile with no focus/extra produces a cached_prefix byte-identical to v3, so no pinned string changes, no existing prompt test changes meaning, and no user pays a cache write on upgrade. Do NOT build: per-profile default answer style (v1), free-text call types, auto-detection from the transcript, per-question switching, cloud sync/CRM. Effort M (core ~1 session, frontend ~1-2, docs ~0.5).

=== rationale
The problem is real and specific: with ONE resume + ONE JD (store/mod.rs:25-28, commands.rs:292-296 build_deps reads settings.resume/job_description), a person running two searches (React/TS Monday, Rust Tuesday) must re-paste a JD into Settings before every call, and the failure mode when they forget is the worst one this product can have: an answer confidently grounded in the WRONG job description, delivered in ~1 s. The prompt already treats resume+JD as the stable-per-call block (ADR 007), so "which stable block" is the natural unit of switching, and switching is rare and between calls. The cache split makes the switch cost trivially bounded: a different prefix = at most one cache write at 1.25x, and only for profiles over Haiku's 4096-token minimum (anthropic.rs:84-90) — typical profiles were never cached anyway. Nothing touches machine.rs, the audio path, or the stop-to-first-word window: the prompt is built once at start_session/ask from a settings snapshot, so a switch mid-recording applies to the next session exactly like a style change today.

The heavy version would betray the product's anti-goals (IDEAS.md "No personas", "generic chatbot drift"): free-text call types and per-question free text head straight for unpinnable system-prompt text (IDEAS #1 "Don't build"). Framing profiles as "what call this is" (context) rather than "who the AI is" (persona), with an enum for call type and pinned per-variant sentences, keeps every byte of the prompt product-owned and testable. Per-profile default answer style is deferred: the chips' aria-pressed contract (§9) mirrors ONE persisted style; a profile default would make the switch action write two fields and create a second source of truth for the same chip. Duplicate covers "same resume, new JD" in one click, which is the 90% case, so resume-per-profile (self-contained bundles) beats a shared-resume/many-JD graph.

=== design
## 1. Data model (core, src-tauri/core/src/store/mod.rs)

New constants:
- MAX_PROFILES = 8
- MAX_PROFILE_NAME_CHARS = 60
- MAX_PROFILE_ID_CHARS = 40 (charset [A-Za-z0-9_-])
- MAX_FOCUS_CHARS = 2_000
- MAX_EXTRA_INSTRUCTIONS_CHARS = 2_000
- MAX_PROFILE_CHARS (200_000) unchanged, now per profile for resume and jobDescription
- DEFAULT_PROFILE_ID = "default", DEFAULT_PROFILE_NAME = "Default", UNNAMED_PROFILE_NAME = "Untitled"

New enum `CallType { Interview (default), Sales, Support, Meeting, Other }` — lives in llm/prompt.rs beside AnswerStyle (same as_str / parse_or_default pattern; unknown -> Interview, the v3 behavior), re-exported from llm, imported by store like AnswerStyle is today.

New struct `CallProfile { id, name, call_type: CallType, resume, job_description, focus, extra_instructions }` (Serialize+Deserialize, camelCase). `CallProfile::empty(id, name)` and `CallProfile::as_prompt(&self) -> Profile<'_>` (borrows; the 200 KB resume is never cloned into the prompt builder).

`Settings` loses `resume`/`job_description`; gains `profiles: Vec<CallProfile>` and `active_profile_id: String`. Default = one empty Default profile, active "default". Invariant (enforced by ONE pure function, `settings::normalize_profiles`, the only writer): 1..=8 profiles, ids valid+unique, every text field capped, active names an existing profile. `Settings::active_profile(&self) -> &CallProfile` (find by id -> first -> a `static EMPTY_PROFILE` so it never panics even on a hand-built empty vec).

`SettingsView` (Rust and TS): replace `resume`/`jobDescription` with `profiles: CallProfile[]` + `activeProfileId: string`. `SettingsPatch`: replace `resume`/`jobDescription` with `profiles?: CallProfile[]` (whole-array replace) + `activeProfileId?: string`. A `CallProfilePatch` deserialization type with every field `#[serde(default)]` (callType as String -> parse_or_default) so a partial object from the UI never fails the whole patch.

## 2. JSON on disk (new shape; only shape ever written)

{ "profiles": [ { "id": "default", "name": "Default", "callType": "interview", "resume": "...", "jobDescription": "...", "focus": "", "extraInstructions": "" }, { "id": "8c1e...-uuid", "name": "Rust / systems", "callType": "interview", "resume": "...", "jobDescription": "...", "focus": "Rust, tokio, async, low-latency audio", "extraInstructions": "" } ], "activeProfileId": "default", "alwaysOnTop": true, "llmProvider": "anthropic", "answerStyle": "balanced", "hotkey": "Ctrl+Shift+Space", "deepgramKey": "enc:...", "anthropicKey": "enc:...", "windowBounds": {...} }

## 3. Migration + per-field fallback (settings.rs)

settings_from_disk: if `profiles` is a JSON array -> each object entry goes through `profile_from_disk` (per-field: bad callType -> interview, bad/missing string -> "", non-object entry dropped — nothing in it to save); `activeProfileId` read as requested. Otherwise (v3 flat file, missing key, or a corrupt non-array value) -> ONE legacy profile built from top-level `resume`/`jobDescription` via the existing `profile_field` (id "default", name "Default", interview). Then `normalize_profiles(profiles, requested_active, previous=None)`. Keys, hotkey, bounds etc. are untouched by migration. to_disk_json writes `profiles` + `activeProfileId` and does NOT write `resume`/`jobDescription` — the file upgrades on first save.

normalize_profiles rules (pure, deterministic — same input, same output, so it can never introduce prefix nondeterminism): truncate to 8; empty -> push Default; name trimmed, capped at 60, "" -> "Untitled"; resume/JD capped 200k VERBATIM (not trimmed — §8 rule preserved); focus/extra capped 2000; invalid or duplicate id -> repaired to the smallest unused "p<n>"; active = requested if it names a profile, else previous if it does, else profiles[0].id.

apply_patch: if `profiles` or `active_profile_id` present -> proposed = patch.profiles mapped through CallProfile::from_patch, else current profiles; (profiles, active) = normalize_profiles(proposed, patch.active_profile_id, Some(current active)). A quick switch to an id that no longer exists therefore keeps the current active and the returned view tells the truth (the chips mirror the persisted value). Everything else in apply_patch unchanged (disk first, cache second).

## 4. Prompt (llm/prompt.rs) — additive only; every pinned string untouched

`Profile<'a>` gains `call_type: CallType`, `focus: &'a str`, `extra_instructions: &'a str` (still Copy + Default; existing literal constructions add `..Default::default()`).

New constants, exact text:
- CALL_TYPE_SALES = "\n\nThis is a sales call: the user is selling to the other person. Answer as the user speaking to a prospect or customer — specific, helpful, and never pushy."
- CALL_TYPE_SUPPORT = "\n\nThis is a customer support call: the user is helping the other person. Answer as the user speaking to a customer — calm, clear, and focused on resolving their issue."
- CALL_TYPE_MEETING = "\n\nThis is a work meeting: the user is a participant, not a candidate. Answer as the user speaking to colleagues — direct and to the point."
- CALL_TYPE_OTHER = "\n\nThis is a general call, not a job interview. Answer as the user speaking to the other person."
- BACKGROUND_HEADER = "\n\n--- ABOUT THE USER ---\n"
- CONTEXT_HEADER = "\n\n--- CONTEXT FOR THIS CALL ---\n"
- GROUNDING_NOTE_CALL = "\n\nGround every answer in the background and call context above. Never invent experience or facts the background does not support."
- FOCUS_HEADER = "\n\n--- WHAT TO EMPHASIZE ---\n"
- EXTRA_INSTRUCTIONS_HEADER = "\n\n--- ADDITIONAL INSTRUCTIONS FROM THE USER ---\n"

`sections_for(call_type) -> (call_line, resume_header, jd_header, grounding)`: Interview -> ("", RESUME_HEADER, JD_HEADER, GROUNDING_NOTE); every other type -> (its CALL_TYPE_* line, BACKGROUND_HEADER, CONTEXT_HEADER, GROUNDING_NOTE_CALL). Straight-line match, no maps.

cached_prefix order (all stable per profile, so all INSIDE the cached block): ROLE_INSTRUCTIONS · [call-type line, non-interview only] · [resume/background header + trimmed resume] · [jd/context header + trimmed JD] · [grounding note if resume or JD present — focus alone does NOT trigger it] · [FOCUS_HEADER + trimmed focus] · [EXTRA_INSTRUCTIONS_HEADER + trimmed extra]. style_suffix unchanged, after the breakpoint. user message unchanged. Property to pin: a migrated v3 profile (interview, empty focus/extra) builds a byte-identical prefix to today.

Cache: a profile switch changes the prefix -> at most one cache write (1.25x) on the next question, only above 4096 tokens; nothing on the stop-to-first-word path changes (prompt built at start_session from a snapshot; concatenation of a few KB is microseconds). Local provider: focus+extra count toward its 7 KB MAX_INPUT_BYTES (local.rs:15) — its error text should say "profile" (see finding PROF-08).

## 5. Shell (src-tauri/src/commands.rs)

build_deps: `let system = build_system_prompt(settings.active_profile().as_prompt(), settings.answer_style);` — that is the whole change. set_settings unchanged (hotkey/always-on-top side effects still diff the view).

## 6. types.ts additions (exact)

export type CallType = 'interview' | 'sales' | 'support' | 'meeting' | 'other';
export interface CallProfile { id: string; name: string; callType: CallType; resume: string; jobDescription: string; focus: string; extraInstructions: string; }
SettingsView: remove resume/jobDescription; add `profiles: CallProfile[]; activeProfileId: string;`
SettingsPatch: remove resume/jobDescription; add `profiles?: CallProfile[]` (doc: whole-array replace, the Settings form owns the draft) and `activeProfileId?: string` (doc: sent ALONE by the main-view switcher — a switch never rewrites profile text).
export const MAX_PROFILES = 8; MAX_PROFILE_NAME_CHARS = 60; MAX_FOCUS_CHARS = 2000; MAX_EXTRA_INSTRUCTIONS_CHARS = 2000;
Bridge unchanged (setSettings already takes a patch).

## 7. UI

App.tsx: `const selectProfile = useCallback((id: string) => applySettings({ activeProfileId: id }), [applySettings]);` passed to MainView as `onSelectProfile`.

MainView.tsx: new `ProfileChips` (src/components/ProfileChips.tsx, copied from StyleChips) rendered directly under the header, before the local-mode banner: one chip per profile, `role="group" aria-label="Call profile"`, aria-pressed mirrors `settings.activeProfileId` (never the clicked chip), disabled while saving, click ignored when already active, renders nothing with < 2 profiles (no clutter for the single-search user; same hide rule as the history bar). A failed save -> session.setError (same as style). Optionally hide the "Interview prep" toggle when the active profile's callType !== 'interview' (finding PROF-05). CSS: `.profile-chips { flex-wrap: wrap }`, `.profile-chip { max-width: 160px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap }`.

SettingsView.tsx: draft state `profiles` (copied from props) + `selectedId` (seeded from activeProfileId). The Profile `<select>` (label "Profile") is BOTH "which one I'm editing" and "which is active on Save" — one concept for a 460 px window. Toolbar: New (disabled at 8), Duplicate (copies every field, new id, name + " copy"), Delete (disabled with 1 profile; selects the previous neighbour). Fields for the selected profile: "Profile name" (maxLength 60) · "Call type" select (Job interview / Sales call / Customer support call / Work meeting / Other) · "Focus (optional)" input, placeholder "What to emphasize, e.g. Rust, tokio, async" · resume textarea labelled "Resume" (interview) / "About you (optional)" (other types) · JD textarea labelled "Job description" (interview) / "Call context (account, product, agenda)" (other types) · "Extra instructions (optional)" textarea rows=2. Ids from `crypto.randomUUID()` with a `p-<base36 time><random>` fallback; the core repairs anything invalid. Save sends `{ profiles, activeProfileId: selectedId, alwaysOnTop, llmProvider, answerStyle, hotkey }` plus only touched key fields (unchanged rule). After a successful save re-seed `profiles`/`selectedId` from the returned view (ids may have been repaired; main is the source of truth, the form is a proposal). Nothing persists until Save; changing the select without saving changes nothing. First-run copy (StatusLine) keys off keys only and is untouched.

## 8. Tests to add

core prompt.rs: migrated_v3_profile_yields_a_byte_identical_prefix · call_type_lines_and_new_headers_are_verbatim (pins all 9 new constants) · interview_emits_no_call_type_line · non_interview_uses_background_context_headers_and_call_grounding_note (and never RESUME_HEADER/JD_HEADER/GROUNDING_NOTE) · sections_appear_in_call_resume_jd_grounding_focus_extra_order · whitespace_only_focus_and_extra_count_as_absent · focus_alone_does_not_trigger_the_grounding_note · unknown_call_type_falls_back_to_interview · extend style_lives_outside_the_cached_prefix and prompt_is_byte_stable_across_repeated_builds to a full Sales profile with focus+extra.

core store (mod.rs/settings.rs): first_run_loads_one_empty_default_profile · v3_flat_file_migrates_into_one_default_interview_profile (keys survive) · migrated_file_is_rewritten_in_the_new_shape_only (no top-level resume/jobDescription after save) · profiles_round_trip_through_disk · each_corrupt_profile_field_falls_back_alone (callType 7 -> interview; resume 42 -> ""; name null -> "Untitled") · non_object_profile_entries_are_dropped_and_the_rest_survive · corrupt_profiles_value_loads_as_default_profile_without_losing_keys · empty_profiles_array_yields_a_default_profile · more_than_max_profiles_are_truncated · duplicate_or_invalid_ids_are_repaired_deterministically (run twice, equal) · unknown_active_id_falls_back_to_first_on_load · unknown_active_id_keeps_current_active_on_patch · switch_patch_changes_only_active_profile_id (profiles text, keys, hotkey byte-equal before/after) · replacing_away_the_active_profile_falls_back_unless_patch_names_a_new_active · per_profile_caps_apply_on_load_and_on_patch (name 60, focus/extra 2000, resume 200k chars not bytes) · active_profile_resolves_by_id_and_falls_back_to_first · as_prompt_borrows_every_field. Adapt: good_value/good_settings, each_corrupt_field_falls_back_alone (8 cases -> profiles cases), patch_round_trips_through_disk, over_length_resume_*, truncation_counts_characters_not_bytes, resume_is_stored_verbatim_never_trimmed, corrupt_answer_style_never_costs_the_resume_or_the_keys, undecryptable_key_* (assert via active_profile().resume).

frontend: MainView "profile chips": hidden with one profile; shown with two; pressed mirrors activeProfileId not the click; click calls onSelectProfile('b'); failed save surfaces in the error box · App: "a profile switch sends ONLY activeProfileId" (expect bridge.setSettings toHaveBeenLastCalledWith({ activeProfileId: 'b' })) and lights the chip the save returned · SettingsView: edits only the selected profile (profiles[0] deep-equal unchanged, [1].focus changed, activeProfileId === 'b') · New adds and selects, disabled at 8 · Duplicate copies fields with a new id and " copy" name · Delete removes the selected, selects a neighbour, disabled at one · labels switch with call type · existing "omits untouched key fields but always sends the rest of the form" now asserts patch.profiles and patch.activeProfileId · testUtils.baseSettings gains profiles/activeProfileId (FakeBridge.setSettings already spreads unknown fields).

## 9. Docs to update

SPEC §7 (new optional sections + call-type header set; pinned strings unchanged), §8 (fields, caps, migration rule, "switch never rewrites text"), §9 (chip row; Settings view list) · TESTING.md (counts + new rows) · DEVELOPMENT.md "Changing the prompt safely" (place by volatility: per-profile -> prefix; note the v3-byte-identity test) · ARCHITECTURE.md:540 · README quick start (step 4) · new ADR 008 "Call profiles: one active bundle, migrated from the flat resume/JD, call-type headers added beside the pinned strings" · IDEAS.md: add to Don't build: auto-detected call type, per-question profile switching.

=== code_sketch
// ───────────── src-tauri/core/src/llm/prompt.rs (additions) ─────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallType { #[default] Interview, Sales, Support, Meeting, Other }

impl CallType {
    pub fn as_str(self) -> &'static str {
        match self { CallType::Interview => "interview", CallType::Sales => "sales",
                     CallType::Support => "support", CallType::Meeting => "meeting", CallType::Other => "other" }
    }
    /// Unknown / corrupt value falls back to interview — exactly v3's behavior (§8).
    pub fn parse_or_default(raw: &str) -> Self {
        match raw { "sales" => CallType::Sales, "support" => CallType::Support,
                    "meeting" => CallType::Meeting, "other" => CallType::Other, _ => CallType::Interview }
    }
}

// ADDED beside the pinned strings; nothing pinned changes.
pub const CALL_TYPE_SALES: &str = "\n\nThis is a sales call: the user is selling to the other person. Answer as the user speaking to a prospect or customer — specific, helpful, and never pushy.";
pub const CALL_TYPE_SUPPORT: &str = "\n\nThis is a customer support call: the user is helping the other person. Answer as the user speaking to a customer — calm, clear, and focused on resolving their issue.";
pub const CALL_TYPE_MEETING: &str = "\n\nThis is a work meeting: the user is a participant, not a candidate. Answer as the user speaking to colleagues — direct and to the point.";
pub const CALL_TYPE_OTHER: &str = "\n\nThis is a general call, not a job interview. Answer as the user speaking to the other person.";
pub const BACKGROUND_HEADER: &str = "\n\n--- ABOUT THE USER ---\n";
pub const CONTEXT_HEADER: &str = "\n\n--- CONTEXT FOR THIS CALL ---\n";
pub const GROUNDING_NOTE_CALL: &str = "\n\nGround every answer in the background and call context above. Never invent experience or facts the background does not support.";
pub const FOCUS_HEADER: &str = "\n\n--- WHAT TO EMPHASIZE ---\n";
pub const EXTRA_INSTRUCTIONS_HEADER: &str = "\n\n--- ADDITIONAL INSTRUCTIONS FROM THE USER ---\n";

#[derive(Debug, Clone, Copy, Default)]
pub struct Profile<'a> {
    pub call_type: CallType,
    pub resume: &'a str,
    pub job_description: &'a str,
    pub focus: &'a str,
    pub extra_instructions: &'a str,
}

/// Header set per call type. A straight-line match — no maps, no clock — so
/// the prefix stays byte-stable (ADR 007). Interview returns v3's exact set.
fn sections_for(call_type: CallType) -> (&'static str, &'static str, &'static str, &'static str) {
    match call_type {
        CallType::Interview => ("", RESUME_HEADER, JD_HEADER, GROUNDING_NOTE),
        CallType::Sales    => (CALL_TYPE_SALES,   BACKGROUND_HEADER, CONTEXT_HEADER, GROUNDING_NOTE_CALL),
        CallType::Support  => (CALL_TYPE_SUPPORT, BACKGROUND_HEADER, CONTEXT_HEADER, GROUNDING_NOTE_CALL),
        CallType::Meeting  => (CALL_TYPE_MEETING, BACKGROUND_HEADER, CONTEXT_HEADER, GROUNDING_NOTE_CALL),
        CallType::Other    => (CALL_TYPE_OTHER,   BACKGROUND_HEADER, CONTEXT_HEADER, GROUNDING_NOTE_CALL),
    }
}

pub fn build_system_prompt(profile: Profile<'_>, style: AnswerStyle) -> SystemPrompt {
    let (call_line, resume_header, jd_header, grounding) = sections_for(profile.call_type);
    // Edges only, as before: interior formatting survives verbatim (§7).
    let resume = profile.resume.trim();
    let jd = profile.job_description.trim();
    let focus = profile.focus.trim();
    let extra = profile.extra_instructions.trim();

    let mut prefix = String::with_capacity(
        ROLE_INSTRUCTIONS.len() + call_line.len() + resume.len() + jd.len() + focus.len() + extra.len() + 512,
    );
    prefix.push_str(ROLE_INSTRUCTIONS);
    prefix.push_str(call_line); // "" for interview: a migrated v3 profile is byte-identical
    if !resume.is_empty() { prefix.push_str(resume_header); prefix.push_str(resume); }
    if !jd.is_empty()     { prefix.push_str(jd_header);     prefix.push_str(jd); }
    // Grounding still keys off resume/JD only — a focus line is not something to ground in.
    if !resume.is_empty() || !jd.is_empty() { prefix.push_str(grounding); }
    if !focus.is_empty()  { prefix.push_str(FOCUS_HEADER); prefix.push_str(focus); }
    if !extra.is_empty()  { prefix.push_str(EXTRA_INSTRUCTIONS_HEADER); prefix.push_str(extra); }

    let style_suffix = match style { AnswerStyle::Brief => STYLE_BRIEF, AnswerStyle::Balanced => STYLE_BALANCED, AnswerStyle::Detailed => STYLE_DETAILED };
    SystemPrompt { cached_prefix: prefix, style_suffix: style_suffix.to_string() }
}

#[test]
fn migrated_v3_profile_yields_a_byte_identical_prefix() {
    // The upgrade must not change what the app says and must not cost every
    // existing user a cache write: interview + no focus + no extra == v3 bytes.
    let p = build_system_prompt(Profile { resume: "R", job_description: "J", ..Default::default() }, AnswerStyle::Balanced);
    assert_eq!(p.cached_prefix, format!("{ROLE_INSTRUCTIONS}{RESUME_HEADER}R{JD_HEADER}J{GROUNDING_NOTE}"));
}

#[test]
fn sections_appear_in_call_resume_jd_grounding_focus_extra_order() {
    let p = build_system_prompt(Profile { call_type: CallType::Sales, resume: "R", job_description: "J", focus: "F", extra_instructions: "E" }, AnswerStyle::Balanced);
    let at = |s: &str| p.cached_prefix.find(s).unwrap();
    assert!(at(CALL_TYPE_SALES) < at(BACKGROUND_HEADER) && at(BACKGROUND_HEADER) < at(CONTEXT_HEADER)
         && at(CONTEXT_HEADER) < at(GROUNDING_NOTE_CALL) && at(GROUNDING_NOTE_CALL) < at(FOCUS_HEADER)
         && at(FOCUS_HEADER) < at(EXTRA_INSTRUCTIONS_HEADER));
    assert!(!p.cached_prefix.contains(RESUME_HEADER) && !p.cached_prefix.contains(JD_HEADER) && !p.cached_prefix.contains(GROUNDING_NOTE));
}

// ───────────── src-tauri/core/src/store/mod.rs (additions/changes) ─────────────
pub const MAX_PROFILES: usize = 8;
pub const MAX_PROFILE_NAME_CHARS: usize = 60;
pub const MAX_PROFILE_ID_CHARS: usize = 40;
pub const MAX_FOCUS_CHARS: usize = 2_000;
pub const MAX_EXTRA_INSTRUCTIONS_CHARS: usize = 2_000;
pub const DEFAULT_PROFILE_ID: &str = "default";
pub const DEFAULT_PROFILE_NAME: &str = "Default";
pub const UNNAMED_PROFILE_NAME: &str = "Untitled";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallProfile {
    pub id: String,
    pub name: String,
    pub call_type: CallType,
    /// Stored verbatim, NOT trimmed (§8) — same rule as the v3 resume.
    pub resume: String,
    pub job_description: String,
    pub focus: String,
    pub extra_instructions: String,
}

impl CallProfile {
    pub fn empty(id: &str, name: &str) -> Self {
        Self { id: id.into(), name: name.into(), call_type: CallType::Interview,
               resume: String::new(), job_description: String::new(), focus: String::new(), extra_instructions: String::new() }
    }
    /// Borrow into the prompt builder's view — the 200 KB resume is never cloned.
    pub fn as_prompt(&self) -> Profile<'_> {
        Profile { call_type: self.call_type, resume: &self.resume, job_description: &self.job_description,
                  focus: &self.focus, extra_instructions: &self.extra_instructions }
    }
    fn from_patch(p: CallProfilePatch) -> Self {
        Self { id: p.id, name: p.name, call_type: CallType::parse_or_default(p.call_type.trim()),
               resume: p.resume, job_description: p.job_description, focus: p.focus, extra_instructions: p.extra_instructions }
    }
}

/// String::new() is const, so this needs no OnceLock: active_profile() can never panic.
static EMPTY_PROFILE: CallProfile = CallProfile { id: String::new(), name: String::new(), call_type: CallType::Interview,
    resume: String::new(), job_description: String::new(), focus: String::new(), extra_instructions: String::new() };

pub struct Settings {
    /// Invariant (enforced by settings::normalize_profiles, the only writer):
    /// never empty, <= MAX_PROFILES, ids unique, active_profile_id names one of them.
    pub profiles: Vec<CallProfile>,
    pub active_profile_id: String,
    pub always_on_top: bool, /* … rest unchanged … */
}

impl Settings {
    pub fn active_profile(&self) -> &CallProfile {
        self.profiles.iter().find(|p| p.id == self.active_profile_id)
            .or_else(|| self.profiles.first())
            .unwrap_or(&EMPTY_PROFILE)
    }
}

/// Every field defaults so a partial object never fails the whole patch (§8 per-field fallback at the IPC edge).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CallProfilePatch { pub id: String, pub name: String, pub call_type: String, pub resume: String,
                              pub job_description: String, pub focus: String, pub extra_instructions: String }

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsPatch {
    /// Whole-array replace: the Settings form owns the draft. None leaves profiles alone.
    pub profiles: Option<Vec<CallProfilePatch>>,
    /// Sent ALONE by the main-view switcher — a switch never rewrites profile text.
    pub active_profile_id: Option<String>,
    pub always_on_top: Option<bool>, /* … rest unchanged; resume/job_description removed … */
}

// ───────────── src-tauri/core/src/store/settings.rs ─────────────
/// The ONE place profile invariants are enforced. Pure: both the disk loader
/// and apply_patch go through it, and it is deterministic (same input, same
/// output), so it can never smuggle nondeterminism into the cached prefix.
pub fn normalize_profiles(mut profiles: Vec<CallProfile>, requested: Option<&str>, previous: Option<&str>) -> (Vec<CallProfile>, String) {
    profiles.truncate(MAX_PROFILES);
    if profiles.is_empty() { profiles.push(CallProfile::empty(DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME)); }
    let mut seen = std::collections::HashSet::new();
    let mut needs_id = Vec::new();
    for (i, p) in profiles.iter_mut().enumerate() {
        let name = cap_chars(p.name.trim(), MAX_PROFILE_NAME_CHARS);
        p.name = if name.is_empty() { UNNAMED_PROFILE_NAME.to_string() } else { name };
        p.resume = cap_chars(&p.resume, MAX_PROFILE_CHARS);              // verbatim, capped
        p.job_description = cap_chars(&p.job_description, MAX_PROFILE_CHARS);
        p.focus = cap_chars(&p.focus, MAX_FOCUS_CHARS);
        p.extra_instructions = cap_chars(&p.extra_instructions, MAX_EXTRA_INSTRUCTIONS_CHARS);
        if !(valid_id(&p.id) && seen.insert(p.id.clone())) { needs_id.push(i); }
    }
    let mut n = 1;
    for i in needs_id {
        let id = loop { let c = format!("p{n}"); n += 1; if !seen.contains(&c) { break c; } };
        seen.insert(id.clone());
        profiles[i].id = id;
    }
    let known = |id: Option<&str>| id.filter(|id| profiles.iter().any(|p| p.id == *id)).map(str::to_string);
    let active = known(requested).or_else(|| known(previous)).unwrap_or_else(|| profiles[0].id.clone());
    (profiles, active)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_PROFILE_ID_CHARS
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

// in settings_from_disk(), replacing the two profile_field lines:
let (profiles, requested) = match obj.get("profiles") {
    Some(Value::Array(items)) => (items.iter().filter_map(profile_from_disk).collect::<Vec<_>>(),
                                  obj.get("activeProfileId").and_then(Value::as_str)),
    // v3 flat file (or a corrupt `profiles` value): the legacy resume/JD
    // become one "Default" interview profile. This shape is never written back.
    _ => (vec![CallProfile { resume: profile_field(obj.get("resume")),
                             job_description: profile_field(obj.get("jobDescription")),
                             ..CallProfile::empty(DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME) }], None),
};
let (profiles, active_profile_id) = normalize_profiles(profiles, requested, None);

/// Per-field fallback INSIDE a profile: a bad callType is interview, a bad
/// string is "". Only a non-object entry is dropped — there is nothing in it to save.
fn profile_from_disk(v: &Value) -> Option<CallProfile> {
    let o = v.as_object()?;
    let s = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    Some(CallProfile { id: s("id"), name: s("name"),
        call_type: o.get("callType").and_then(Value::as_str).map(CallType::parse_or_default).unwrap_or_default(),
        resume: s("resume"), job_description: s("jobDescription"), focus: s("focus"), extra_instructions: s("extraInstructions") })
}

// in apply_patch(), replacing the resume/job_description arms:
if patch.profiles.is_some() || patch.active_profile_id.is_some() {
    let proposed = match patch.profiles {
        Some(list) => list.into_iter().map(CallProfile::from_patch).collect(),
        None => next.profiles.clone(),
    };
    let (profiles, active) = normalize_profiles(proposed, patch.active_profile_id.as_deref(), Some(&next.active_profile_id));
    next.profiles = profiles;
    next.active_profile_id = active;
}

// in to_disk_json(): `resume`/`jobDescription` are NOT written — the file upgrades on first save.
let profiles = s.profiles.iter().map(|p| {
    let mut o = Map::new();
    o.insert("id".into(), Value::String(p.id.clone()));
    o.insert("name".into(), Value::String(p.name.clone()));
    o.insert("callType".into(), Value::String(p.call_type.as_str().into()));
    o.insert("resume".into(), Value::String(p.resume.clone()));
    o.insert("jobDescription".into(), Value::String(p.job_description.clone()));
    o.insert("focus".into(), Value::String(p.focus.clone()));
    o.insert("extraInstructions".into(), Value::String(p.extra_instructions.clone()));
    Value::Object(o)
}).collect();
m.insert("profiles".into(), Value::Array(profiles));
m.insert("activeProfileId".into(), Value::String(s.active_profile_id.clone()));

#[test]
fn v3_flat_file_migrates_into_one_default_interview_profile() {
    let dir = tempdir().unwrap();
    write_settings(dir.path(), &json!({ "resume": "Resume text", "jobDescription": "JD text", "deepgramKey": plain("dg-key") }).to_string());
    let got = load(dir.path());
    assert_eq!(got.profiles.len(), 1);
    let p = got.active_profile();
    assert_eq!((p.id.as_str(), p.name.as_str(), p.call_type), ("default", "Default", CallType::Interview));
    assert_eq!((p.resume.as_str(), p.job_description.as_str()), ("Resume text", "JD text"));
    assert_eq!(got.deepgram_key.as_deref(), Some("dg-key")); // migration never touches keys
}

#[test]
fn switch_patch_changes_only_active_profile_id() {
    let dir = tempdir().unwrap();
    let store = SettingsStore::load_from(dir.path());
    store.apply_patch(SettingsPatch { profiles: Some(vec![
        CallProfilePatch { id: "a".into(), name: "A".into(), resume: "RA".into(), ..Default::default() },
        CallProfilePatch { id: "b".into(), name: "B".into(), resume: "RB".into(), ..Default::default() },
    ]), deepgram_key: Some("dg".into()), ..Default::default() }).unwrap();
    let before = store.get();
    store.apply_patch(SettingsPatch { active_profile_id: Some("b".into()), ..Default::default() }).unwrap();
    let after = store.get();
    assert_eq!(after.active_profile_id, "b");
    assert_eq!(after.profiles, before.profiles);        // not one byte of profile text changed
    assert_eq!(after.deepgram_key, before.deepgram_key);
    // A stale switch to a vanished id keeps the current active rather than jumping to the first.
    store.apply_patch(SettingsPatch { active_profile_id: Some("zzz".into()), ..Default::default() }).unwrap();
    assert_eq!(store.get().active_profile_id, "b");
}

// ───────────── src-tauri/src/commands.rs build_deps ─────────────
let system = build_system_prompt(settings.active_profile().as_prompt(), settings.answer_style);

// ───────────── src/types.ts ─────────────
export type CallType = 'interview' | 'sales' | 'support' | 'meeting' | 'other';
export interface CallProfile { id: string; name: string; callType: CallType; resume: string; jobDescription: string; focus: string; extraInstructions: string; }
export interface SettingsView { profiles: CallProfile[]; activeProfileId: string; alwaysOnTop: boolean; llmProvider: LlmProviderKind; answerStyle: AnswerStyle; hotkey: string; hasDeepgramKey: boolean; hasAnthropicKey: boolean; hasGroqKey: boolean; }
export interface SettingsPatch {
  /** Whole-array replace: the Settings form owns the draft and sends all of it. */
  profiles?: CallProfile[];
  /** Sent ALONE by the main-view switcher — a switch never rewrites profile text. */
  activeProfileId?: string;
  alwaysOnTop?: boolean; llmProvider?: LlmProviderKind; answerStyle?: AnswerStyle; hotkey?: string;
  deepgramKey?: string; anthropicKey?: string; groqKey?: string;
}
export const MAX_PROFILES = 8;
export const MAX_PROFILE_NAME_CHARS = 60;
export const MAX_FOCUS_CHARS = 2000;
export const MAX_EXTRA_INSTRUCTIONS_CHARS = 2000;

// ───────────── src/components/ProfileChips.tsx (StyleChips pattern) ─────────────
import { useState } from 'react';
import type { CallProfile } from '../types';

interface ProfileChipsProps {
  profiles: ReadonlyArray<Pick<CallProfile, 'id' | 'name'>>;
  /** The PERSISTED active id — what the next answer will actually use. */
  activeId: string;
  onSelect(id: string): Promise<void>;
}

export function ProfileChips({ profiles, activeId, onSelect }: ProfileChipsProps) {
  const [saving, setSaving] = useState(false);
  // One profile = nothing to switch. Same hide rule as the history bar.
  if (profiles.length < 2) return null;
  async function pick(id: string) {
    if (saving || id === activeId) return; // serialize saves, skip no-ops
    setSaving(true);
    try { await onSelect(id); } finally { setSaving(false); }
  }
  return (
    <div className="style-chips profile-chips" role="group" aria-label="Call profile">
      {profiles.map((p) => (
        <button key={p.id} type="button" className="chip style-chip profile-chip" title={p.name}
          // Pressed mirrors the persisted id, never the clicked chip (§9 discipline).
          aria-pressed={p.id === activeId} disabled={saving} onClick={() => void pick(p.id)}>
          {p.name}
        </button>
      ))}
    </div>
  );
}

// ───────────── src/App.tsx ─────────────
const selectProfile = useCallback((id: string) => applySettings({ activeProfileId: id }), [applySettings]);
// <MainView … onSelectProfile={selectProfile} />

// ───────────── src/views/MainView.tsx (after </header>) ─────────────
<ProfileChips profiles={settings?.profiles ?? []} activeId={settings?.activeProfileId ?? ''}
  onSelect={async (id) => { const env = await onSelectProfile(id); if (!env.ok) session.setError(env.error); }} />

// ───────────── src/views/SettingsView.tsx (draft state) ─────────────
const [profiles, setProfiles] = useState<CallProfile[]>(() => settings.profiles.map((p) => ({ ...p })));
const [selectedId, setSelectedId] = useState(settings.activeProfileId);
const selected = profiles.find((p) => p.id === selectedId) ?? profiles[0];
const interview = selected.callType === 'interview';

function updateSelected(patch: Partial<CallProfile>) {
  setProfiles((list) => list.map((p) => (p.id === selected.id ? { ...p, ...patch } : p)));
}
function newProfileId(): string {
  return typeof crypto !== 'undefined' && 'randomUUID' in crypto
    ? crypto.randomUUID()
    : `p-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`; // core repairs anything invalid
}
function addProfile(from?: CallProfile) {
  if (profiles.length >= MAX_PROFILES) return;
  const id = newProfileId();
  const next: CallProfile = from
    ? { ...from, id, name: `${from.name} copy` }
    : { id, name: 'New profile', callType: 'interview', resume: '', jobDescription: '', focus: '', extraInstructions: '' };
  setProfiles((list) => [...list, next]);
  setSelectedId(id);
}
function deleteSelected() {
  if (profiles.length < 2) return;
  const i = profiles.findIndex((p) => p.id === selected.id);
  const rest = profiles.filter((p) => p.id !== selected.id);
  setProfiles(rest);
  setSelectedId(rest[Math.max(0, i - 1)].id);
}
// save(): const patch: SettingsPatch = { profiles, activeProfileId: selectedId, alwaysOnTop, llmProvider, answerStyle, hotkey };
//   … on env.ok: setProfiles(env.value.profiles); setSelectedId(env.value.activeProfileId); // ids may have been repaired

// ───────────── frontend tests (shape) ─────────────
// App.test.tsx
it('a profile switch sends ONLY activeProfileId and lights the chip the save returned', async () => {
  const bridge = new FakeBridge({ profiles: [P_A, P_B], activeProfileId: 'a' });
  setBridge(bridge); render(<App />);
  await user.click(await screen.findByRole('button', { name: 'Rust / systems' }));
  expect(bridge.setSettings).toHaveBeenLastCalledWith({ activeProfileId: 'b' });
  expect(screen.getByRole('button', { name: 'Rust / systems' })).toHaveAttribute('aria-pressed', 'true');
});
// SettingsView.test.tsx
it('edits only the selected profile and sends the whole array plus its id as active', async () => {
  const { onSave } = renderSettings({ profiles: [P_A, P_B], activeProfileId: 'a' });
  await user.selectOptions(screen.getByLabelText('Profile'), 'b');
  await user.type(screen.getByLabelText('Focus (optional)'), 'tokio');
  await user.click(screen.getByRole('button', { name: 'Save' }));
  const patch = lastPatch(onSave);
  expect(patch.activeProfileId).toBe('b');
  expect(patch.profiles?.[0]).toEqual(P_A);
  expect(patch.profiles?.[1]?.focus).toBe(`${P_B.focus}tokio`);
});

=== risks
1. Test churn on pinned suites (expected, by design): every `Profile { resume, job_description }` literal needs `..Default::default()` (prompt.rs ~12 sites, anthropic.rs:397, groq.rs:336, local.rs:250); settings.rs good_value/good_settings/each_corrupt_field_falls_back_alone (the 8-case array), patch_round_trips_through_disk, over_length_resume_*, truncation_counts_characters_not_bytes, resume_is_stored_verbatim_never_trimmed, corrupt_answer_style_never_costs_the_resume_or_the_keys, undecryptable_key_* all reference settings.resume and must go through active_profile(); frontend testUtils.baseSettings, SettingsView.test.tsx:77-78 (patch.resume/jobDescription), App.test.tsx settings round trip. No pinned STRING changes (ROLE_INSTRUCTIONS, headers, styles, user wrapper) — SPEC §7 wording stays; §8/§9 field lists and TESTING.md counts change.
2. Migration/downgrade: after the first save the file has no top-level resume/jobDescription; an older build reading it sees an empty profile. Acceptable for a single-user app; note it in ADR 008. A corrupt `profiles` VALUE (non-array) cannot be salvaged and loads as one empty Default while keys/hotkey/bounds survive — pin that.
3. Cache: a profile switch is a new prefix -> one cache write (1.25x) for profiles over 4096 tokens, nothing for typical ones; mid-recording switches apply to the NEXT session (deps snapshot at start_session), same as style today — say so in Settings help or accept the existing style precedent.
4. Local provider: focus+extra (up to 4 KB) count toward the 7 KB MAX_INPUT_BYTES (local.rs:15,54); a profile that fit before may fail at answer time; the error text should name the profile (finding PROF-08) and a save-time counter helps (PROF-02).
5. IPC size: every setSettings/getSettings round-trips every profile's full text (≤ 8 × 400 KB worst case, realistic tens of KB). Local WebView2 IPC, fine for v1; PROF-07 records the escape hatch.
6. Prompt-injection posture: unchanged in kind — profile text is user-owned and user-typed, same trust as the resume; nothing new reaches the webview except text the user entered; never log it (§11). Extra instructions can override the app's own role text, which is the user's prerogative, not a boundary.
7. UI invariants: ProfileChips must mirror the PERSISTED id (a coerced/stale switch leaves the real chip lit); Settings select == active-on-Save is one concept — document it so nobody later adds a separate "editing" selector. Escape still discards unsaved profile edits silently (PROF-03), and profiles make that loss bigger.
8. Ordering/determinism: normalize_profiles must stay pure and deterministic (no HashMap iteration in output order, no clock in ids on the core side) — pin with a run-twice equality test so the cached prefix can never become nondeterministic through the store.

=== dont_build
- Auto-detecting the call type or profile from the transcript: a wrong guess silently grounds the answer in the wrong JD — the exact "confidently wrong in 1 s" failure profiles exist to prevent — and it adds work inside the stop-to-first-word window. Selection is a deliberate user gesture, like Stop.
- Per-question profile switching or modifier-key profile overrides: the cache split exists for per-question STYLE (IDEAS #1); a profile is per-call by definition, and a per-question prefix change is a cache write every question with a large profile (ADR 007 "If revisited").
- Per-profile default answer style (v1): it makes the switch action write two fields and creates a second source of truth for the chips' aria-pressed contract (§9). Revisit only if users report flipping style after every switch.
- Free-text call type: unpinnable prompt text headed for the system block; the enum keeps every prompt byte product-owned and verbatim-tested.
- Cloud sync, CRM, LinkedIn/ATS import, shared or team profiles: anti-goals (IDEAS "Cloud accounts", "Telemetry, ever"). Profiles live in the one settings file.
- Per-profile API keys, providers, or hotkeys: keys and the shortcut are per-user, not per-call.
- A shared-resume/many-JD graph or "smart merge" of resumes: Duplicate covers "same resume, new JD" in one click; self-contained bundles keep migration and fallback trivially per-profile.
- A "fetch JD from URL" button inside the profile editor: separate idea with its own posture change (IDEAS #13); keep it out of this change.

=== findings
{
 "id": "PROF-01",
 "title": "Single resume/JD forces daily re-paste across concurrent searches and silently grounds answers in the wrong JD",
 "area": "settings / prompt / UI",
 "severity": "high",
 "evidence": "store/mod.rs:25-28 (one `resume`, one `job_description`); commands.rs:292-296 build_deps reads them directly; SettingsView.tsx:31-32 single textareas; prompt.rs:83-107 cached_prefix = role + resume + JD.",
 "problem": "A person interviewing for a React role Monday and a Rust role Tuesday must overwrite the JD (and often the resume emphasis) in Settings before every call; forgetting produces an answer confidently grounded in the wrong job description, delivered in ~1 s. Non-interview calls (sales, support, meeting) get an interview-shaped prompt (\"THE JOB THEY ARE INTERVIEWING FOR\").",
 "proposal": "Build the lean call-profiles design in this report: CallProfile {id,name,callType,resume,jobDescription,focus,extraInstructions}, profiles[] + activeProfileId in Settings/SettingsView/SettingsPatch, legacy flat file read once into a Default profile, additive prompt constants with call-type-dependent headers (interview stays byte-identical to v3), ProfileChips on the main view patching only activeProfileId, Settings profile select + New/Duplicate/Delete editing the selected profile. Tests and docs as listed in the design.",
 "effort": "M",
 "files": [
  "src-tauri/core/src/store/mod.rs",
  "src-tauri/core/src/store/settings.rs",
  "src-tauri/core/src/llm/prompt.rs",
  "src-tauri/core/src/llm/mod.rs",
  "src-tauri/src/commands.rs",
  "src/types.ts",
  "src/App.tsx",
  "src/views/MainView.tsx",
  "src/views/SettingsView.tsx",
  "src/components/ProfileChips.tsx",
  "src/views/testUtils.tsx",
  "src/styles.css",
  "docs/SPEC.md",
  "docs/TESTING.md",
  "docs/DEVELOPMENT.md",
  "docs/adr/008-call-profiles.md",
  "README.md"
 ],
 "risk": "Broad test churn (Profile literals in prompt/anthropic/groq/local tests; settings.rs corruption matrix; frontend baseSettings and patch assertions). SPEC §8/§9 field lists change; §7 pinned strings do not. Older builds reading a migrated file see an empty profile."
}
{
 "id": "PROF-02",
 "title": "Local mode's 7 KB profile cap is enforced only after Stop, never at save time",
 "area": "settings UI / local provider",
 "severity": "medium",
 "evidence": "local.rs:15 `MAX_INPUT_BYTES = 7000`; local.rs:50-56 rejects the request at answer time with \"Free local mode supports about 7 KB…\"; SettingsView.tsx has no size indicator on the resume/JD textareas.",
 "problem": "A user on the free local preset pastes a 12 KB resume, saves successfully, records a real question, presses Stop, and only then learns the profile is too big — the one moment the app must not fail. With profiles adding focus/extra text the cap is easier to hit.",
 "proposal": "Mirror MAX_INPUT_BYTES into types.ts; in SettingsView show a live UTF-8 byte count on the profile fields when llmProvider === 'local' and a field-help warning above ~6.5 KB (measure with TextEncoder). Keep the runtime check as the backstop.",
 "effort": "S",
 "files": [
  "src/views/SettingsView.tsx",
  "src/types.ts",
  "src-tauri/core/src/llm/local.rs"
 ],
 "risk": "None to the hot path; SettingsView tests gain one case. The byte estimate excludes the role/style text, hence the margin."
}
{
 "id": "PROF-03",
 "title": "Escape/Back in Settings discards unsaved edits silently",
 "area": "settings UI",
 "severity": "medium",
 "evidence": "SettingsView.tsx:58-64 `if (e.key === 'Escape') onBack();` with no dirty check; form state is local until Save (SettingsView.tsx:31-42, 76-102).",
 "problem": "A stray Escape mid-paste throws away a resume edit with no confirmation; profiles multiply the amount of text at risk (several profiles edited before one Save).",
 "proposal": "Track `dirty` (any field differs from the seeded props); on Escape/Back while dirty show an inline confirm row (\"Discard unsaved changes? Discard / Keep editing\") instead of closing; a clean form still closes on the first Escape so the pinned 'Escape closes' test survives.",
 "effort": "S",
 "files": [
  "src/views/SettingsView.tsx",
  "src/views/SettingsView.test.tsx",
  "docs/SPEC.md"
 ],
 "risk": "§9 'Escape closes Settings' must remain true for the clean case; no dialogs (inline row only, 460 px window)."
}
{
 "id": "PROF-04",
 "title": "Frontend DEFAULT_HOTKEY placeholder does not mirror the core default it claims to mirror",
 "area": "settings UI",
 "severity": "low",
 "evidence": "store/mod.rs:17 `DEFAULT_HOTKEY = \"Ctrl+Shift+Space\"` vs SettingsView.tsx:15-16 `/** Mirrors the core's default accelerator */ const DEFAULT_HOTKEY = 'CommandOrControl+Shift+Space'`.",
 "problem": "Both strings register the same key on Windows, but the comment is false and the placeholder teaches users a different spelling than the file stores; a future reader will 'fix' one side and break the other's assumption.",
 "proposal": "Either align the frontend constant to 'Ctrl+Shift+Space' or (better) drop the duplicate and add `defaultHotkey: string` to SettingsView (Rust `DEFAULT_HOTKEY`) so the UI cannot drift. Update the comment either way.",
 "effort": "S",
 "files": [
  "src/views/SettingsView.tsx",
  "src-tauri/core/src/store/mod.rs",
  "src/types.ts"
 ],
 "risk": "SettingsView tests that assert the placeholder text; docs/SPEC §8 says default Ctrl+Shift+Space."
}
{
 "id": "PROF-05",
 "title": "Interview prep library is shown regardless of call type",
 "area": "main view UX",
 "severity": "low",
 "evidence": "AskForm.tsx:62-75 always renders the 'Interview prep' toggle; PracticeLibrary.tsx is interview-only content (categories Introduction/Behavioral/Technical/Leadership).",
 "problem": "Once profiles carry a call type, a sales or support profile still shows 'Interview prep — Questions & quick guides', which reads as the app not knowing what call this is.",
 "proposal": "MainView passes `showPrep={activeProfile.callType === 'interview'}` to AskForm; AskForm hides the toggle (and closes an open library) when false. No copy change.",
 "effort": "S",
 "files": [
  "src/components/AskForm.tsx",
  "src/views/MainView.tsx",
  "src/views/MainView.test.tsx"
 ],
 "risk": "Depends on PROF-01. AskForm tests that find the toggle need the default to stay visible."
}
{
 "id": "PROF-06",
 "title": "No nudge when the active profile has neither resume nor JD",
 "area": "main view UX",
 "severity": "low",
 "evidence": "prompt.rs:102-107 silently omits the grounding note when both sections are empty; StatusLine.tsx:44-46 first-run copy keys only off API keys; nothing on the main view says the answers are ungrounded.",
 "problem": "A user who added keys but not a resume (or who switched to a freshly created empty profile) gets generic answers with no hint why; with profiles this happens after every New.",
 "proposal": "Under the ProfileChips (or as the AnswerPanel placeholder variant) show a field-help line: \"No resume or job description saved for <name> — answers won't be grounded. Add them in Settings.\" Not in StatusLine (its strings are pinned by §9).",
 "effort": "S",
 "files": [
  "src/views/MainView.tsx",
  "src/views/MainView.test.tsx",
  "docs/SPEC.md"
 ],
 "risk": "Must not alter any §9 pinned status-line string; one new test."
}
{
 "id": "PROF-07",
 "title": "Every settings save/switch round-trips every profile's full text over IPC",
 "area": "IPC / performance",
 "severity": "low",
 "evidence": "commands.rs:69-71 get_settings and :74-105 set_settings return the full SettingsView; App.tsx:100-111 applySettings replaces state from that view on every StyleChips/ProfileChips click.",
 "problem": "Today one resume+JD (≤ 400 KB) is serialized per chip click; with up to 8 profiles that is ≤ 3.2 MB worst case per style flip or switch. Realistic profiles are tens of KB and the IPC is local, so this is a note, not a bug.",
 "proposal": "Accept for v1 and record it in ADR 008. If it ever measures (add a console.time around setSettings when profiling), add a light `SettingsSummary` (profiles as {id,name,callType} + the scalar fields) returned by switch/style patches and used by the main view, keeping the full view for Settings.",
 "effort": "M",
 "files": [
  "src-tauri/src/commands.rs",
  "src-tauri/core/src/store/mod.rs",
  "src/App.tsx",
  "src/types.ts"
 ],
 "risk": "Two view shapes double the IPC contract surface; do not build until measured."
}
{
 "id": "PROF-08",
 "title": "Local-mode size error names 'resume, job description' and will be stale once profiles carry focus/extra text",
 "area": "local provider copy",
 "severity": "low",
 "evidence": "local.rs:55 \"Free local mode supports about 7 KB of combined instructions, resume, job description and question. Shorten the profile in Settings or use a cloud model.\"",
 "problem": "After PROF-01 the string undercounts what is in the prompt (focus + extra instructions) and does not say which profile is too large.",
 "proposal": "Reword to \"Free local mode supports about 7 KB of combined instructions, profile (resume, job description, focus, extra instructions) and question. Shorten the active profile in Settings or use a cloud model.\" and pin it in local.rs tests alongside the existing request-shape test.",
 "effort": "S",
 "files": [
  "src-tauri/core/src/llm/local.rs",
  "docs/SPEC.md"
 ],
 "risk": "Any test pinning the current message text; fold into PROF-01's change."
}

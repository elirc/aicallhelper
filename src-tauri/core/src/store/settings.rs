//! The settings store (§8): one JSON file, atomic writes, per-field fallback.
//!
//! Two failure modes shape everything here:
//!
//! 1. **The file is untrusted.** It is user-writable and survives upgrades, so
//!    every field validates and falls back *individually* — one corrupt value
//!    must never cost the user their profiles or their keys. A file that is
//!    not a JSON object at all is moved aside to
//!    `settings.json.corrupt-<unix-seconds>` before the app starts from
//!    defaults, and a file that cannot be read or preserved is never
//!    overwritten (see `SettingsStore::load_from`).
//! 2. **Writes must be atomic.** The file also holds the (encrypted) keys, so
//!    a crash or full disk mid-write that truncated it would silently destroy
//!    every setting. Writes go to `settings.json.tmp` and rename over the real
//!    file; the in-memory cache is updated only after the write lands, so a
//!    failed write leaves memory matching disk.
//!
//! Profiles (v3.1) add a third: **one writer for the invariant.** Every path
//! that produces a profile list — the disk loader, a whole-array patch, a
//! bare active-id switch — goes through `normalize_profiles`, which is pure
//! and deterministic, so the store can never smuggle nondeterminism into the
//! cached prompt prefix (ADR 007).

use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::error::{AppError, ErrorCode};
use crate::llm::{AnswerStyle, CallType, LlmProviderKind};

use super::{
    secrets, CallProfile, LaunchPlacement, RawBounds, Settings, SettingsPatch, SettingsView,
    StreamFollow, DEFAULT_HOTKEY, DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME,
    MAX_EXTRA_INSTRUCTIONS_CHARS, MAX_FOCUS_CHARS, MAX_HOTKEY_CHARS, MAX_PROFILES,
    MAX_PROFILE_CHARS, MAX_PROFILE_ID_CHARS, MAX_PROFILE_NAME_CHARS, UNNAMED_PROFILE_NAME,
};

pub const SETTINGS_FILE_NAME: &str = "settings.json";
pub const SETTINGS_TMP_NAME: &str = "settings.json.tmp";
/// Prefix of a quarantined copy: `settings.json.corrupt-<unix-seconds>`.
pub const SETTINGS_CORRUPT_PREFIX: &str = "settings.json.corrupt-";
/// The message a stale full-form save gets (R5, ADR 016). The UI keys its
/// reload offer off the `SettingsConflict` code, not this text.
pub const MSG_SETTINGS_CONFLICT: &str = "Settings changed while this form was open. Reload it.";

/// How many `-n` suffixes a quarantine tries before giving up. A collision
/// needs two damaged files in the same second, so ten is generous.
const QUARANTINE_ATTEMPTS: u32 = 10;

/// Everything behind the store's one lock. The revision and the settings
/// change together or not at all, which is what lets a compare-and-commit
/// happen in one critical section.
struct Committed {
    settings: Settings,
    /// Editable-settings revision (R5). Advances only after a patch reached
    /// disk.
    revision: u64,
    /// Shown in the view until the next successful save, if any.
    warning: Option<String>,
    /// Set when the file on disk could not be read or preserved: every write
    /// is refused so a default-filled store never replaces a file that may
    /// still hold the user's profiles and keys.
    write_block: Option<String>,
}

impl Committed {
    fn view(&self) -> SettingsView {
        self.settings.view_at(self.revision, self.warning.clone())
    }
}

/// How the startup read went. Only `Invalid` is ever quarantined: a file
/// that could not be READ (permissions, a sharing lock, a transient error)
/// is not known to be damaged.
enum LoadResult {
    Missing,
    Parsed(Settings),
    Invalid,
    Unreadable(std::io::Error),
}

pub struct SettingsStore {
    path: PathBuf,
    tmp_path: PathBuf,
    state: RwLock<Committed>,
}

impl SettingsStore {
    /// Load from `dir/settings.json`. Never fails: startup must not depend on
    /// the health of a user-editable file. What happens to a file that cannot
    /// be used depends on why (see `LoadResult`):
    ///
    /// * missing: first run, defaults, saving works;
    /// * present but not a settings JSON object: renamed to
    ///   `settings.json.corrupt-<unix-seconds>` first, then defaults, and the
    ///   view carries a warning naming the copy;
    /// * unreadable, or damaged but the rename failed: defaults in memory
    ///   only, every write refused, and the view says why. Overwriting a file
    ///   we could not read or preserve is how a transient lock turns into
    ///   lost profiles and keys.
    pub fn load_from(dir: &Path) -> Self {
        Self::load_at(dir, unix_seconds())
    }

    /// `load_from` with the quarantine timestamp supplied, so tests can force
    /// a backup-name collision.
    fn load_at(dir: &Path, now_secs: u64) -> Self {
        let path = dir.join(SETTINGS_FILE_NAME);
        let tmp_path = dir.join(SETTINGS_TMP_NAME);
        // Only settings.json is ever read. A stale .tmp left by a crashed
        // write is dead data; the next save simply overwrites it.
        let (settings, warning, write_block) = match read_settings(&path) {
            LoadResult::Missing => (Settings::default(), None, None),
            LoadResult::Parsed(s) => (s, None, None),
            LoadResult::Invalid => match quarantine(&path, dir, now_secs) {
                Ok(name) => (
                    Settings::default(),
                    Some(format!(
                        "Your settings file was damaged and could not be read. It was kept as {name} in the app's data folder, and the app started with default settings."
                    )),
                    None,
                ),
                Err(e) => {
                    let msg = format!(
                        "Your settings file is damaged and a backup copy could not be made ({e}). The app is using default settings and will not save changes, so the file is left untouched. Move settings.json out of the app's data folder, then restart."
                    );
                    (Settings::default(), Some(msg.clone()), Some(msg))
                }
            },
            LoadResult::Unreadable(e) => {
                let msg = format!(
                    "Your settings file could not be read ({e}). The app is using default settings and will not save changes, so the file is left untouched. Close anything that has settings.json open, then restart the app."
                );
                (Settings::default(), Some(msg.clone()), Some(msg))
            }
        };
        Self {
            path,
            tmp_path,
            state: RwLock::new(Committed { settings, revision: 1, warning, write_block }),
        }
    }

    pub fn get(&self) -> Settings {
        self.read_state().settings.clone()
    }

    pub fn view(&self) -> SettingsView {
        self.read_state().view()
    }

    /// The committed editable-settings revision.
    pub fn revision(&self) -> u64 {
        self.read_state().revision
    }

    /// Apply a partial update and persist it. Key fields are write-only across
    /// the UI boundary: `None` leaves the stored key untouched, an
    /// empty-after-trim value clears it, anything else replaces it trimmed.
    /// The returned view never contains key material.
    ///
    /// Revision rule (R5, ADR 016): a patch carrying `expected_revision` is
    /// compared with the committed revision under the same write lock that
    /// commits it, so no other patch can land between the check and the
    /// write. The revision advances only after the file reached disk.
    pub fn apply_patch(&self, patch: SettingsPatch) -> Result<SettingsView, AppError> {
        let mut state = self.write_state();
        if let Some(block) = &state.write_block {
            return Err(AppError::internal(block.clone()));
        }
        if let Some(expected) = patch.expected_revision {
            if expected != state.revision {
                return Err(AppError::new(ErrorCode::SettingsConflict, MSG_SETTINGS_CONFLICT));
            }
        }
        let mut next = state.settings.clone();

        if patch.profiles.is_some() || patch.active_profile_id.is_some() {
            // Whole-array replace when the form sent one; a bare switch keeps
            // every byte of profile text and only moves the active id.
            let proposed: Vec<CallProfile> = match patch.profiles {
                Some(list) => list.into_iter().map(CallProfile::from).collect(),
                None => next.profiles.clone(),
            };
            // `previous` = the current active: a switch to an id that no
            // longer exists keeps what is active rather than jumping to the
            // first profile, and the returned view then tells the chips the
            // truth (they mirror the persisted id, never the click).
            let (profiles, active) = normalize_profiles(
                proposed,
                patch.active_profile_id.as_deref(),
                Some(&next.active_profile_id),
            );
            next.profiles = profiles;
            next.active_profile_id = active;
        }
        if let Some(v) = patch.always_on_top {
            next.always_on_top = v;
        }
        // Typed at the IPC edge: an unknown wire value already failed
        // deserialization, so there is nothing to parse or trim here (§4).
        if let Some(v) = patch.llm_provider {
            next.llm_provider = v;
        }
        if let Some(v) = patch.answer_style {
            next.answer_style = v;
        }
        if let Some(v) = patch.launch_placement {
            next.launch_placement = v;
        }
        if let Some(v) = patch.stream_follow {
            next.stream_follow = v;
        }
        if let Some(v) = patch.hotkey {
            // Trimmed: raw spaces make the shortcut registration throw. A
            // whitespace-only value therefore becomes "" — shortcut disabled —
            // and stays that way; it must never spring back to the default.
            next.hotkey = cap_chars(v.trim(), MAX_HOTKEY_CHARS);
        }
        apply_key_patch(&mut next.deepgram_key, patch.deepgram_key);
        apply_key_patch(&mut next.anthropic_key, patch.anthropic_key);
        apply_key_patch(&mut next.groq_key, patch.groq_key);

        // Disk first, cache second: if the write fails the user sees an error
        // and memory (settings AND revision) still matches what will load
        // next launch.
        self.persist(&next)
            .map_err(|e| AppError::internal(format!("Could not save settings: {e}")))?;
        state.settings = next;
        state.revision += 1;
        // A successful save means the store is healthy again, and the
        // startup warning was on screen in the form that just saved.
        state.warning = None;
        Ok(state.view())
    }

    /// Persist window geometry. Swallows every failure: this is cosmetic data
    /// written during shutdown, and throwing there would lose the real close
    /// path. Deliberately does NOT advance the revision (ADR 016): geometry
    /// is not an editable setting, and moving the window must never make an
    /// open Settings form stale.
    pub fn save_window_bounds(&self, b: RawBounds) {
        let mut state = self.write_state();
        if state.write_block.is_some() {
            // Same protection as apply_patch: never replace a file we could
            // not read or preserve, not even for cosmetic data.
            return;
        }
        let mut next = state.settings.clone();
        next.window_bounds = Some(b);
        // Same disk-then-cache discipline as apply_patch, minus the error —
        // on failure memory keeps matching disk and the app closes normally.
        if self.persist(&next).is_ok() {
            state.settings = next;
        }
    }

    pub fn window_bounds(&self) -> Option<RawBounds> {
        self.read_state().settings.window_bounds
    }

    fn persist(&self, settings: &Settings) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(&to_disk_json(settings))
            .map_err(std::io::Error::other)?;
        // Write and flush the temp file completely before the rename, so the
        // real file only ever transitions between two complete states — a
        // half-written settings.json would be quarantined on the next launch
        // and every setting, keys included, would start from defaults.
        let mut file = fs::File::create(&self.tmp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&self.tmp_path, &self.path)
    }

    // A poisoned lock means a writer panicked, but writers only ever store a
    // fully-assembled state, so the data behind the lock is always coherent;
    // recovering beats poisoning every settings read for the rest of the
    // session.
    fn read_state(&self) -> RwLockReadGuard<'_, Committed> {
        self.state.read().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_state(&self) -> RwLockWriteGuard<'_, Committed> {
        self.state.write().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Read and classify the settings file. Bytes, not `read_to_string`: invalid
/// UTF-8 is damaged content (quarantine), not an I/O failure (leave alone).
fn read_settings(path: &Path) -> LoadResult {
    match fs::read(path) {
        Ok(bytes) => match std::str::from_utf8(&bytes).ok().and_then(settings_from_disk) {
            Some(settings) => LoadResult::Parsed(settings),
            None => LoadResult::Invalid,
        },
        Err(e) if e.kind() == ErrorKind::NotFound => LoadResult::Missing,
        Err(e) => LoadResult::Unreadable(e),
    }
}

/// Move a damaged settings file aside to the first free
/// `settings.json.corrupt-<secs>[-n]` name and return that name. A rename,
/// not a copy: the original is either preserved under the new name or left
/// where it was, never half of each. An existing backup is never overwritten
/// (Windows `rename` replaces a file silently), so the existence check comes
/// first.
fn quarantine(path: &Path, dir: &Path, now_secs: u64) -> std::io::Result<String> {
    for n in 0..QUARANTINE_ATTEMPTS {
        let name = if n == 0 {
            format!("{SETTINGS_CORRUPT_PREFIX}{now_secs}")
        } else {
            format!("{SETTINGS_CORRUPT_PREFIX}{now_secs}-{n}")
        };
        let target = dir.join(&name);
        if target.exists() {
            continue;
        }
        fs::rename(path, &target)?;
        return Ok(name);
    }
    Err(std::io::Error::new(
        ErrorKind::AlreadyExists,
        "every backup name for this second is already taken",
    ))
}

/// The ONE place the profile invariant is enforced (§8): at most
/// MAX_PROFILES and never none, every text field capped, ids valid and
/// unique, and the returned active id names one of the returned profiles.
///
/// Pure and deterministic — same input, same output; no clock, no random
/// ids, no hash iteration in anything that reaches the result — so the two
/// callers (the disk loader and `apply_patch`) can never smuggle
/// nondeterminism into the cached prompt prefix (ADR 007). It is also
/// idempotent, which is what lets a bare active-id switch re-run it over the
/// stored list without touching a byte of profile text.
///
/// `requested` is the id the caller wants active (a patch, or the file's
/// `activeProfileId`); `previous` is the id that was active before. A
/// requested id that names no profile is ignored in favour of `previous`
/// rather than jumping to the first profile — a stale switch from the UI
/// must not silently re-ground the next answer in a different profile.
pub fn normalize_profiles(
    mut profiles: Vec<CallProfile>,
    requested: Option<&str>,
    previous: Option<&str>,
) -> (Vec<CallProfile>, String) {
    profiles.truncate(MAX_PROFILES);
    if profiles.is_empty() {
        profiles.push(CallProfile::empty(DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME));
    }

    // Membership only: nothing about the output depends on set iteration.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut needs_id = Vec::new();
    for (i, p) in profiles.iter_mut().enumerate() {
        // Names are display text: trimmed, capped, never blank — a chip cannot
        // show "" and a select cannot list it.
        let name = cap_chars(p.name.trim(), MAX_PROFILE_NAME_CHARS);
        let name = name.trim();
        p.name = if name.is_empty() { UNNAMED_PROFILE_NAME.to_string() } else { name.to_string() };
        // Resume / JD verbatim, NOT trimmed: profile formatting belongs to the
        // user (§8). Capped so a maliciously large file cannot balloon every
        // prompt.
        cap_in_place(&mut p.resume, MAX_PROFILE_CHARS);
        cap_in_place(&mut p.job_description, MAX_PROFILE_CHARS);
        cap_in_place(&mut p.focus, MAX_FOCUS_CHARS);
        cap_in_place(&mut p.extra_instructions, MAX_EXTRA_INSTRUCTIONS_CHARS);
        // The first occurrence of an id keeps it; a later duplicate, or an id
        // that would need escaping somewhere, is repaired below.
        if !(valid_id(&p.id) && seen.insert(p.id.clone())) {
            needs_id.push(i);
        }
    }
    // Repaired ids are the smallest unused `p<n>`, assigned in list order, so
    // the same broken input always repairs to the same ids.
    let mut n = 1;
    for i in needs_id {
        let id = loop {
            let candidate = format!("p{n}");
            n += 1;
            if !seen.contains(&candidate) {
                break candidate;
            }
        };
        seen.insert(id.clone());
        profiles[i].id = id;
    }

    let known = |id: Option<&str>| {
        id.filter(|id| profiles.iter().any(|p| p.id == *id)).map(str::to_string)
    };
    let active = known(requested)
        .or_else(|| known(previous))
        .unwrap_or_else(|| profiles[0].id.clone());
    (profiles, active)
}

/// `[A-Za-z0-9_-]{1,40}`. ASCII-only, so `len()` is the character count.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PROFILE_ID_CHARS
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Truncate by characters, not bytes — a byte cut can split a UTF-8 sequence
/// and turn a resume into invalid data on the next load.
fn cap_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// `cap_chars` without the copy, for the profile fields that can be 200 KB
/// each and are already owned. The byte length bounds the char count, so a
/// string under `max` bytes is left alone without a scan.
fn cap_in_place(s: &mut String, max: usize) {
    if s.len() > max {
        if let Some((idx, _)) = s.char_indices().nth(max) {
            s.truncate(idx);
        }
    }
}

/// Three-way key semantics (see `SettingsPatch`): omitted leaves the stored
/// key, empty-after-trim clears it, anything else replaces it trimmed.
fn apply_key_patch(slot: &mut Option<String>, patch: Option<String>) {
    if let Some(v) = patch {
        let trimmed = v.trim();
        *slot = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };
    }
}

/// Parse file contents with per-field fallback. Every arm answers the same
/// question: "what does the user lose if only this value is garbage?" — and
/// the answer must always be "only this value". `None` only when there is no
/// JSON object at all: that file is damaged as a whole and is quarantined.
fn settings_from_disk(text: &str) -> Option<Settings> {
    let value = serde_json::from_str::<Value>(text).ok()?;
    let obj = value.as_object()?;

    let (profiles, requested) = match obj.get("profiles") {
        Some(Value::Array(items)) => (
            items.iter().filter_map(profile_from_disk).collect::<Vec<_>>(),
            obj.get("activeProfileId").and_then(Value::as_str),
        ),
        // A v3 flat file (or a corrupt `profiles` value): the legacy top-level
        // resume / JD become one "Default" interview profile — the exact
        // prompt the user had before the upgrade. This shape is read once and
        // never written back; the file upgrades on its first save.
        _ => (
            vec![CallProfile {
                resume: profile_field(obj.get("resume")),
                job_description: profile_field(obj.get("jobDescription")),
                ..CallProfile::empty(DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME)
            }],
            None,
        ),
    };
    // No `previous` on load: an unknown active id falls back to the first
    // profile, which is the only honest answer with nothing else to go on.
    let (profiles, active_profile_id) = normalize_profiles(profiles, requested, None);

    Some(Settings {
        profiles,
        active_profile_id,
        always_on_top: obj.get("alwaysOnTop").and_then(Value::as_bool).unwrap_or(true),
        llm_provider: obj
            .get("llmProvider")
            .and_then(Value::as_str)
            .map(LlmProviderKind::parse_or_default)
            .unwrap_or_default(),
        answer_style: obj
            .get("answerStyle")
            .and_then(Value::as_str)
            .map(AnswerStyle::parse_or_default)
            .unwrap_or_default(),
        hotkey: hotkey_field(obj.get("hotkey")),
        launch_placement: obj
            .get("launchPlacement")
            .and_then(Value::as_str)
            .map(LaunchPlacement::parse_or_default)
            .unwrap_or_default(),
        stream_follow: obj
            .get("streamFollow")
            .and_then(Value::as_str)
            .map(StreamFollow::parse_or_default)
            .unwrap_or_default(),
        deepgram_key: key_field(obj.get("deepgramKey")),
        anthropic_key: key_field(obj.get("anthropicKey")),
        groq_key: key_field(obj.get("groqKey")),
        // Corrupt geometry drops as a unit — a good width welded to a
        // defaulted height is a shape the user never chose (see bounds.rs).
        // The object check matters: serde will happily read a struct out of a
        // JSON array ([1,2,3,4] "works"), and this app never writes one, so a
        // non-object here is corruption by definition.
        window_bounds: obj
            .get("windowBounds")
            .filter(|v| v.is_object())
            .cloned()
            .and_then(|v| serde_json::from_value::<RawBounds>(v).ok()),
    })
}

/// Per-field fallback INSIDE a profile: a bad callType is interview, a bad or
/// missing string is "". Only a non-object entry is dropped — there is
/// nothing in it to save. Caps and id repair happen in `normalize_profiles`.
fn profile_from_disk(v: &Value) -> Option<CallProfile> {
    let o = v.as_object()?;
    let s = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    Some(CallProfile {
        id: s("id"),
        name: s("name"),
        call_type: o
            .get("callType")
            .and_then(Value::as_str)
            .map(CallType::parse_or_default)
            .unwrap_or_default(),
        resume: s("resume"),
        job_description: s("jobDescription"),
        focus: s("focus"),
        extra_instructions: s("extraInstructions"),
    })
}

fn profile_field(v: Option<&Value>) -> String {
    // Stored verbatim — no trim. Capped so a maliciously large file cannot
    // balloon every prompt; a non-string reads as "no profile yet".
    v.and_then(Value::as_str).map(|s| cap_chars(s, MAX_PROFILE_CHARS)).unwrap_or_default()
}

fn hotkey_field(v: Option<&Value>) -> String {
    match v.and_then(Value::as_str) {
        // A present-but-empty string is a choice — "shortcut disabled" — and
        // must not spring back to the default. Trimmed for the same reason as
        // on save: raw spaces make shortcut registration throw.
        Some(s) => cap_chars(s.trim(), MAX_HOTKEY_CHARS),
        // Absent or non-string means "never configured", which does get the
        // default.
        None => DEFAULT_HOTKEY.to_string(),
    }
}

fn key_field(v: Option<&Value>) -> Option<String> {
    // Undecodable values read as unset (see secrets.rs) — never hand the raw
    // stored string to a provider as if it were a key. An empty decode is
    // equally unusable and would make has*Key lie to the UI.
    v.and_then(Value::as_str).and_then(secrets::unprotect).filter(|k| !k.is_empty())
}

/// The §8 disk shape for one profile. Built by hand rather than through
/// serde so the file format cannot drift with a rename attribute on the wire
/// type, and so a serialization failure is impossible rather than swallowed.
fn profile_to_disk(p: &CallProfile) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::String(p.id.clone()));
    o.insert("name".into(), Value::String(p.name.clone()));
    o.insert("callType".into(), Value::String(p.call_type.as_str().to_string()));
    o.insert("resume".into(), Value::String(p.resume.clone()));
    o.insert("jobDescription".into(), Value::String(p.job_description.clone()));
    o.insert("focus".into(), Value::String(p.focus.clone()));
    o.insert("extraInstructions".into(), Value::String(p.extra_instructions.clone()));
    Value::Object(o)
}

fn to_disk_json(s: &Settings) -> Value {
    let mut m = Map::new();
    // Only the new shape is ever written: no top-level resume / jobDescription
    // — a file that has been saved once by this version has migrated.
    m.insert("profiles".into(), Value::Array(s.profiles.iter().map(profile_to_disk).collect()));
    m.insert("activeProfileId".into(), Value::String(s.active_profile_id.clone()));
    m.insert("alwaysOnTop".into(), Value::Bool(s.always_on_top));
    m.insert("llmProvider".into(), Value::String(s.llm_provider.as_str().to_string()));
    m.insert("answerStyle".into(), Value::String(s.answer_style.as_str().to_string()));
    m.insert("hotkey".into(), Value::String(s.hotkey.clone()));
    m.insert("launchPlacement".into(), Value::String(s.launch_placement.as_str().to_string()));
    m.insert("streamFollow".into(), Value::String(s.stream_follow.as_str().to_string()));
    // Cleared keys are omitted rather than written as "" — an absent field and
    // an unset key must mean the same thing on the next load.
    if let Some(k) = &s.deepgram_key {
        m.insert("deepgramKey".into(), Value::String(secrets::protect(k)));
    }
    if let Some(k) = &s.anthropic_key {
        m.insert("anthropicKey".into(), Value::String(secrets::protect(k)));
    }
    if let Some(k) = &s.groq_key {
        m.insert("groqKey".into(), Value::String(secrets::protect(k)));
    }
    if let Some(b) = s.window_bounds {
        if let Ok(v) = serde_json::to_value(b) {
            m.insert("windowBounds".into(), v);
        }
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::CallProfilePatch;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use serde_json::json;
    use tempfile::tempdir;

    /// A stored key readable regardless of DPAPI availability, for hand-built
    /// files.
    fn plain(v: &str) -> String {
        format!("plain:{}", B64.encode(v))
    }

    fn write_settings(dir: &Path, contents: &str) {
        fs::write(dir.join(SETTINGS_FILE_NAME), contents).unwrap();
    }

    fn load(dir: &Path) -> Settings {
        SettingsStore::load_from(dir).get()
    }

    fn read_raw(dir: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(dir.join(SETTINGS_FILE_NAME)).unwrap()).unwrap()
    }

    /// A patch that only edits the active profile's resume — the v3 "user
    /// edited the resume" gesture, in the profiles shape.
    fn resume_patch(resume: &str) -> SettingsPatch {
        SettingsPatch {
            profiles: Some(vec![CallProfilePatch {
                id: DEFAULT_PROFILE_ID.into(),
                name: DEFAULT_PROFILE_NAME.into(),
                resume: resume.into(),
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    fn profile_patch(id: &str, name: &str, resume: &str) -> CallProfilePatch {
        CallProfilePatch { id: id.into(), name: name.into(), resume: resume.into(), ..Default::default() }
    }

    fn default_profile() -> CallProfile {
        CallProfile::empty(DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME)
    }

    /// A fully-populated, fully-valid file — the assets the corruption tests
    /// must prove survive. Two profiles with the SECOND active, and every
    /// enum at its non-default value, so each fallback is observable.
    fn good_value() -> Value {
        json!({
            "profiles": [
                {
                    "id": "default", "name": "Default", "callType": "interview",
                    "resume": "Resume text", "jobDescription": "JD text",
                    "focus": "", "extraInstructions": ""
                },
                {
                    "id": "rust", "name": "Rust / systems", "callType": "sales",
                    "resume": "About me", "jobDescription": "Account context",
                    "focus": "Rust, tokio", "extraInstructions": "Keep it short"
                }
            ],
            "activeProfileId": "rust",
            "alwaysOnTop": false,
            "llmProvider": "groq",
            "answerStyle": "brief",
            "hotkey": "Ctrl+K",
            "launchPlacement": "remembered",
            "streamFollow": "top",
            "deepgramKey": plain("dg-key"),
            "anthropicKey": plain("ant-key"),
            "groqKey": plain("gq-key"),
            "windowBounds": {"x": 10.0, "y": 20.0, "width": 500.0, "height": 700.0}
        })
    }

    fn good_settings() -> Settings {
        Settings {
            profiles: vec![
                CallProfile {
                    id: "default".into(),
                    name: "Default".into(),
                    call_type: CallType::Interview,
                    resume: "Resume text".into(),
                    job_description: "JD text".into(),
                    focus: String::new(),
                    extra_instructions: String::new(),
                },
                CallProfile {
                    id: "rust".into(),
                    name: "Rust / systems".into(),
                    call_type: CallType::Sales,
                    resume: "About me".into(),
                    job_description: "Account context".into(),
                    focus: "Rust, tokio".into(),
                    extra_instructions: "Keep it short".into(),
                },
            ],
            active_profile_id: "rust".into(),
            always_on_top: false,
            llm_provider: LlmProviderKind::Groq,
            answer_style: AnswerStyle::Brief,
            hotkey: "Ctrl+K".into(),
            launch_placement: LaunchPlacement::Remembered,
            stream_follow: StreamFollow::Top,
            deepgram_key: Some("dg-key".into()),
            anthropic_key: Some("ant-key".into()),
            groq_key: Some("gq-key".into()),
            window_bounds: Some(RawBounds { x: 10.0, y: 20.0, width: 500.0, height: 700.0 }),
        }
    }

    #[test]
    fn first_run_loads_defaults() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        assert_eq!(store.get(), Settings::default());
        assert_eq!(store.get().hotkey, DEFAULT_HOTKEY);
        assert!(store.get().always_on_top);
        assert_eq!(store.window_bounds(), None);
    }

    #[test]
    fn first_run_loads_one_empty_default_profile() {
        // No file at all: the user gets a "Default" interview profile to fill
        // in, docked launch, tail follow — never an empty profile list the
        // answer path would have to special-case.
        let dir = tempdir().unwrap();
        let got = load(dir.path());
        assert_eq!(got.profiles, vec![default_profile()]);
        assert_eq!(got.active_profile_id, DEFAULT_PROFILE_ID);
        assert_eq!(got.active_profile().name, DEFAULT_PROFILE_NAME);
        assert_eq!(got.launch_placement, LaunchPlacement::Camera);
        assert_eq!(got.stream_follow, StreamFollow::Tail);
    }

    #[test]
    fn a_fully_valid_file_loads_every_field() {
        let dir = tempdir().unwrap();
        write_settings(dir.path(), &good_value().to_string());
        assert_eq!(load(dir.path()), good_settings());
    }

    #[test]
    fn patch_round_trips_through_disk() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store
            .apply_patch(SettingsPatch {
                // The store's first revision: a form seeded at launch.
                expected_revision: Some(1),
                profiles: Some(vec![
                    CallProfilePatch {
                        id: "a".into(),
                        name: "A".into(),
                        call_type: "meeting".into(),
                        resume: "My resume".into(),
                        job_description: "The JD".into(),
                        focus: "F".into(),
                        extra_instructions: "E".into(),
                    },
                    profile_patch("b", "B", "RB"),
                ]),
                active_profile_id: Some("b".into()),
                always_on_top: Some(false),
                llm_provider: Some(LlmProviderKind::Groq),
                answer_style: Some(AnswerStyle::Detailed),
                hotkey: Some("Alt+Q".into()),
                launch_placement: Some(LaunchPlacement::Remembered),
                stream_follow: Some(StreamFollow::Top),
                deepgram_key: Some("dg-1".into()),
                anthropic_key: Some("ant-1".into()),
                groq_key: Some("gq-1".into()),
            })
            .unwrap();
        store.save_window_bounds(RawBounds { x: 1.0, y: 2.0, width: 460.0, height: 700.0 });

        // A fresh store from the same directory must see exactly what the
        // first one wrote — that is what persistence means here.
        let reloaded = load(dir.path());
        assert_eq!(reloaded, store.get());
        let a = &reloaded.profiles[0];
        assert_eq!((a.id.as_str(), a.name.as_str(), a.call_type), ("a", "A", CallType::Meeting));
        assert_eq!((a.resume.as_str(), a.job_description.as_str()), ("My resume", "The JD"));
        assert_eq!((a.focus.as_str(), a.extra_instructions.as_str()), ("F", "E"));
        assert_eq!(reloaded.active_profile_id, "b");
        assert_eq!(reloaded.active_profile().resume, "RB");
        assert_eq!(reloaded.launch_placement, LaunchPlacement::Remembered);
        assert_eq!(reloaded.stream_follow, StreamFollow::Top);
        assert_eq!(reloaded.anthropic_key.as_deref(), Some("ant-1"));
        assert_eq!(
            reloaded.window_bounds,
            Some(RawBounds { x: 1.0, y: 2.0, width: 460.0, height: 700.0 })
        );
    }

    /// Every file in `dir` whose name starts with the quarantine prefix.
    fn backups(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with(SETTINGS_CORRUPT_PREFIX))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn unparseable_json_is_quarantined_then_loads_as_defaults() {
        let dir = tempdir().unwrap();
        write_settings(dir.path(), "{ this is not json");
        let store = SettingsStore::load_at(dir.path(), 1_000);
        assert_eq!(store.get(), Settings::default());

        // The damaged bytes survive, byte for byte, under the backup name,
        // and the original name is free for the next save.
        let name = format!("{SETTINGS_CORRUPT_PREFIX}1000");
        assert_eq!(backups(dir.path()), vec![name.clone()]);
        assert_eq!(fs::read_to_string(dir.path().join(&name)).unwrap(), "{ this is not json");
        assert!(!dir.path().join(SETTINGS_FILE_NAME).exists());

        // The user is told where the copy went, and saving works normally.
        let warning = store.view().storage_warning.expect("a quarantine must be surfaced");
        assert!(warning.contains(&name), "warning: {warning}");
        let view = store.apply_patch(resume_patch("fresh start")).unwrap();
        assert_eq!(view.storage_warning, None, "a successful save clears the warning");
        assert_eq!(load(dir.path()).active_profile().resume, "fresh start");
        assert_eq!(backups(dir.path()), vec![name], "the backup is never touched again");
    }

    #[test]
    fn non_object_json_is_quarantined_then_loads_as_defaults() {
        for bad in ["[1,2,3]", "\"hello\"", "42", "true", "null", ""] {
            let dir = tempdir().unwrap();
            write_settings(dir.path(), bad);
            let store = SettingsStore::load_from(dir.path());
            assert_eq!(store.get(), Settings::default(), "bad: {bad:?}");
            let names = backups(dir.path());
            assert_eq!(names.len(), 1, "bad: {bad:?}");
            assert_eq!(fs::read_to_string(dir.path().join(&names[0])).unwrap(), bad);
            assert!(store.view().storage_warning.is_some(), "bad: {bad:?}");
        }
    }

    #[test]
    fn invalid_utf8_is_damaged_content_not_an_io_error() {
        // read_to_string would report InvalidData, an I/O-shaped error; the
        // bytes were read fine and are simply not settings, so they are
        // quarantined rather than protected as "unreadable".
        let dir = tempdir().unwrap();
        let bytes = [b'{', 0xFF, 0xFE, b'}'];
        fs::write(dir.path().join(SETTINGS_FILE_NAME), bytes).unwrap();
        let store = SettingsStore::load_at(dir.path(), 7);
        assert_eq!(store.get(), Settings::default());
        let name = format!("{SETTINGS_CORRUPT_PREFIX}7");
        assert_eq!(fs::read(dir.path().join(name)).unwrap(), bytes);
        store.apply_patch(resume_patch("r")).expect("saving works after a quarantine");
    }

    #[test]
    fn a_missing_file_is_a_clean_first_run_with_no_backup_and_no_warning() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        assert_eq!(store.view().storage_warning, None);
        assert!(backups(dir.path()).is_empty());
        store.apply_patch(resume_patch("r")).unwrap();
        assert_eq!(load(dir.path()).active_profile().resume, "r");
    }

    #[test]
    fn quarantine_never_overwrites_an_existing_backup() {
        let dir = tempdir().unwrap();
        let older = format!("{SETTINGS_CORRUPT_PREFIX}1000");
        fs::write(dir.path().join(&older), "older backup").unwrap();
        write_settings(dir.path(), "not json");
        let store = SettingsStore::load_at(dir.path(), 1_000);
        assert_eq!(fs::read_to_string(dir.path().join(&older)).unwrap(), "older backup");
        let newer = format!("{SETTINGS_CORRUPT_PREFIX}1000-1");
        assert_eq!(fs::read_to_string(dir.path().join(&newer)).unwrap(), "not json");
        assert!(store.view().storage_warning.unwrap().contains(&newer));
    }

    #[test]
    fn a_failed_quarantine_leaves_the_original_untouched_and_blocks_every_write() {
        // Every backup name for this second is taken, so preservation fails.
        // The damaged file must then stay exactly where and what it was:
        // replacing it with defaults would destroy the only copy.
        let dir = tempdir().unwrap();
        for n in 0..QUARANTINE_ATTEMPTS {
            let name = if n == 0 {
                format!("{SETTINGS_CORRUPT_PREFIX}5")
            } else {
                format!("{SETTINGS_CORRUPT_PREFIX}5-{n}")
            };
            fs::write(dir.path().join(name), "taken").unwrap();
        }
        write_settings(dir.path(), "{ damaged");
        let store = SettingsStore::load_at(dir.path(), 5);
        assert_eq!(store.get(), Settings::default(), "defaults in memory only");
        let warning = store.view().storage_warning.expect("the problem is surfaced");

        let err = store.apply_patch(resume_patch("would overwrite")).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert_eq!(err.message, warning);
        store.save_window_bounds(RawBounds { x: 1.0, y: 2.0, width: 460.0, height: 700.0 });
        assert_eq!(
            fs::read_to_string(dir.path().join(SETTINGS_FILE_NAME)).unwrap(),
            "{ damaged",
            "neither a patch nor a geometry save may replace the original"
        );
        assert_eq!(store.revision(), 1, "a refused write never advances the revision");
    }

    #[cfg(windows)]
    #[test]
    fn a_locked_file_is_unreadable_not_corrupt_and_is_never_overwritten() {
        // A sharing violation (another process holds the file without
        // sharing) is the transient case: the file may be perfectly good, so
        // it is neither renamed nor replaced, and the app runs on defaults
        // with saving refused until it can read the file again.
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempdir().unwrap();
        let good = good_value().to_string();
        write_settings(dir.path(), &good);
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(dir.path().join(SETTINGS_FILE_NAME))
            .unwrap();

        let store = SettingsStore::load_from(dir.path());
        assert_eq!(store.get(), Settings::default());
        assert!(backups(dir.path()).is_empty(), "an unreadable file is not quarantined");
        let warning = store.view().storage_warning.expect("the problem is surfaced");
        assert!(warning.contains("could not be read"), "warning: {warning}");
        let err = store.apply_patch(resume_patch("would overwrite")).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        store.save_window_bounds(RawBounds { x: 1.0, y: 2.0, width: 460.0, height: 700.0 });

        drop(lock);
        assert_eq!(fs::read_to_string(dir.path().join(SETTINGS_FILE_NAME)).unwrap(), good);
        // The next launch reads it whole.
        assert_eq!(load(dir.path()), good_settings());
    }

    #[test]
    fn each_corrupt_field_falls_back_alone() {
        // The core promise of per-field validation: corrupting any single
        // value defaults that value and nothing else. Whole-struct equality
        // proves both halves at once.
        #[allow(clippy::type_complexity)]
        let cases: [(&str, Value, fn(&mut Settings)); 10] = [
            // A corrupt `profiles` VALUE cannot be salvaged: one empty Default
            // profile (there is no legacy resume in this file), keys intact.
            ("profiles", json!(42), |s| {
                s.profiles = vec![CallProfile::empty(DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME)];
                s.active_profile_id = DEFAULT_PROFILE_ID.to_string();
            }),
            // Non-string active id: the first profile, not the second.
            ("activeProfileId", json!(7), |s| s.active_profile_id = "default".to_string()),
            ("alwaysOnTop", json!("yes"), |s| s.always_on_top = true),
            ("llmProvider", json!(7), |s| s.llm_provider = LlmProviderKind::Anthropic),
            ("answerStyle", json!(["brief"]), |s| s.answer_style = AnswerStyle::Balanced),
            ("hotkey", json!(false), |s| s.hotkey = DEFAULT_HOTKEY.to_string()),
            ("launchPlacement", json!(3), |s| s.launch_placement = LaunchPlacement::Camera),
            ("streamFollow", json!(["top"]), |s| s.stream_follow = StreamFollow::Tail),
            ("deepgramKey", json!(12), |s| s.deepgram_key = None),
            (
                "windowBounds",
                json!({"x": "a", "y": 2.0, "width": 3.0, "height": 4.0}),
                |s| s.window_bounds = None,
            ),
        ];
        for (field, bad, expect_default) in cases {
            let dir = tempdir().unwrap();
            let mut file = good_value();
            file[field] = bad;
            write_settings(dir.path(), &file.to_string());

            let mut expected = good_settings();
            expect_default(&mut expected);
            assert_eq!(load(dir.path()), expected, "corrupt field: {field}");
        }
    }

    #[test]
    fn corrupt_answer_style_never_costs_the_resume_or_the_keys() {
        // The named disaster from the spec: one bad enum string must not read
        // as "corrupt file" and wipe everything.
        let dir = tempdir().unwrap();
        let mut file = good_value();
        file["answerStyle"] = json!("verbose");
        write_settings(dir.path(), &file.to_string());

        let got = load(dir.path());
        assert_eq!(got.answer_style, AnswerStyle::Balanced);
        assert_eq!(got.profiles[0].resume, "Resume text");
        assert_eq!(got.active_profile().resume, "About me");
        assert_eq!(got.deepgram_key.as_deref(), Some("dg-key"));
        assert_eq!(got.anthropic_key.as_deref(), Some("ant-key"));
        assert_eq!(got.groq_key.as_deref(), Some("gq-key"));
    }

    #[test]
    fn unknown_launch_placement_falls_back_to_camera() {
        // A hand-edited or downgraded value must not read as "corrupt file":
        // the placement defaults and every profile and key survives.
        let dir = tempdir().unwrap();
        let mut file = good_value();
        file["launchPlacement"] = json!("sideways");
        write_settings(dir.path(), &file.to_string());

        let got = load(dir.path());
        assert_eq!(got.launch_placement, LaunchPlacement::Camera);
        assert_eq!(got.active_profile().resume, "About me");
        assert_eq!(got.deepgram_key.as_deref(), Some("dg-key"));
        assert_eq!(got.stream_follow, StreamFollow::Top);
    }

    #[test]
    fn unknown_stream_follow_falls_back_to_tail() {
        let dir = tempdir().unwrap();
        let mut file = good_value();
        file["streamFollow"] = json!("middle");
        write_settings(dir.path(), &file.to_string());

        let got = load(dir.path());
        assert_eq!(got.stream_follow, StreamFollow::Tail);
        assert_eq!(got.active_profile().resume, "About me");
        assert_eq!(got.anthropic_key.as_deref(), Some("ant-key"));
        assert_eq!(got.launch_placement, LaunchPlacement::Remembered);
    }

    #[test]
    fn launch_placement_round_trips_through_disk() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store
            .apply_patch(SettingsPatch { launch_placement: Some(LaunchPlacement::Remembered), ..Default::default() })
            .unwrap();
        assert_eq!(store.get().launch_placement, LaunchPlacement::Remembered);
        assert_eq!(load(dir.path()).launch_placement, LaunchPlacement::Remembered);
        assert_eq!(read_raw(dir.path())["launchPlacement"], json!("remembered"));

        store
            .apply_patch(SettingsPatch { launch_placement: Some(LaunchPlacement::Camera), ..Default::default() })
            .unwrap();
        assert_eq!(load(dir.path()).launch_placement, LaunchPlacement::Camera);
        assert_eq!(read_raw(dir.path())["launchPlacement"], json!("camera"));
    }

    #[test]
    fn stream_follow_round_trips_through_disk() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store.apply_patch(SettingsPatch { stream_follow: Some(StreamFollow::Top), ..Default::default() }).unwrap();
        assert_eq!(store.get().stream_follow, StreamFollow::Top);
        assert_eq!(load(dir.path()).stream_follow, StreamFollow::Top);
        assert_eq!(read_raw(dir.path())["streamFollow"], json!("top"));

        store.apply_patch(SettingsPatch { stream_follow: Some(StreamFollow::Tail), ..Default::default() }).unwrap();
        assert_eq!(load(dir.path()).stream_follow, StreamFollow::Tail);
        assert_eq!(read_raw(dir.path())["streamFollow"], json!("tail"));
    }

    // ------------------------------------------------------- profiles ----

    #[test]
    fn v3_flat_file_migrates_into_one_default_interview_profile() {
        // The pre-profiles file: top-level resume / jobDescription, no
        // `profiles` key. It must load as exactly the prompt the user had.
        let dir = tempdir().unwrap();
        write_settings(
            dir.path(),
            &json!({
                "resume": "Resume text",
                "jobDescription": "JD text",
                "hotkey": "Ctrl+K",
                "deepgramKey": plain("dg-key"),
                "anthropicKey": plain("ant-key"),
                "windowBounds": {"x": 10.0, "y": 20.0, "width": 500.0, "height": 700.0}
            })
            .to_string(),
        );
        let got = load(dir.path());
        assert_eq!(got.profiles.len(), 1);
        let p = got.active_profile();
        assert_eq!((p.id.as_str(), p.name.as_str(), p.call_type), ("default", "Default", CallType::Interview));
        assert_eq!((p.resume.as_str(), p.job_description.as_str()), ("Resume text", "JD text"));
        assert_eq!((p.focus.as_str(), p.extra_instructions.as_str()), ("", ""));
        assert_eq!(got.active_profile_id, "default");
        // Migration never touches keys, the hotkey or the bounds.
        assert_eq!(got.deepgram_key.as_deref(), Some("dg-key"));
        assert_eq!(got.anthropic_key.as_deref(), Some("ant-key"));
        assert_eq!(got.hotkey, "Ctrl+K");
        assert_eq!(got.window_bounds, Some(RawBounds { x: 10.0, y: 20.0, width: 500.0, height: 700.0 }));
        // New fields absent from a v3 file take their defaults.
        assert_eq!(got.launch_placement, LaunchPlacement::Camera);
        assert_eq!(got.stream_follow, StreamFollow::Tail);
    }

    #[test]
    fn migrated_file_is_rewritten_in_the_new_shape_only() {
        // After the first save the file carries `profiles` and no top-level
        // resume / jobDescription — the legacy shape is read, never written.
        let dir = tempdir().unwrap();
        write_settings(
            dir.path(),
            &json!({ "resume": "Resume text", "jobDescription": "JD text", "deepgramKey": plain("dg-key") })
                .to_string(),
        );
        let store = SettingsStore::load_from(dir.path());
        store.apply_patch(SettingsPatch { always_on_top: Some(false), ..Default::default() }).unwrap();

        let raw = read_raw(dir.path());
        let obj = raw.as_object().unwrap();
        assert!(!obj.contains_key("resume"), "legacy resume written back: {raw}");
        assert!(!obj.contains_key("jobDescription"), "legacy jobDescription written back: {raw}");
        assert_eq!(raw["activeProfileId"], json!("default"));
        assert_eq!(
            raw["profiles"],
            json!([{
                "id": "default", "name": "Default", "callType": "interview",
                "resume": "Resume text", "jobDescription": "JD text",
                "focus": "", "extraInstructions": ""
            }])
        );
        assert_eq!(raw["launchPlacement"], json!("camera"));
        assert_eq!(raw["streamFollow"], json!("tail"));
        assert!(obj.contains_key("deepgramKey"));

        // And the rewritten file loads back to the same settings.
        assert_eq!(load(dir.path()), store.get());
        assert_eq!(load(dir.path()).active_profile().resume, "Resume text");
    }

    #[test]
    fn profiles_round_trip_through_disk() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        let view = store
            .apply_patch(SettingsPatch {
                profiles: Some(vec![
                    CallProfilePatch {
                        id: "8c1e-uuid_1".into(),
                        name: "Rust / systems".into(),
                        call_type: "interview".into(),
                        resume: "  R\n\n  indented  \n".into(),
                        job_description: "J".into(),
                        focus: "Rust, tokio".into(),
                        extra_instructions: "Mention the audio work".into(),
                    },
                    CallProfilePatch {
                        id: "acme".into(),
                        name: "Acme renewal".into(),
                        call_type: "sales".into(),
                        resume: "AE, 6 years".into(),
                        job_description: "Renewal call, 40 seats".into(),
                        focus: "".into(),
                        extra_instructions: "Never quote a price".into(),
                    },
                    CallProfilePatch { id: "sup".into(), name: "Support".into(), call_type: "support".into(), ..Default::default() },
                    CallProfilePatch { id: "mtg".into(), name: "Standup".into(), call_type: "meeting".into(), ..Default::default() },
                    CallProfilePatch { id: "oth".into(), name: "Other".into(), call_type: "other".into(), ..Default::default() },
                ]),
                active_profile_id: Some("acme".into()),
                ..Default::default()
            })
            .unwrap();
        let in_memory = store.get();
        let reloaded = load(dir.path());
        assert_eq!(reloaded, in_memory);
        assert_eq!(reloaded.profiles.len(), 5);
        assert_eq!(reloaded.active_profile_id, "acme");
        assert_eq!(reloaded.active_profile().call_type, CallType::Sales);
        // Verbatim, including the whitespace edges.
        assert_eq!(reloaded.profiles[0].resume, "  R\n\n  indented  \n");
        let kinds: Vec<CallType> = reloaded.profiles.iter().map(|p| p.call_type).collect();
        assert_eq!(kinds, [CallType::Interview, CallType::Sales, CallType::Support, CallType::Meeting, CallType::Other]);
        // The view the UI got back is the view a fresh load produces (the
        // revision is per run, so it is carried over, not compared).
        assert_eq!(view, reloaded.view_at(view.revision, None));
    }

    #[test]
    fn each_corrupt_profile_field_falls_back_alone() {
        // Per-field fallback INSIDE a profile: one bad value in one profile
        // costs only that value — never the profile, never its neighbours.
        let good = json!({
            "id": "rust", "name": "Rust / systems", "callType": "sales",
            "resume": "About me", "jobDescription": "Account context",
            "focus": "Rust, tokio", "extraInstructions": "Keep it short"
        });
        let expected_good = good_settings().profiles[1].clone();
        #[allow(clippy::type_complexity)]
        let cases: [(&str, Value, fn(&mut CallProfile)); 7] = [
            ("callType", json!(7), |p| p.call_type = CallType::Interview),
            ("resume", json!(42), |p| p.resume.clear()),
            ("jobDescription", json!({"a": 1}), |p| p.job_description.clear()),
            ("focus", json!(["x"]), |p| p.focus.clear()),
            ("extraInstructions", json!(true), |p| p.extra_instructions.clear()),
            ("name", json!(null), |p| p.name = UNNAMED_PROFILE_NAME.to_string()),
            // A non-string id is "" → invalid → repaired to the smallest free p<n>.
            ("id", json!(5), |p| p.id = "p1".to_string()),
        ];
        for (field, bad, expect_default) in cases {
            let dir = tempdir().unwrap();
            let mut profile = good.clone();
            profile[field] = bad;
            let mut file = good_value();
            file["profiles"] = json!([profile]);
            file["activeProfileId"] = json!("rust");
            write_settings(dir.path(), &file.to_string());

            let mut expected = expected_good.clone();
            expect_default(&mut expected);
            let got = load(dir.path());
            assert_eq!(got.profiles, vec![expected.clone()], "corrupt profile field: {field}");
            assert_eq!(got.active_profile_id, expected.id, "corrupt profile field: {field}");
            assert_eq!(got.deepgram_key.as_deref(), Some("dg-key"), "corrupt profile field: {field}");
        }
    }

    #[test]
    fn non_object_profile_entries_are_dropped_and_the_rest_survive() {
        // There is nothing in a number or a string to salvage as a profile;
        // the objects around it load exactly as if the junk were not there.
        let dir = tempdir().unwrap();
        let mut file = good_value();
        file["profiles"] = json!([
            42,
            { "id": "a", "name": "A", "resume": "RA" },
            "not a profile",
            null,
            { "id": "b", "name": "B", "resume": "RB" },
            [1, 2, 3]
        ]);
        file["activeProfileId"] = json!("b");
        write_settings(dir.path(), &file.to_string());

        let got = load(dir.path());
        let ids: Vec<&str> = got.profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(got.active_profile_id, "b");
        assert_eq!(got.active_profile().resume, "RB");
        assert_eq!(got.deepgram_key.as_deref(), Some("dg-key"));
    }

    #[test]
    fn corrupt_profiles_value_loads_as_default_profile_without_losing_keys() {
        // A `profiles` that is not an array cannot be salvaged: the user gets
        // one empty Default profile and keeps everything else. The named
        // disaster is "corrupt file → wiped keys"; this pins that it cannot
        // happen through this field either.
        for bad in [json!(42), json!("x"), json!({"id": "a"}), json!(null), json!(true)] {
            let dir = tempdir().unwrap();
            let mut file = good_value();
            file["profiles"] = bad.clone();
            write_settings(dir.path(), &file.to_string());

            let got = load(dir.path());
            assert_eq!(got.profiles, vec![default_profile()], "bad profiles: {bad}");
            assert_eq!(got.active_profile_id, DEFAULT_PROFILE_ID, "bad profiles: {bad}");
            assert_eq!(got.hotkey, "Ctrl+K", "bad profiles: {bad}");
            assert_eq!(got.deepgram_key.as_deref(), Some("dg-key"), "bad profiles: {bad}");
            assert_eq!(got.anthropic_key.as_deref(), Some("ant-key"), "bad profiles: {bad}");
            assert_eq!(got.groq_key.as_deref(), Some("gq-key"), "bad profiles: {bad}");
            assert_eq!(got.window_bounds, good_settings().window_bounds, "bad profiles: {bad}");
        }
    }

    #[test]
    fn empty_profiles_array_yields_a_default_profile() {
        let dir = tempdir().unwrap();
        write_settings(dir.path(), &json!({ "profiles": [], "activeProfileId": "zzz" }).to_string());
        let got = load(dir.path());
        assert_eq!(got.profiles, vec![default_profile()]);
        assert_eq!(got.active_profile_id, DEFAULT_PROFILE_ID);

        // Same through a patch: the form cannot delete the last profile, but
        // the core does not rely on the form.
        let dir2 = tempdir().unwrap();
        let store = SettingsStore::load_from(dir2.path());
        let view = store.apply_patch(SettingsPatch { profiles: Some(vec![]), ..Default::default() }).unwrap();
        assert_eq!(view.profiles, vec![default_profile()]);
        assert_eq!(view.active_profile_id, DEFAULT_PROFILE_ID);
    }

    #[test]
    fn more_than_max_profiles_are_truncated() {
        let many: Vec<Value> = (0..12)
            .map(|i| json!({ "id": format!("q{i}"), "name": format!("Q{i}"), "resume": format!("R{i}") }))
            .collect();

        // On load: the first MAX_PROFILES survive in order; an active id that
        // fell off the end falls back to the first.
        let dir = tempdir().unwrap();
        write_settings(dir.path(), &json!({ "profiles": many, "activeProfileId": "q11" }).to_string());
        let got = load(dir.path());
        assert_eq!(got.profiles.len(), MAX_PROFILES);
        let ids: Vec<&str> = got.profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["q0", "q1", "q2", "q3", "q4", "q5", "q6", "q7"]);
        assert_eq!(got.active_profile_id, "q0");

        // On patch: same cut, and the requested active must be one that survived.
        let dir2 = tempdir().unwrap();
        let store = SettingsStore::load_from(dir2.path());
        let view = store
            .apply_patch(SettingsPatch {
                profiles: Some((0..12).map(|i| profile_patch(&format!("q{i}"), "Q", "R")).collect()),
                active_profile_id: Some("q7".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(view.profiles.len(), MAX_PROFILES);
        assert_eq!(view.active_profile_id, "q7");
    }

    #[test]
    fn duplicate_or_invalid_ids_are_repaired_deterministically() {
        // Ids are DOM ids and JSON keys downstream; anything outside
        // [A-Za-z0-9_-]{1,40}, and any repeat, is replaced with the smallest
        // unused p<n> in list order. Same input, same output — pinned by
        // running it twice, because a random or clock-based repair would make
        // the cached prefix nondeterministic through the store (ADR 007).
        let input: Vec<CallProfile> = [
            "",
            "dup",
            "dup",
            "has space",
            "p1",
            &"x".repeat(MAX_PROFILE_ID_CHARS + 1),
            "ok_id-9",
            "café",
        ]
        .iter()
        .map(|id| CallProfile::empty(id, "N"))
        .collect();

        let (first, active) = normalize_profiles(input.clone(), Some("dup"), None);
        let (second, active2) = normalize_profiles(input.clone(), Some("dup"), None);
        assert_eq!(first, second);
        assert_eq!(active, active2);

        let ids: Vec<&str> = first.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["p2", "dup", "p3", "p4", "p1", "p5", "ok_id-9", "p6"]);
        assert_eq!(active, "dup");
        // A 40-char id is the longest valid one.
        let long_ok = "x".repeat(MAX_PROFILE_ID_CHARS);
        let (kept, _) = normalize_profiles(vec![CallProfile::empty(&long_ok, "N")], None, None);
        assert_eq!(kept[0].id, long_ok);

        // Through the store the repair is what the UI gets back AND what the
        // next launch loads — the two never disagree.
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        let view = store
            .apply_patch(SettingsPatch {
                profiles: Some(input.into_iter().map(|p| profile_patch(&p.id, &p.name, "")).collect()),
                ..Default::default()
            })
            .unwrap();
        let view_ids: Vec<&str> = view.profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(view_ids, ids);
        assert_eq!(load(dir.path()).profiles, view.profiles);
    }

    #[test]
    fn normalize_is_idempotent() {
        // A bare active-id switch re-runs normalize over the stored list; if
        // a second pass could change anything, a switch would rewrite text.
        let (once, active) = normalize_profiles(
            vec![
                CallProfile { name: "  padded  ".into(), resume: "  R  ".into(), ..CallProfile::empty("", "") },
                CallProfile::empty("b", ""),
            ],
            Some("b"),
            None,
        );
        let (twice, active2) = normalize_profiles(once.clone(), Some(&active), Some(&active));
        assert_eq!(once, twice);
        assert_eq!(active, active2);
        assert_eq!(once[0].name, "padded");
        assert_eq!(once[0].resume, "  R  ");
        assert_eq!(once[1].name, UNNAMED_PROFILE_NAME);
    }

    #[test]
    fn unknown_active_id_falls_back_to_first_on_load() {
        let dir = tempdir().unwrap();
        let mut file = good_value();
        file["activeProfileId"] = json!("vanished");
        write_settings(dir.path(), &file.to_string());
        let got = load(dir.path());
        assert_eq!(got.active_profile_id, "default");
        assert_eq!(got.active_profile().resume, "Resume text");
        // Nothing else moved.
        assert_eq!(got.profiles, good_settings().profiles);
    }

    #[test]
    fn unknown_active_id_keeps_current_active_on_patch() {
        // A stale switch (the chip row raced a delete) must not jump to the
        // first profile: the current one stays, and the returned view says so.
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store
            .apply_patch(SettingsPatch {
                profiles: Some(vec![profile_patch("a", "A", "RA"), profile_patch("b", "B", "RB")]),
                active_profile_id: Some("b".into()),
                ..Default::default()
            })
            .unwrap();
        let view = store
            .apply_patch(SettingsPatch { active_profile_id: Some("zzz".into()), ..Default::default() })
            .unwrap();
        assert_eq!(view.active_profile_id, "b");
        assert_eq!(store.get().active_profile_id, "b");
        assert_eq!(load(dir.path()).active_profile_id, "b");
    }

    #[test]
    fn switch_patch_changes_only_active_profile_id() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store
            .apply_patch(SettingsPatch {
                profiles: Some(vec![
                    CallProfilePatch {
                        id: "a".into(),
                        name: "A".into(),
                        call_type: "sales".into(),
                        resume: "  RA  ".into(),
                        job_description: "JA".into(),
                        focus: "FA".into(),
                        extra_instructions: "EA".into(),
                    },
                    profile_patch("b", "B", "RB"),
                ]),
                hotkey: Some("Ctrl+K".into()),
                deepgram_key: Some("dg".into()),
                ..Default::default()
            })
            .unwrap();
        let before = store.get();
        let raw_before = read_raw(dir.path());
        assert_eq!(before.active_profile_id, "a");

        let view = store
            .apply_patch(SettingsPatch { active_profile_id: Some("b".into()), ..Default::default() })
            .unwrap();
        let after = store.get();
        assert_eq!(view.active_profile_id, "b");
        assert_eq!(after.active_profile_id, "b");
        // Not one byte of profile text, key or hotkey changed.
        assert_eq!(after.profiles, before.profiles);
        assert_eq!(after.deepgram_key, before.deepgram_key);
        assert_eq!(after.hotkey, before.hotkey);
        let raw_after = read_raw(dir.path());
        assert_eq!(raw_after["profiles"], raw_before["profiles"]);
        assert_eq!(raw_after["hotkey"], raw_before["hotkey"]);
        assert_eq!(raw_after["activeProfileId"], json!("b"));
        // The stored key is compared decrypted (above), not as ciphertext:
        // DPAPI randomizes every encryption, so identical plaintext re-saves
        // to different bytes and a raw comparison would fail for no reason.
        assert!(raw_after["deepgramKey"].is_string());

        // A stale switch to a vanished id keeps the current active rather
        // than jumping to the first.
        store
            .apply_patch(SettingsPatch { active_profile_id: Some("zzz".into()), ..Default::default() })
            .unwrap();
        assert_eq!(store.get().active_profile_id, "b");
    }

    #[test]
    fn replacing_away_the_active_profile_falls_back_unless_patch_names_a_new_active() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store
            .apply_patch(SettingsPatch {
                profiles: Some(vec![profile_patch("a", "A", "RA"), profile_patch("b", "B", "RB")]),
                active_profile_id: Some("b".into()),
                ..Default::default()
            })
            .unwrap();

        // The form deleted "b" and did not say what to activate: the first
        // profile, since the previous one is gone.
        let view = store
            .apply_patch(SettingsPatch {
                profiles: Some(vec![profile_patch("a", "A", "RA"), profile_patch("c", "C", "RC")]),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(view.active_profile_id, "a");

        // The form deleted "a" and selected "c": "c".
        let view = store
            .apply_patch(SettingsPatch {
                profiles: Some(vec![profile_patch("c", "C", "RC"), profile_patch("d", "D", "RD")]),
                active_profile_id: Some("c".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(view.active_profile_id, "c");

        // The previous active survives a replace that does not mention it.
        let view = store
            .apply_patch(SettingsPatch {
                profiles: Some(vec![profile_patch("d", "D", "RD"), profile_patch("c", "C", "RC2")]),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(view.active_profile_id, "c");
        assert_eq!(store.get().active_profile().resume, "RC2");
    }

    #[test]
    fn per_profile_caps_apply_on_load_and_on_patch() {
        // Characters, not bytes: 'é' is two bytes, and a byte cut would
        // poison the file for every later load.
        let name = "é".repeat(MAX_PROFILE_NAME_CHARS + 1);
        let resume = "é".repeat(MAX_PROFILE_CHARS + 50);
        let jd = "é".repeat(MAX_PROFILE_CHARS + 1);
        let focus = "é".repeat(MAX_FOCUS_CHARS + 1);
        let extra = "é".repeat(MAX_EXTRA_INSTRUCTIONS_CHARS + 1);

        let check = |p: &CallProfile, where_: &str| {
            assert_eq!(p.name.chars().count(), MAX_PROFILE_NAME_CHARS, "{where_}: name");
            assert_eq!(p.resume.chars().count(), MAX_PROFILE_CHARS, "{where_}: resume");
            assert_eq!(p.job_description.chars().count(), MAX_PROFILE_CHARS, "{where_}: jd");
            assert_eq!(p.focus.chars().count(), MAX_FOCUS_CHARS, "{where_}: focus");
            assert_eq!(p.extra_instructions.chars().count(), MAX_EXTRA_INSTRUCTIONS_CHARS, "{where_}: extra");
            for text in [&p.name, &p.resume, &p.job_description, &p.focus, &p.extra_instructions] {
                assert!(text.chars().all(|c| c == 'é'), "{where_}: split character");
            }
        };

        let dir = tempdir().unwrap();
        write_settings(
            dir.path(),
            &json!({ "profiles": [{ "id": "a", "name": name, "resume": resume, "jobDescription": jd, "focus": focus, "extraInstructions": extra }] })
                .to_string(),
        );
        check(&load(dir.path()).profiles[0], "load");

        let dir2 = tempdir().unwrap();
        let store = SettingsStore::load_from(dir2.path());
        let view = store
            .apply_patch(SettingsPatch {
                profiles: Some(vec![CallProfilePatch {
                    id: "a".into(),
                    name,
                    call_type: String::new(),
                    resume,
                    job_description: jd,
                    focus,
                    extra_instructions: extra,
                }]),
                ..Default::default()
            })
            .unwrap();
        check(&view.profiles[0], "patch view");
        check(&store.get().profiles[0], "patch memory");
        check(&load(dir2.path()).profiles[0], "patch reload");
    }

    #[test]
    fn profile_name_is_trimmed_and_blank_becomes_untitled() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        let view = store
            .apply_patch(SettingsPatch {
                profiles: Some(vec![
                    profile_patch("a", "  Rust  ", ""),
                    profile_patch("b", "   ", ""),
                    profile_patch("c", "", ""),
                ]),
                ..Default::default()
            })
            .unwrap();
        let names: Vec<&str> = view.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Rust", UNNAMED_PROFILE_NAME, UNNAMED_PROFILE_NAME]);
    }

    #[test]
    fn over_length_resume_is_truncated_on_load_and_on_patch() {
        let long = "a".repeat(MAX_PROFILE_CHARS + 50);

        // Legacy flat file and the profiles shape both cap.
        let dir = tempdir().unwrap();
        write_settings(dir.path(), &json!({ "resume": long }).to_string());
        assert_eq!(load(dir.path()).active_profile().resume.chars().count(), MAX_PROFILE_CHARS);

        let dir2 = tempdir().unwrap();
        let store = SettingsStore::load_from(dir2.path());
        store.apply_patch(resume_patch(&long)).unwrap();
        assert_eq!(store.get().active_profile().resume.chars().count(), MAX_PROFILE_CHARS);
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        // A multi-byte cap cut at a byte boundary would poison the file for
        // every later load.
        let long = "é".repeat(MAX_PROFILE_CHARS + 10);
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store.apply_patch(resume_patch(&long)).unwrap();
        let saved = store.get().active_profile().resume.clone();
        assert_eq!(saved.chars().count(), MAX_PROFILE_CHARS);
        assert!(saved.chars().all(|c| c == 'é'));
        assert_eq!(load(dir.path()).active_profile().resume, saved);
    }

    #[test]
    fn resume_is_stored_verbatim_never_trimmed() {
        // Profile formatting belongs to the user; leading/trailing whitespace
        // survives the full round trip.
        let text = "  \n  My resume, indented on purpose  \n\n";
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store.apply_patch(resume_patch(text)).unwrap();
        assert_eq!(store.get().active_profile().resume, text);
        assert_eq!(load(dir.path()).active_profile().resume, text);
    }

    #[test]
    fn empty_hotkey_means_disabled_and_never_reverts_to_default() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store.apply_patch(SettingsPatch { hotkey: Some(String::new()), ..Default::default() }).unwrap();
        assert_eq!(store.get().hotkey, "");
        // The reload is the trap: an absent-vs-empty confusion here would
        // resurrect the shortcut the user turned off.
        assert_eq!(load(dir.path()).hotkey, "");
    }

    #[test]
    fn whitespace_only_hotkey_becomes_empty() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store.apply_patch(SettingsPatch { hotkey: Some("  \t ".into()), ..Default::default() }).unwrap();
        assert_eq!(store.get().hotkey, "");
    }

    #[test]
    fn hotkey_is_trimmed_and_capped() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store
            .apply_patch(SettingsPatch { hotkey: Some("  Ctrl+Alt+K  ".into()), ..Default::default() })
            .unwrap();
        assert_eq!(store.get().hotkey, "Ctrl+Alt+K");

        store
            .apply_patch(SettingsPatch { hotkey: Some("x".repeat(MAX_HOTKEY_CHARS + 20)), ..Default::default() })
            .unwrap();
        assert_eq!(store.get().hotkey.chars().count(), MAX_HOTKEY_CHARS);
    }

    #[test]
    fn missing_hotkey_field_gets_the_default() {
        let dir = tempdir().unwrap();
        write_settings(dir.path(), &json!({ "resume": "r" }).to_string());
        assert_eq!(load(dir.path()).hotkey, DEFAULT_HOTKEY);
    }

    #[test]
    fn omitted_key_field_leaves_the_stored_key_untouched() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store
            .apply_patch(SettingsPatch { deepgram_key: Some("dg-1".into()), ..Default::default() })
            .unwrap();
        // A typical patch — the user edited the resume, not the key.
        store.apply_patch(resume_patch("r")).unwrap();
        assert_eq!(store.get().deepgram_key.as_deref(), Some("dg-1"));
        assert_eq!(load(dir.path()).deepgram_key.as_deref(), Some("dg-1"));
        // And a bare profile switch, the other common patch.
        store
            .apply_patch(SettingsPatch { active_profile_id: Some(DEFAULT_PROFILE_ID.into()), ..Default::default() })
            .unwrap();
        assert_eq!(load(dir.path()).deepgram_key.as_deref(), Some("dg-1"));
    }

    #[test]
    fn empty_key_patch_clears_the_stored_key() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store
            .apply_patch(SettingsPatch { anthropic_key: Some("ant-1".into()), ..Default::default() })
            .unwrap();
        let view = store
            .apply_patch(SettingsPatch { anthropic_key: Some(String::new()), ..Default::default() })
            .unwrap();
        assert!(!view.has_anthropic_key);
        assert_eq!(store.get().anthropic_key, None);
        assert_eq!(load(dir.path()).anthropic_key, None);
    }

    #[test]
    fn key_patch_values_are_trimmed() {
        // Pasted keys arrive with the clipboard's stray whitespace, which the
        // provider would reject with an auth error mid-call.
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store
            .apply_patch(SettingsPatch { groq_key: Some("  gq-value  ".into()), ..Default::default() })
            .unwrap();
        assert_eq!(store.get().groq_key.as_deref(), Some("gq-value"));
    }

    #[test]
    fn view_never_exposes_key_material() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        let view = store
            .apply_patch(SettingsPatch {
                deepgram_key: Some("dg-secret-material".into()),
                anthropic_key: Some("ant-secret-material".into()),
                ..Default::default()
            })
            .unwrap();

        assert!(view.has_deepgram_key);
        assert!(view.has_anthropic_key);
        assert!(!view.has_groq_key);

        // Serialize the exact type that crosses the IPC boundary and prove the
        // material is not in it — booleans are all the UI ever gets.
        let wire = serde_json::to_string(&view).unwrap();
        assert!(!wire.contains("secret-material"), "wire: {wire}");
        assert!(wire.contains("\"hasDeepgramKey\":true"));
        assert_eq!(store.view(), view);
    }

    #[test]
    fn keys_are_never_written_to_disk_in_plaintext() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store
            .apply_patch(SettingsPatch { deepgram_key: Some("sk-super-secret".into()), ..Default::default() })
            .unwrap();

        let raw = fs::read_to_string(dir.path().join(SETTINGS_FILE_NAME)).unwrap();
        assert!(!raw.contains("sk-super-secret"), "plaintext key on disk");
        let stored = serde_json::from_str::<Value>(&raw).unwrap()["deepgramKey"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            stored.starts_with("enc:") || stored.starts_with("plain:"),
            "unprefixed key encoding: {stored}"
        );
    }

    #[test]
    fn plain_prefixed_key_on_disk_is_read_back() {
        // Decode dispatches on the stored prefix, not on whether DPAPI is
        // available right now.
        let dir = tempdir().unwrap();
        write_settings(dir.path(), &json!({ "groqKey": plain("gq-imported") }).to_string());
        assert_eq!(load(dir.path()).groq_key.as_deref(), Some("gq-imported"));
    }

    #[test]
    fn undecryptable_key_reads_as_unset_without_losing_other_fields() {
        // A settings file copied from another machine: DPAPI is per-user, so
        // the blob will not open. The key must vanish — not surface as
        // ciphertext-as-key — and everything else must survive.
        let dir = tempdir().unwrap();
        let mut file = good_value();
        file["deepgramKey"] = json!(format!("enc:{}", B64.encode("blob from another machine")));
        write_settings(dir.path(), &file.to_string());

        let got = load(dir.path());
        assert_eq!(got.deepgram_key, None);
        assert_eq!(got.profiles, good_settings().profiles);
        assert_eq!(got.active_profile().resume, "About me");
        assert_eq!(got.anthropic_key.as_deref(), Some("ant-key"));
    }

    #[test]
    fn successful_save_leaves_no_tmp_file() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store.apply_patch(resume_patch("r")).unwrap();
        assert!(dir.path().join(SETTINGS_FILE_NAME).exists());
        assert!(!dir.path().join(SETTINGS_TMP_NAME).exists(), "tmp left behind after rename");
    }

    #[test]
    fn a_stale_tmp_file_does_not_corrupt_a_load_or_the_next_save() {
        // The crashed-mid-write aftermath: garbage tmp beside a good file.
        let dir = tempdir().unwrap();
        write_settings(dir.path(), &good_value().to_string());
        fs::write(dir.path().join(SETTINGS_TMP_NAME), "{ half a wri").unwrap();

        let store = SettingsStore::load_from(dir.path());
        assert_eq!(store.get(), good_settings());

        // The next save overwrites the stale tmp rather than tripping on it.
        store
            .apply_patch(SettingsPatch {
                profiles: Some(vec![profile_patch("rust", "Rust / systems", "updated")]),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(load(dir.path()).active_profile().resume, "updated");
        assert!(!dir.path().join(SETTINGS_TMP_NAME).exists());
    }

    #[test]
    fn window_bounds_round_trip() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        let b = RawBounds { x: -5.0, y: 40.5, width: 460.0, height: 700.0 };
        store.save_window_bounds(b);
        assert_eq!(store.window_bounds(), Some(b));
        assert_eq!(load(dir.path()).window_bounds, Some(b));
    }

    #[test]
    fn corrupt_bounds_are_dropped_without_losing_other_fields() {
        for bad in [
            json!({"x": 1.0, "y": 2.0, "width": 3.0}),           // missing height
            json!({"x": null, "y": 2.0, "width": 3.0, "height": 4.0}),
            json!("10,20,300,400"),
            json!([1, 2, 3, 4]),
        ] {
            let dir = tempdir().unwrap();
            let mut file = good_value();
            file["windowBounds"] = bad.clone();
            write_settings(dir.path(), &file.to_string());

            let got = load(dir.path());
            assert_eq!(got.window_bounds, None, "bad bounds: {bad}");
            assert_eq!(got.active_profile().resume, "About me", "bad bounds: {bad}");
        }
    }

    #[test]
    fn save_window_bounds_swallows_write_failures() {
        // Forced failure: a directory squatting on the settings path makes the
        // rename fail on every platform. This runs during shutdown, so the
        // only acceptable outcomes are "saved" or "silently didn't".
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(SETTINGS_FILE_NAME)).unwrap();

        let store = SettingsStore::load_from(dir.path());
        store.save_window_bounds(RawBounds { x: 1.0, y: 2.0, width: 460.0, height: 700.0 });
        // Cache follows disk: the failed write must not leave memory claiming
        // bounds that will not be there next launch.
        assert_eq!(store.window_bounds(), None);
    }

    #[test]
    fn failed_patch_reports_an_error_and_leaves_memory_matching_disk() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(SETTINGS_FILE_NAME)).unwrap();

        let store = SettingsStore::load_from(dir.path());
        let err = store.apply_patch(resume_patch("never lands")).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::Internal);
        // The cache was not updated, so the UI cannot show settings that will
        // silently vanish on restart.
        assert_eq!(store.get().active_profile().resume, "");
    }

    // --- R5: revisions (ADR 016) -------------------------------------------

    fn form_patch(expected: u64, resume: &str) -> SettingsPatch {
        SettingsPatch { expected_revision: Some(expected), ..resume_patch(resume) }
    }

    #[test]
    fn revision_starts_at_one_and_advances_once_per_committed_patch() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        assert_eq!(store.view().revision, 1);
        let v2 = store.apply_patch(resume_patch("a")).unwrap();
        assert_eq!(v2.revision, 2);
        let v3 = store
            .apply_patch(SettingsPatch { answer_style: Some(AnswerStyle::Brief), ..Default::default() })
            .unwrap();
        assert_eq!(v3.revision, 3);
        assert_eq!(store.revision(), 3);
        assert_eq!(store.view(), v3, "the returned view is the committed view");
    }

    #[test]
    fn a_stale_form_is_rejected_with_settings_conflict_and_changes_nothing() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        let seeded = store.view().revision;
        // A chip switch lands while the form is open.
        store
            .apply_patch(SettingsPatch { answer_style: Some(AnswerStyle::Detailed), ..Default::default() })
            .unwrap();
        let before_disk = fs::read(dir.path().join(SETTINGS_FILE_NAME)).unwrap();

        let err = store.apply_patch(form_patch(seeded, "stale text")).unwrap_err();
        assert_eq!(err.code, ErrorCode::SettingsConflict);
        assert_eq!(err.message, MSG_SETTINGS_CONFLICT);
        assert_eq!(store.revision(), seeded + 1);
        assert_eq!(store.get().active_profile().resume, "");
        assert_eq!(store.get().answer_style, AnswerStyle::Detailed, "the chip's change survives");
        assert_eq!(fs::read(dir.path().join(SETTINGS_FILE_NAME)).unwrap(), before_disk);

        // Reloaded, the same form commits against the current revision.
        let ok = store.apply_patch(form_patch(store.revision(), "fresh text")).unwrap();
        assert_eq!(ok.revision, seeded + 2);
        assert_eq!(load(dir.path()).active_profile().resume, "fresh text");
        assert_eq!(load(dir.path()).answer_style, AnswerStyle::Detailed);
    }

    #[test]
    fn single_field_patches_without_a_revision_merge_whatever_order_they_commit_in() {
        // The chips send one distinct field and no revision: each must merge
        // with anything committed meanwhile, in either order.
        for style_first in [true, false] {
            let dir = tempdir().unwrap();
            write_settings(dir.path(), &good_value().to_string());
            let store = SettingsStore::load_from(dir.path());
            let style = || SettingsPatch { answer_style: Some(AnswerStyle::Brief), ..Default::default() };
            let switch = || SettingsPatch { active_profile_id: Some("default".into()), ..Default::default() };
            if style_first {
                store.apply_patch(style()).unwrap();
                store.apply_patch(switch()).unwrap();
            } else {
                store.apply_patch(switch()).unwrap();
                store.apply_patch(style()).unwrap();
            }
            let disk = load(dir.path());
            assert_eq!((disk.answer_style, disk.active_profile_id.as_str()), (AnswerStyle::Brief, "default"));
            assert_eq!(store.revision(), 3);
        }
    }

    #[test]
    fn a_failed_write_never_advances_the_revision() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store.apply_patch(resume_patch("saved")).unwrap();
        let committed = store.revision();
        // Break the next write after a healthy load: a directory squatting
        // on the tmp name makes File::create fail.
        fs::create_dir(dir.path().join(SETTINGS_TMP_NAME)).unwrap();
        let err = store.apply_patch(form_patch(committed, "lost")).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert_eq!(store.revision(), committed, "memory and revision still match disk");
        assert_eq!(store.get().active_profile().resume, "saved");

        // The same form, retried once the disk recovers, is still current.
        fs::remove_dir(dir.path().join(SETTINGS_TMP_NAME)).unwrap();
        let view = store.apply_patch(form_patch(committed, "landed")).unwrap();
        assert_eq!(view.revision, committed + 1);
        assert_eq!(load(dir.path()).active_profile().resume, "landed");
    }

    #[test]
    fn geometry_saves_never_advance_the_revision() {
        // Moving the window saves bounds through the same store; an open
        // Settings form must not become stale because of it.
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        let seeded = store.revision();
        store.save_window_bounds(RawBounds { x: 1.0, y: 2.0, width: 460.0, height: 700.0 });
        store.save_window_bounds(RawBounds { x: 9.0, y: 9.0, width: 500.0, height: 700.0 });
        assert_eq!(store.revision(), seeded);
        store.apply_patch(form_patch(seeded, "still current")).expect("the form is not stale");
        assert_eq!(
            load(dir.path()).window_bounds,
            Some(RawBounds { x: 9.0, y: 9.0, width: 500.0, height: 700.0 }),
            "and the form's save keeps the geometry"
        );
    }

    #[test]
    fn racing_full_form_saves_from_one_revision_commit_exactly_once() {
        // The compare and the commit share one lock: of N forms seeded from
        // the same revision, exactly one wins and the rest see a conflict.
        let dir = tempdir().unwrap();
        let store = std::sync::Arc::new(SettingsStore::load_from(dir.path()));
        let seeded = store.revision();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let store = std::sync::Arc::clone(&store);
                std::thread::spawn(move || store.apply_patch(form_patch(seeded, &format!("form {i}"))))
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let winners: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
        assert_eq!(winners.len(), 1);
        assert!(results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| e.code == ErrorCode::SettingsConflict));
        assert_eq!(store.revision(), seeded + 1);
        assert_eq!(load(dir.path()).active_profile().resume, winners[0].profiles[0].resume);
    }
}

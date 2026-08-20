//! The settings store (§8): one JSON file, atomic writes, per-field fallback.
//!
//! Two failure modes shape everything here:
//!
//! 1. **The file is untrusted.** It is user-writable and survives upgrades, so
//!    every field validates and falls back *individually* — one corrupt value
//!    must never cost the user their resume or their keys. An unparseable file
//!    loads as first-run defaults, never a crash.
//! 2. **Writes must be atomic.** The file also holds the (encrypted) keys, so
//!    a crash or full disk mid-write that truncated it would silently destroy
//!    every setting. Writes go to `settings.json.tmp` and rename over the real
//!    file; the in-memory cache is updated only after the write lands, so a
//!    failed write leaves memory matching disk.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde_json::{Map, Value};

use crate::error::AppError;
use crate::llm::{AnswerStyle, LlmProviderKind};

use super::{
    secrets, RawBounds, Settings, SettingsPatch, SettingsView, DEFAULT_HOTKEY, MAX_HOTKEY_CHARS,
    MAX_PROFILE_CHARS,
};

pub const SETTINGS_FILE_NAME: &str = "settings.json";
pub const SETTINGS_TMP_NAME: &str = "settings.json.tmp";

pub struct SettingsStore {
    path: PathBuf,
    tmp_path: PathBuf,
    cache: RwLock<Settings>,
}

impl SettingsStore {
    /// Load from `dir/settings.json`. Never fails: a missing, unreadable or
    /// corrupt file loads as defaults — startup must not depend on the health
    /// of a user-editable file.
    pub fn load_from(dir: &Path) -> Self {
        let path = dir.join(SETTINGS_FILE_NAME);
        let tmp_path = dir.join(SETTINGS_TMP_NAME);
        // Only settings.json is ever read. A stale .tmp left by a crashed
        // write is dead data; the next save simply overwrites it.
        let settings = match fs::read_to_string(&path) {
            Ok(text) => settings_from_disk(&text),
            Err(_) => Settings::default(),
        };
        Self { path, tmp_path, cache: RwLock::new(settings) }
    }

    pub fn get(&self) -> Settings {
        self.read_cache().clone()
    }

    pub fn view(&self) -> SettingsView {
        self.read_cache().view()
    }

    /// Apply a partial update and persist it. Key fields are write-only across
    /// the UI boundary: `None` leaves the stored key untouched, an
    /// empty-after-trim value clears it, anything else replaces it trimmed.
    /// The returned view never contains key material.
    pub fn apply_patch(&self, patch: SettingsPatch) -> Result<SettingsView, AppError> {
        let mut cache = self.write_cache();
        let mut next = cache.clone();

        if let Some(v) = patch.resume {
            // Verbatim, NOT trimmed: profile formatting belongs to the user.
            next.resume = cap_chars(&v, MAX_PROFILE_CHARS);
        }
        if let Some(v) = patch.job_description {
            next.job_description = cap_chars(&v, MAX_PROFILE_CHARS);
        }
        if let Some(v) = patch.always_on_top {
            next.always_on_top = v;
        }
        if let Some(v) = patch.llm_provider {
            next.llm_provider = LlmProviderKind::parse_or_default(v.trim());
        }
        if let Some(v) = patch.answer_style {
            next.answer_style = AnswerStyle::parse_or_default(v.trim());
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
        // and memory still matches what will load next launch.
        self.persist(&next)
            .map_err(|e| AppError::internal(format!("Could not save settings: {e}")))?;
        *cache = next;
        Ok(cache.view())
    }

    /// Persist window geometry. Swallows every failure: this is cosmetic data
    /// written during shutdown, and throwing there would lose the real close
    /// path.
    pub fn save_window_bounds(&self, b: RawBounds) {
        let mut cache = self.write_cache();
        let mut next = cache.clone();
        next.window_bounds = Some(b);
        // Same disk-then-cache discipline as apply_patch, minus the error —
        // on failure memory keeps matching disk and the app closes normally.
        if self.persist(&next).is_ok() {
            *cache = next;
        }
    }

    pub fn window_bounds(&self) -> Option<RawBounds> {
        self.read_cache().window_bounds
    }

    fn persist(&self, settings: &Settings) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(&to_disk_json(settings))
            .map_err(std::io::Error::other)?;
        // Write and flush the temp file completely before the rename, so the
        // real file only ever transitions between two complete states — a
        // half-written settings.json reads as "defaults" and silently destroys
        // every setting including the keys.
        let mut file = fs::File::create(&self.tmp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&self.tmp_path, &self.path)
    }

    // A poisoned lock means a writer panicked, but writers only ever store a
    // fully-assembled Settings, so the data behind the lock is always
    // coherent; recovering beats poisoning every settings read for the rest of
    // the session.
    fn read_cache(&self) -> RwLockReadGuard<'_, Settings> {
        self.cache.read().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_cache(&self) -> RwLockWriteGuard<'_, Settings> {
        self.cache.write().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Truncate by characters, not bytes — a byte cut can split a UTF-8 sequence
/// and turn a resume into invalid data on the next load.
fn cap_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
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
/// the answer must always be "only this value".
fn settings_from_disk(text: &str) -> Settings {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Settings::default();
    };
    let Some(obj) = value.as_object() else {
        return Settings::default();
    };
    Settings {
        resume: profile_field(obj.get("resume")),
        job_description: profile_field(obj.get("jobDescription")),
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
    }
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

fn to_disk_json(s: &Settings) -> Value {
    let mut m = Map::new();
    m.insert("resume".into(), Value::String(s.resume.clone()));
    m.insert("jobDescription".into(), Value::String(s.job_description.clone()));
    m.insert("alwaysOnTop".into(), Value::Bool(s.always_on_top));
    m.insert("llmProvider".into(), Value::String(s.llm_provider.as_str().to_string()));
    m.insert("answerStyle".into(), Value::String(s.answer_style.as_str().to_string()));
    m.insert("hotkey".into(), Value::String(s.hotkey.clone()));
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

    /// A fully-populated, fully-valid file — the assets the corruption tests
    /// must prove survive.
    fn good_value() -> Value {
        json!({
            "resume": "Resume text",
            "jobDescription": "JD text",
            "alwaysOnTop": false,
            "llmProvider": "groq",
            "answerStyle": "brief",
            "hotkey": "Ctrl+K",
            "deepgramKey": plain("dg-key"),
            "anthropicKey": plain("ant-key"),
            "groqKey": plain("gq-key"),
            "windowBounds": {"x": 10.0, "y": 20.0, "width": 500.0, "height": 700.0}
        })
    }

    fn good_settings() -> Settings {
        Settings {
            resume: "Resume text".into(),
            job_description: "JD text".into(),
            always_on_top: false,
            llm_provider: LlmProviderKind::Groq,
            answer_style: AnswerStyle::Brief,
            hotkey: "Ctrl+K".into(),
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
                resume: Some("My resume".into()),
                job_description: Some("The JD".into()),
                always_on_top: Some(false),
                llm_provider: Some("groq".into()),
                answer_style: Some("detailed".into()),
                hotkey: Some("Alt+Q".into()),
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
        assert_eq!(reloaded.resume, "My resume");
        assert_eq!(reloaded.anthropic_key.as_deref(), Some("ant-1"));
        assert_eq!(
            reloaded.window_bounds,
            Some(RawBounds { x: 1.0, y: 2.0, width: 460.0, height: 700.0 })
        );
    }

    #[test]
    fn unparseable_json_loads_as_defaults() {
        let dir = tempdir().unwrap();
        write_settings(dir.path(), "{ this is not json");
        assert_eq!(load(dir.path()), Settings::default());
    }

    #[test]
    fn non_object_json_loads_as_defaults() {
        for bad in ["[1,2,3]", "\"hello\"", "42", "true", "null"] {
            let dir = tempdir().unwrap();
            write_settings(dir.path(), bad);
            assert_eq!(load(dir.path()), Settings::default(), "bad: {bad}");
        }
    }

    #[test]
    fn each_corrupt_field_falls_back_alone() {
        // The core promise of per-field validation: corrupting any single
        // value defaults that value and nothing else. Whole-struct equality
        // proves both halves at once.
        #[allow(clippy::type_complexity)]
        let cases: [(&str, Value, fn(&mut Settings)); 8] = [
            ("resume", json!(42), |s| s.resume.clear()),
            ("jobDescription", json!({"a": 1}), |s| s.job_description.clear()),
            ("alwaysOnTop", json!("yes"), |s| s.always_on_top = true),
            ("llmProvider", json!(7), |s| s.llm_provider = LlmProviderKind::Anthropic),
            ("answerStyle", json!(["brief"]), |s| s.answer_style = AnswerStyle::Balanced),
            ("hotkey", json!(false), |s| s.hotkey = DEFAULT_HOTKEY.to_string()),
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
        assert_eq!(got.resume, "Resume text");
        assert_eq!(got.deepgram_key.as_deref(), Some("dg-key"));
        assert_eq!(got.anthropic_key.as_deref(), Some("ant-key"));
        assert_eq!(got.groq_key.as_deref(), Some("gq-key"));
    }

    #[test]
    fn over_length_resume_is_truncated_on_load_and_on_patch() {
        let long = "a".repeat(MAX_PROFILE_CHARS + 50);

        let dir = tempdir().unwrap();
        write_settings(dir.path(), &json!({ "resume": long }).to_string());
        assert_eq!(load(dir.path()).resume.chars().count(), MAX_PROFILE_CHARS);

        let dir2 = tempdir().unwrap();
        let store = SettingsStore::load_from(dir2.path());
        store.apply_patch(SettingsPatch { resume: Some(long), ..Default::default() }).unwrap();
        assert_eq!(store.get().resume.chars().count(), MAX_PROFILE_CHARS);
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        // A multi-byte cap cut at a byte boundary would poison the file for
        // every later load.
        let long = "é".repeat(MAX_PROFILE_CHARS + 10);
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store.apply_patch(SettingsPatch { resume: Some(long), ..Default::default() }).unwrap();
        let saved = store.get().resume;
        assert_eq!(saved.chars().count(), MAX_PROFILE_CHARS);
        assert!(saved.chars().all(|c| c == 'é'));
        assert_eq!(load(dir.path()).resume, saved);
    }

    #[test]
    fn resume_is_stored_verbatim_never_trimmed() {
        // Profile formatting belongs to the user; leading/trailing whitespace
        // survives the full round trip.
        let text = "  \n  My resume, indented on purpose  \n\n";
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store.apply_patch(SettingsPatch { resume: Some(text.into()), ..Default::default() }).unwrap();
        assert_eq!(store.get().resume, text);
        assert_eq!(load(dir.path()).resume, text);
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
        store.apply_patch(SettingsPatch { resume: Some("r".into()), ..Default::default() }).unwrap();
        assert_eq!(store.get().deepgram_key.as_deref(), Some("dg-1"));
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
        assert_eq!(got.resume, "Resume text");
        assert_eq!(got.anthropic_key.as_deref(), Some("ant-key"));
    }

    #[test]
    fn successful_save_leaves_no_tmp_file() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        store.apply_patch(SettingsPatch { resume: Some("r".into()), ..Default::default() }).unwrap();
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
        store.apply_patch(SettingsPatch { resume: Some("updated".into()), ..Default::default() }).unwrap();
        assert_eq!(load(dir.path()).resume, "updated");
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
            assert_eq!(got.resume, "Resume text", "bad bounds: {bad}");
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
        let err = store
            .apply_patch(SettingsPatch { resume: Some("never lands".into()), ..Default::default() })
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::Internal);
        // The cache was not updated, so the UI cannot show settings that will
        // silently vanish on restart.
        assert_eq!(store.get().resume, "");
    }
}

//! Settings, secrets and window-geometry persistence (§8).

pub mod bounds;
pub mod effects;
pub mod secrets;
pub mod settings;

use serde::{Deserialize, Serialize};

use crate::llm::{AnswerStyle, CallType, LlmProviderKind, Profile};

pub use bounds::{
    dock_preset_size, dock_top_center, sanitize_bounds, RawBounds, SanitizedBounds, WorkArea,
};
pub use effects::{DesiredOsState, EffectReconciler, OsEffects, ReconcileReport};
pub use settings::{normalize_profiles, SettingsStore};

/// Resume / job description cap, per profile. Generous: a long CV plus a long
/// JD is normal.
pub const MAX_PROFILE_CHARS: usize = 200_000;
pub const MAX_HOTKEY_CHARS: usize = 100;
pub const DEFAULT_HOTKEY: &str = "Ctrl+Shift+Space";

/// Call profiles (§8). Eight is plenty for concurrent searches and keeps the
/// main-view chip row readable at 460 px.
pub const MAX_PROFILES: usize = 8;
pub const MAX_PROFILE_NAME_CHARS: usize = 60;
/// Ids are `[A-Za-z0-9_-]{1,40}`: a UUID fits, and nothing in an id ever
/// needs escaping in JSON, CSS or a DOM id. Anything else is repaired.
pub const MAX_PROFILE_ID_CHARS: usize = 40;
pub const MAX_FOCUS_CHARS: usize = 2_000;
pub const MAX_EXTRA_INSTRUCTIONS_CHARS: usize = 2_000;
pub const DEFAULT_PROFILE_ID: &str = "default";
pub const DEFAULT_PROFILE_NAME: &str = "Default";
/// A profile saved with a blank name gets this — a chip cannot show "".
pub const UNNAMED_PROFILE_NAME: &str = "Untitled";

/// One call profile (§8): a self-contained grounding bundle. Exactly one is
/// active; the prompt's cached prefix is built from the active one, so a
/// switch is a deliberate between-calls cache write, never a per-question one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallProfile {
    pub id: String,
    pub name: String,
    pub call_type: CallType,
    /// Stored verbatim, NOT trimmed (§8) — same rule as the v3 resume.
    pub resume: String,
    /// The job description (interview) or the call context (other types).
    pub job_description: String,
    pub focus: String,
    pub extra_instructions: String,
}

impl CallProfile {
    pub fn empty(id: &str, name: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            call_type: CallType::Interview,
            resume: String::new(),
            job_description: String::new(),
            focus: String::new(),
            extra_instructions: String::new(),
        }
    }

    /// Borrow into the prompt builder's view — the 200 KB resume is never
    /// cloned on the way to a prompt.
    pub fn as_prompt(&self) -> Profile<'_> {
        Profile {
            call_type: self.call_type,
            resume: &self.resume,
            job_description: &self.job_description,
            focus: &self.focus,
            extra_instructions: &self.extra_instructions,
        }
    }
}

impl From<CallProfilePatch> for CallProfile {
    fn from(p: CallProfilePatch) -> Self {
        Self {
            id: p.id,
            name: p.name,
            // The lenient wire form carries a String so one bad callType
            // never fails the whole patch; unknown reads as interview (§8).
            call_type: CallType::parse_or_default(p.call_type.trim()),
            resume: p.resume,
            job_description: p.job_description,
            focus: p.focus,
            extra_instructions: p.extra_instructions,
        }
    }
}

/// `String::new()` is const, so this needs no lazy init: `active_profile`
/// can never panic, even on a hand-built `Settings` with an empty vec.
static EMPTY_PROFILE: CallProfile = CallProfile {
    id: String::new(),
    name: String::new(),
    call_type: CallType::Interview,
    resume: String::new(),
    job_description: String::new(),
    focus: String::new(),
    extra_instructions: String::new(),
};

/// Where the window goes on launch (§8/§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LaunchPlacement {
    /// Restore the saved bounds through the sanitizer — the v3 behaviour.
    Remembered,
    /// Re-dock to the top-centre of the current display every launch, keeping
    /// the saved size, so the answer sits directly under the webcam.
    #[default]
    Camera,
}

impl LaunchPlacement {
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchPlacement::Remembered => "remembered",
            LaunchPlacement::Camera => "camera",
        }
    }

    /// The settings file is user-writable; an unknown value falls back to the
    /// default rather than failing the whole load (§8).
    pub fn parse_or_default(raw: &str) -> Self {
        match raw {
            "remembered" => LaunchPlacement::Remembered,
            _ => LaunchPlacement::Camera,
        }
    }
}

/// How the answer panel scrolls while a stream lands (§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamFollow {
    /// Stick to the newest text, but only if the reader was already at the
    /// bottom — the pinned v3 behaviour.
    #[default]
    Tail,
    /// Stay parked at the opening sentence: teleprompter pacing.
    Top,
}

impl StreamFollow {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamFollow::Tail => "tail",
            StreamFollow::Top => "top",
        }
    }

    /// Unknown falls back to the default (§8).
    pub fn parse_or_default(raw: &str) -> Self {
        match raw {
            "top" => StreamFollow::Top,
            _ => StreamFollow::Tail,
        }
    }
}

/// The full in-memory settings, including decrypted key material.
///
/// This type never crosses the IPC boundary — `SettingsView` does. Keeping the
/// two distinct is what makes it structurally impossible to leak a key to the
/// webview (§8).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Invariant (enforced by `settings::normalize_profiles`, the only
    /// writer): never empty, at most MAX_PROFILES, ids valid and unique, and
    /// `active_profile_id` names one of them.
    pub profiles: Vec<CallProfile>,
    pub active_profile_id: String,
    pub always_on_top: bool,
    pub llm_provider: LlmProviderKind,
    pub answer_style: AnswerStyle,
    /// Accelerator string. Empty means "shortcut disabled" and must never
    /// spring back to the default.
    pub hotkey: String,
    pub launch_placement: LaunchPlacement,
    pub stream_follow: StreamFollow,
    pub deepgram_key: Option<String>,
    pub anthropic_key: Option<String>,
    pub groq_key: Option<String>,
    pub window_bounds: Option<RawBounds>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            profiles: vec![CallProfile::empty(DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME)],
            active_profile_id: DEFAULT_PROFILE_ID.to_string(),
            always_on_top: true,
            llm_provider: LlmProviderKind::default(),
            answer_style: AnswerStyle::default(),
            hotkey: DEFAULT_HOTKEY.to_string(),
            launch_placement: LaunchPlacement::default(),
            stream_follow: StreamFollow::default(),
            deepgram_key: None,
            anthropic_key: None,
            groq_key: None,
            window_bounds: None,
        }
    }
}

impl Settings {
    /// The key for whichever provider is currently selected.
    pub fn active_llm_key(&self) -> Option<&str> {
        match self.llm_provider {
            LlmProviderKind::Anthropic => self.anthropic_key.as_deref(),
            LlmProviderKind::Groq => self.groq_key.as_deref(),
            LlmProviderKind::Local => None,
        }
    }

    /// The profile the next answer is grounded in. By id, else the first
    /// profile, else an empty one — the fallbacks exist so a hand-built or
    /// mid-migration `Settings` can never panic on the answer path; the
    /// store's invariant makes the first arm the only one that runs in
    /// practice.
    pub fn active_profile(&self) -> &CallProfile {
        self.profiles
            .iter()
            .find(|p| p.id == self.active_profile_id)
            .or_else(|| self.profiles.first())
            .unwrap_or(&EMPTY_PROFILE)
    }

    /// The view of a free-standing `Settings`, at revision 0 with no storage
    /// warning. The store's own `view()` stamps the committed revision and any
    /// load problem instead (R5, ADR 016); this form is for tests and callers
    /// that hold a `Settings` without a store.
    pub fn view(&self) -> SettingsView {
        self.view_at(0, None)
    }

    pub fn view_at(&self, revision: u64, storage_warning: Option<String>) -> SettingsView {
        SettingsView {
            revision,
            storage_warning,
            profiles: self.profiles.clone(),
            active_profile_id: self.active_profile_id.clone(),
            always_on_top: self.always_on_top,
            llm_provider: self.llm_provider,
            answer_style: self.answer_style,
            hotkey: self.hotkey.clone(),
            launch_placement: self.launch_placement,
            stream_follow: self.stream_follow,
            has_deepgram_key: self.deepgram_key.is_some(),
            has_anthropic_key: self.anthropic_key.is_some(),
            has_groq_key: self.groq_key.is_some(),
        }
    }
}

/// What the frontend is allowed to see. Key material is reduced to booleans —
/// the UI only ever needs to know whether to show "saved — type to replace".
/// Mirrors `SettingsView` in `src/types.ts` field for field (§4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    /// The committed editable-settings revision (R5, ADR 016). Bumped once per
    /// successful `apply_patch`, never by a failed write and never by a
    /// window-geometry save, so moving the window cannot invalidate an open
    /// form. Starts at 1 each launch; it orders views within one run only.
    pub revision: u64,
    /// Set when the settings file could not be used at startup: the damaged
    /// file was quarantined, or it could not be read or preserved and saving
    /// is blocked to protect it. `None` in the normal case.
    pub storage_warning: Option<String>,
    /// Never empty, at most MAX_PROFILES.
    pub profiles: Vec<CallProfile>,
    /// Always names an entry of `profiles`.
    pub active_profile_id: String,
    pub always_on_top: bool,
    pub llm_provider: LlmProviderKind,
    pub answer_style: AnswerStyle,
    pub hotkey: String,
    pub launch_placement: LaunchPlacement,
    pub stream_follow: StreamFollow,
    pub has_deepgram_key: bool,
    pub has_anthropic_key: bool,
    pub has_groq_key: bool,
}

/// One profile as the UI sends it. Every field defaults so a partial object
/// never fails the whole patch (§8's per-field fallback, applied at the IPC
/// edge); `call_type` is a String for the same reason and is parsed on apply.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CallProfilePatch {
    pub id: String,
    pub name: String,
    pub call_type: String,
    pub resume: String,
    pub job_description: String,
    pub focus: String,
    pub extra_instructions: String,
}

/// A partial update. Every field is optional; omitted fields are untouched.
///
/// Key fields carry three-way meaning, which is why they are `Option<String>`
/// and not `String`:
/// * `None` -> leave the stored key exactly as it is (the UI omits the field
///   unless the user actually typed into it),
/// * `Some("")` -> clear the stored key,
/// * `Some(value)` -> replace it.
///
/// The enum fields are typed: an unknown wire value fails deserialization,
/// the invoke rejects, and the UI folds that into an `internal` error (§4).
/// `parse_or_default` is for the untrusted settings FILE only — the UI is
/// compiled against the same enum and has no business sending anything else.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsPatch {
    /// The revision the caller's form was seeded from (R5, ADR 016). When
    /// present, the store commits only if it still equals the committed
    /// revision, checked under the same lock as the write, and otherwise
    /// fails with `SettingsConflict`. The full Settings form always sends it;
    /// the single-field chip patches omit it, because they change one
    /// distinct field and merge safely with anything committed meanwhile.
    pub expected_revision: Option<u64>,
    /// Whole-array replace: the Settings form owns the draft and sends all of
    /// it. `None` leaves the profiles alone.
    pub profiles: Option<Vec<CallProfilePatch>>,
    /// Sent ALONE by the main-view switcher — a switch never rewrites profile
    /// text. An id that names no profile leaves the active one unchanged.
    pub active_profile_id: Option<String>,
    pub always_on_top: Option<bool>,
    pub llm_provider: Option<LlmProviderKind>,
    pub answer_style: Option<AnswerStyle>,
    pub hotkey: Option<String>,
    pub launch_placement: Option<LaunchPlacement>,
    pub stream_follow: Option<StreamFollow>,
    pub deepgram_key: Option<String>,
    pub anthropic_key: Option<String>,
    pub groq_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, resume: &str) -> CallProfile {
        CallProfile { resume: resume.into(), ..CallProfile::empty(id, id) }
    }

    #[test]
    fn active_profile_resolves_by_id_and_falls_back_to_first() {
        let mut s = Settings {
            profiles: vec![profile("a", "RA"), profile("b", "RB")],
            active_profile_id: "b".into(),
            ..Default::default()
        };
        assert_eq!(s.active_profile().id, "b");

        // A hand-built Settings that breaks the invariant must still answer
        // with SOMETHING grounded rather than panic on the answer path.
        s.active_profile_id = "zzz".into();
        assert_eq!(s.active_profile().id, "a");

        s.profiles.clear();
        let empty = s.active_profile();
        assert_eq!(empty, &EMPTY_PROFILE);
        assert_eq!((empty.id.as_str(), empty.resume.as_str()), ("", ""));
        assert_eq!(empty.call_type, CallType::Interview);
    }

    #[test]
    fn as_prompt_borrows_every_field() {
        let p = CallProfile {
            id: "a".into(),
            name: "A".into(),
            call_type: CallType::Support,
            resume: "R".into(),
            job_description: "J".into(),
            focus: "F".into(),
            extra_instructions: "E".into(),
        };
        let view = p.as_prompt();
        assert_eq!(view.call_type, CallType::Support);
        assert_eq!(view.resume, "R");
        assert_eq!(view.job_description, "J");
        assert_eq!(view.focus, "F");
        assert_eq!(view.extra_instructions, "E");
        // Borrowed, not copied: the same bytes the profile owns.
        assert!(std::ptr::eq(view.resume, p.resume.as_str()));
        assert!(std::ptr::eq(view.job_description, p.job_description.as_str()));
    }

    #[test]
    fn default_settings_hold_one_empty_default_profile_docked_and_tailing() {
        let s = Settings::default();
        assert_eq!(s.profiles, vec![CallProfile::empty(DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME)]);
        assert_eq!(s.active_profile_id, DEFAULT_PROFILE_ID);
        assert_eq!(s.active_profile().name, "Default");
        assert_eq!(s.launch_placement, LaunchPlacement::Camera);
        assert_eq!(s.stream_follow, StreamFollow::Tail);
    }

    #[test]
    fn launch_placement_and_stream_follow_parse_or_default() {
        assert_eq!(LaunchPlacement::parse_or_default("remembered"), LaunchPlacement::Remembered);
        assert_eq!(LaunchPlacement::parse_or_default("camera"), LaunchPlacement::Camera);
        assert_eq!(LaunchPlacement::parse_or_default(""), LaunchPlacement::Camera);
        assert_eq!(LaunchPlacement::parse_or_default("sideways"), LaunchPlacement::Camera);
        assert_eq!(LaunchPlacement::parse_or_default("Remembered"), LaunchPlacement::Camera);

        assert_eq!(StreamFollow::parse_or_default("top"), StreamFollow::Top);
        assert_eq!(StreamFollow::parse_or_default("tail"), StreamFollow::Tail);
        assert_eq!(StreamFollow::parse_or_default(""), StreamFollow::Tail);
        assert_eq!(StreamFollow::parse_or_default("middle"), StreamFollow::Tail);

        for v in [LaunchPlacement::Remembered, LaunchPlacement::Camera] {
            assert_eq!(LaunchPlacement::parse_or_default(v.as_str()), v);
            assert_eq!(serde_json::to_string(&v).unwrap(), format!("\"{}\"", v.as_str()));
        }
        for v in [StreamFollow::Tail, StreamFollow::Top] {
            assert_eq!(StreamFollow::parse_or_default(v.as_str()), v);
            assert_eq!(serde_json::to_string(&v).unwrap(), format!("\"{}\"", v.as_str()));
        }
    }

    #[test]
    fn typed_enum_patch_fields_reject_unknown_wire_values() {
        // §4: the wire is typed. A value the TS enum cannot produce fails the
        // invoke instead of being silently coerced to a default the user did
        // not choose.
        for bad in [
            r#"{"llmProvider":"bogus"}"#,
            r#"{"answerStyle":"verbose"}"#,
            r#"{"launchPlacement":"sideways"}"#,
            r#"{"streamFollow":"middle"}"#,
            r#"{"llmProvider":7}"#,
        ] {
            assert!(serde_json::from_str::<SettingsPatch>(bad).is_err(), "accepted: {bad}");
        }
        let ok: SettingsPatch = serde_json::from_str(
            r#"{"llmProvider":"local","answerStyle":"brief","launchPlacement":"remembered","streamFollow":"top"}"#,
        )
        .unwrap();
        assert_eq!(ok.llm_provider, Some(LlmProviderKind::Local));
        assert_eq!(ok.answer_style, Some(AnswerStyle::Brief));
        assert_eq!(ok.launch_placement, Some(LaunchPlacement::Remembered));
        assert_eq!(ok.stream_follow, Some(StreamFollow::Top));
    }

    #[test]
    fn a_partial_profile_object_never_fails_the_patch() {
        // The lenient profile form: missing fields default, an unknown
        // callType string reads as interview, and a bare switch carries only
        // the active id.
        let patch: SettingsPatch =
            serde_json::from_str(r#"{"profiles":[{"id":"a","callType":"bogus"},{"name":"B"}]}"#).unwrap();
        let list = patch.profiles.unwrap();
        assert_eq!(list.len(), 2);
        let a = CallProfile::from(list[0].clone());
        assert_eq!((a.id.as_str(), a.name.as_str(), a.call_type), ("a", "", CallType::Interview));
        assert_eq!(a.resume, "");
        let b = CallProfile::from(list[1].clone());
        assert_eq!((b.id.as_str(), b.name.as_str()), ("", "B"));

        let switch: SettingsPatch = serde_json::from_str(r#"{"activeProfileId":"b"}"#).unwrap();
        assert!(switch.profiles.is_none());
        assert_eq!(switch.active_profile_id.as_deref(), Some("b"));

        let typed: SettingsPatch =
            serde_json::from_str(r#"{"profiles":[{"id":"s","callType":"sales"}]}"#).unwrap();
        assert_eq!(CallProfile::from(typed.profiles.unwrap().remove(0)).call_type, CallType::Sales);
    }

    #[test]
    fn settings_view_serializes_the_wire_shape() {
        // Pinned against `src/types.ts` SettingsView / CallProfile: camelCase
        // keys, lowercase enums, booleans for keys.
        let s = Settings {
            profiles: vec![CallProfile {
                id: "a".into(),
                name: "A".into(),
                call_type: CallType::Meeting,
                resume: "R".into(),
                job_description: "J".into(),
                focus: "F".into(),
                extra_instructions: "E".into(),
            }],
            active_profile_id: "a".into(),
            launch_placement: LaunchPlacement::Remembered,
            stream_follow: StreamFollow::Top,
            deepgram_key: Some("k".into()),
            ..Default::default()
        };
        let wire: serde_json::Value = serde_json::to_value(s.view()).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "revision": 0,
                "storageWarning": null,
                "profiles": [{
                    "id": "a", "name": "A", "callType": "meeting", "resume": "R",
                    "jobDescription": "J", "focus": "F", "extraInstructions": "E"
                }],
                "activeProfileId": "a",
                "alwaysOnTop": true,
                "llmProvider": "anthropic",
                "answerStyle": "balanced",
                "hotkey": DEFAULT_HOTKEY,
                "launchPlacement": "remembered",
                "streamFollow": "top",
                "hasDeepgramKey": true,
                "hasAnthropicKey": false,
                "hasGroqKey": false
            })
        );
    }

    #[test]
    fn expected_revision_travels_on_the_patch_wire_and_is_optional() {
        // The full form sends the revision it was seeded from; the chips omit
        // it (R5, ADR 016). A wrong-typed value fails the invoke like any
        // other typed field.
        let form: SettingsPatch = serde_json::from_str(r#"{"expectedRevision":4,"hotkey":"Alt+Q"}"#).unwrap();
        assert_eq!(form.expected_revision, Some(4));
        let chip: SettingsPatch = serde_json::from_str(r#"{"answerStyle":"brief"}"#).unwrap();
        assert_eq!(chip.expected_revision, None);
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"expectedRevision":"4"}"#).is_err());
    }
}

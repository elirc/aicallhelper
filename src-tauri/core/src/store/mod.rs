//! Settings, secrets and window-geometry persistence (§8).

pub mod bounds;
pub mod secrets;
pub mod settings;

use serde::{Deserialize, Serialize};

use crate::llm::{AnswerStyle, LlmProviderKind};

pub use bounds::{sanitize_bounds, RawBounds, SanitizedBounds, WorkArea};
pub use settings::SettingsStore;

/// Resume / job description cap. Generous: a long CV plus a long JD is normal.
pub const MAX_PROFILE_CHARS: usize = 200_000;
pub const MAX_HOTKEY_CHARS: usize = 100;
pub const DEFAULT_HOTKEY: &str = "Ctrl+Shift+Space";

/// The full in-memory settings, including decrypted key material.
///
/// This type never crosses the IPC boundary — `SettingsView` does. Keeping the
/// two distinct is what makes it structurally impossible to leak a key to the
/// webview (§8).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Stored verbatim, NOT trimmed: profile formatting belongs to the user.
    pub resume: String,
    pub job_description: String,
    pub always_on_top: bool,
    pub llm_provider: LlmProviderKind,
    pub answer_style: AnswerStyle,
    /// Accelerator string. Empty means "shortcut disabled" and must never
    /// spring back to the default.
    pub hotkey: String,
    pub deepgram_key: Option<String>,
    pub anthropic_key: Option<String>,
    pub groq_key: Option<String>,
    pub window_bounds: Option<RawBounds>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            resume: String::new(),
            job_description: String::new(),
            always_on_top: true,
            llm_provider: LlmProviderKind::default(),
            answer_style: AnswerStyle::default(),
            hotkey: DEFAULT_HOTKEY.to_string(),
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
        }
    }

    pub fn view(&self) -> SettingsView {
        SettingsView {
            resume: self.resume.clone(),
            job_description: self.job_description.clone(),
            always_on_top: self.always_on_top,
            llm_provider: self.llm_provider,
            answer_style: self.answer_style,
            hotkey: self.hotkey.clone(),
            has_deepgram_key: self.deepgram_key.is_some(),
            has_anthropic_key: self.anthropic_key.is_some(),
            has_groq_key: self.groq_key.is_some(),
        }
    }
}

/// What the frontend is allowed to see. Key material is reduced to booleans —
/// the UI only ever needs to know whether to show "saved — type to replace".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub resume: String,
    pub job_description: String,
    pub always_on_top: bool,
    pub llm_provider: LlmProviderKind,
    pub answer_style: AnswerStyle,
    pub hotkey: String,
    pub has_deepgram_key: bool,
    pub has_anthropic_key: bool,
    pub has_groq_key: bool,
}

/// A partial update. Every field is optional; omitted fields are untouched.
///
/// Key fields carry three-way meaning, which is why they are `Option<String>`
/// and not `String`:
/// * `None` -> leave the stored key exactly as it is (the UI omits the field
///   unless the user actually typed into it),
/// * `Some("")` -> clear the stored key,
/// * `Some(value)` -> replace it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsPatch {
    pub resume: Option<String>,
    pub job_description: Option<String>,
    pub always_on_top: Option<bool>,
    pub llm_provider: Option<String>,
    pub answer_style: Option<String>,
    pub hotkey: Option<String>,
    pub deepgram_key: Option<String>,
    pub anthropic_key: Option<String>,
    pub groq_key: Option<String>,
}

//! The closed set of error codes the UI keys its behavior off (§4).
//!
//! Keeping this a closed enum rather than free-form strings is deliberate: the
//! frontend branches on `aborted` (never shown) and on the key-missing codes
//! (which nudge toward Settings). A typo'd string code would silently degrade
//! into "show the raw message", which is how v2 leaked exception text at users.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NoSttKey,
    NoLlmKey,
    SttConnect,
    SttError,
    SttTimeout,
    NoSpeech,
    LlmAuth,
    LlmHttp,
    LlmRateLimit,
    LlmFirstTokenTimeout,
    LlmTimeout,
    Aborted,
    Internal,
    /// A full Settings form was seeded from an older settings revision than
    /// the one now committed (R5, ADR 016). Distinct so the UI can keep the
    /// draft and offer a reload instead of showing a generic failure.
    SettingsConflict,
}

impl ErrorCode {
    /// The wire string. Used by tests and by the Tauri serialization layer.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::NoSttKey => "no_stt_key",
            ErrorCode::NoLlmKey => "no_llm_key",
            ErrorCode::SttConnect => "stt_connect",
            ErrorCode::SttError => "stt_error",
            ErrorCode::SttTimeout => "stt_timeout",
            ErrorCode::NoSpeech => "no_speech",
            ErrorCode::LlmAuth => "llm_auth",
            ErrorCode::LlmHttp => "llm_http",
            ErrorCode::LlmRateLimit => "llm_rate_limit",
            ErrorCode::LlmFirstTokenTimeout => "llm_first_token_timeout",
            ErrorCode::LlmTimeout => "llm_timeout",
            ErrorCode::Aborted => "aborted",
            ErrorCode::Internal => "internal",
            ErrorCode::SettingsConflict => "settings_conflict",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A user-facing error. `message` is written for the person on the call, not
/// for a log reader — it says what to do next wherever that is knowable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    /// The user superseded or cancelled this work. The UI never renders this;
    /// it exists so the pipeline has a non-error way to unwind.
    pub fn aborted() -> Self {
        Self::new(ErrorCode::Aborted, "Cancelled.")
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn is_aborted(&self) -> bool {
        self.code == ErrorCode::Aborted
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

pub type AppResult<T> = Result<T, AppError>;

/// Canonical, actionable messages for the "you have not set this up yet" cases.
/// They name the gear icon because that is the only affordance that reaches
/// Settings (§9).
pub const MSG_NO_STT_KEY: &str =
    "No Deepgram API key set, so speech can't be transcribed. Open Settings (gear icon) and add it.";
pub const MSG_NO_SPEECH: &str =
    "No speech detected in the recording. Make sure call audio is playing.";

pub fn no_llm_key_message(provider_label: &str) -> String {
    format!("No {provider_label} API key set, so answers can't be generated. Open Settings (gear icon) and add it.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_serialize_to_the_documented_wire_strings() {
        // The frontend switches on these literals; a rename here is a silent
        // behavior change over the IPC boundary, so pin every one.
        let all = [
            (ErrorCode::NoSttKey, "no_stt_key"),
            (ErrorCode::NoLlmKey, "no_llm_key"),
            (ErrorCode::SttConnect, "stt_connect"),
            (ErrorCode::SttError, "stt_error"),
            (ErrorCode::SttTimeout, "stt_timeout"),
            (ErrorCode::NoSpeech, "no_speech"),
            (ErrorCode::LlmAuth, "llm_auth"),
            (ErrorCode::LlmHttp, "llm_http"),
            (ErrorCode::LlmRateLimit, "llm_rate_limit"),
            (ErrorCode::LlmFirstTokenTimeout, "llm_first_token_timeout"),
            (ErrorCode::LlmTimeout, "llm_timeout"),
            (ErrorCode::Aborted, "aborted"),
            (ErrorCode::Internal, "internal"),
            (ErrorCode::SettingsConflict, "settings_conflict"),
        ];
        for (code, wire) in all {
            assert_eq!(code.as_str(), wire);
            assert_eq!(serde_json::to_string(&code).unwrap(), format!("\"{wire}\""));
        }
    }

    #[test]
    fn aborted_is_recognisable_so_the_ui_can_stay_silent() {
        assert!(AppError::aborted().is_aborted());
        assert!(!AppError::internal("boom").is_aborted());
    }
}

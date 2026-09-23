//! Answer generation: prompt construction, SSE decoding, the two providers,
//! the shared retry policy, and origin pre-warming.

pub mod anthropic;
pub mod groq;
pub mod http;
pub mod local;
pub mod prompt;
pub mod retry;
pub mod sse;
pub mod warm;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::error::{AppError, AppResult, ErrorCode};
use crate::session::AnswerLimits;

pub use prompt::{
    build_system_prompt, build_user_message, local_prompt_budget, request_input_bytes,
    AnswerStyle, BudgetStatus, CallType, LocalPromptBudget, Profile, SystemPrompt,
    QUESTION_RESERVE_BYTES,
};
pub use sse::{SseDecoder, SseEvent};

/// Spoken answers are short. An uncapped completion is pure tail latency (§6.2).
pub const MAX_ANSWER_TOKENS: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmProviderKind {
    #[default]
    Anthropic,
    Groq,
    Local,
}

impl LlmProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LlmProviderKind::Anthropic => "anthropic",
            LlmProviderKind::Groq => "groq",
            LlmProviderKind::Local => "local",
        }
    }

    /// The settings file is user-writable; an unknown value falls back rather
    /// than failing the whole load (§8).
    pub fn parse_or_default(raw: &str) -> Self {
        match raw {
            "groq" => LlmProviderKind::Groq,
            "local" => LlmProviderKind::Local,
            _ => LlmProviderKind::Anthropic,
        }
    }

    /// Used in "No {label} API key set…" messages.
    pub fn label(self) -> &'static str {
        match self {
            LlmProviderKind::Anthropic => "Anthropic",
            LlmProviderKind::Groq => "Groq",
            LlmProviderKind::Local => "Free local",
        }
    }

    /// Whether a session needs this provider's API key before it may start
    /// (§8). The shell's first-run rule and its "No {label} API key set…"
    /// check key off this instead of `== Local`, so a keyless provider added
    /// later cannot be refused by a check written for the cloud pair.
    pub fn needs_cloud_keys(self) -> bool {
        match self {
            LlmProviderKind::Anthropic | LlmProviderKind::Groq => true,
            LlmProviderKind::Local => false,
        }
    }

    /// Whether speech goes through Deepgram (and so needs its key) rather
    /// than the loopback speech service that ships with free local mode
    /// (§6.5). Kept separate from `needs_cloud_keys` because the two are
    /// separate keys with separate error messages.
    pub fn uses_deepgram(self) -> bool {
        match self {
            LlmProviderKind::Anthropic | LlmProviderKind::Groq => true,
            LlmProviderKind::Local => false,
        }
    }

    /// Whether requests are refused above `local::MAX_INPUT_BYTES` (R4). The
    /// pre-record and pre-ask budget checks key off this, for the same reason
    /// the key gates key off `needs_cloud_keys`.
    pub fn caps_input_bytes(self) -> bool {
        match self {
            LlmProviderKind::Anthropic | LlmProviderKind::Groq => false,
            LlmProviderKind::Local => true,
        }
    }
}

/// One answer request. Built once so a retry can resend byte-identical bytes
/// (§6.4).
#[derive(Debug, Clone)]
pub struct AnswerRequest {
    pub system: SystemPrompt,
    /// What the other person said. Filled in by the state machine once the
    /// transcript is final.
    pub transcript: String,
}

impl AnswerRequest {
    pub fn new(system: SystemPrompt) -> Self {
        Self { system, transcript: String::new() }
    }

    pub fn with_transcript(&self, transcript: impl Into<String>) -> Self {
        Self { system: self.system.clone(), transcript: transcript.into() }
    }

    pub fn user_message(&self) -> String {
        build_user_message(&self.transcript)
    }
}

/// Why generation stopped, kept apart from WHETHER the stream finished (R2).
///
/// Protocol completion is the provider's terminal event (`message_stop`,
/// `data: [DONE]`, Ollama's `done` frame) and nothing else; the stop reason is
/// metadata carried next to it. Conflating the two is how a stream that ended
/// with a `stop_reason` but no terminator — or a terminator but no text — used
/// to be reported as a finished answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished on its own terms (end of turn, stop sequence, …).
    #[default]
    Complete,
    /// The answer hit `MAX_ANSWER_TOKENS` (or the local `num_predict`). The
    /// text is kept and shown, labelled "cut short" — a capped answer is not a
    /// network failure and must not be reported as one.
    TokenLimit,
}

impl StopReason {
    /// Map a provider's raw stop string. Only the token-limit spellings are
    /// distinguished; every other reason (end_turn, stop, stop_sequence,
    /// refusal, content_filter, …) is a model-chosen end and reads as
    /// complete — the text itself says what the model decided.
    pub fn from_provider(raw: Option<&str>) -> Self {
        match raw {
            Some("max_tokens") | Some("length") => StopReason::TokenLimit,
            _ => StopReason::Complete,
        }
    }
}

/// A finished answer: the exact concatenation of the streamed deltas plus why
/// generation stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    pub stop_reason: StopReason,
}

impl Answer {
    /// The R2 rule for a stream that DID reach its terminal event: usable text
    /// is an answer; no usable text is an explicit failure, never a blank
    /// success the UI would render as the model silently saying nothing.
    pub fn from_terminal(
        provider_label: &str,
        text: String,
        stop_reason: StopReason,
    ) -> AppResult<Self> {
        if text.trim().is_empty() {
            return Err(no_answer_text(provider_label));
        }
        Ok(Self { text, stop_reason })
    }
}

/// The terminal event arrived but no answer text came with it (a ping-only or
/// role-only stream).
pub fn no_answer_text(provider_label: &str) -> AppError {
    AppError::new(
        ErrorCode::LlmHttp,
        format!("{provider_label} finished without any answer text. Try again."),
    )
}

/// The stream closed cleanly before the provider's terminal event. Whatever
/// streamed is already on screen; this error is what marks it incomplete.
pub fn ended_early(provider_label: &str) -> AppError {
    AppError::new(
        ErrorCode::LlmHttp,
        format!(
            "{provider_label} stopped sending before the answer was finished, so it is incomplete. Try again."
        ),
    )
}

/// A frame of a known protocol failed to parse. Ignoring it would silently
/// drop answer text and still label the answer finished.
pub fn malformed_frame(provider_label: &str) -> AppError {
    AppError::new(
        ErrorCode::LlmHttp,
        format!("{provider_label} sent a malformed streaming frame, so the answer may be incomplete. Try again."),
    )
}

/// A single streaming frame outgrew the decoder's cap (see `sse.rs`).
pub fn oversized_frame(provider_label: &str) -> AppError {
    AppError::new(
        ErrorCode::LlmHttp,
        format!("{provider_label} sent an oversized streaming frame; the answer was stopped. Try again."),
    )
}

/// Where answer deltas go as they stream.
pub trait LlmSink: Send + Sync + 'static {
    fn on_delta(&self, delta: String);
}

#[async_trait]
pub trait LlmProvider: Send + Sync + 'static {
    /// Stream an answer, pushing deltas to `sink`, and resolve with the
    /// complete answer.
    ///
    /// The returned text MUST equal the concatenation of every delta pushed to
    /// the sink, byte for byte — the UI builds its display from the deltas and
    /// then renders the returned answer, so any divergence shows up as text
    /// changing after it has been read.
    ///
    /// `Ok` means the provider's terminal event arrived AND there is usable
    /// text (R2, `Answer::from_terminal`); an error frame, a malformed frame or
    /// an end of stream before the terminal event is an `Err`, and the deltas
    /// already pushed stay on screen marked incomplete.
    async fn stream_answer(
        &self,
        req: &AnswerRequest,
        sink: Arc<dyn LlmSink>,
        cancel: CancellationToken,
    ) -> AppResult<Answer>;

    /// Fire-and-forget warm of this provider's origin (§6.4). Must never block,
    /// throw, or log loudly.
    fn prewarm(&self);

    fn kind(&self) -> LlmProviderKind;

    /// The answer-stage deadlines the state machine arms for this provider
    /// (§3). Cloud pacing by default — the safe assumption for anything that
    /// talks to a remote model; a provider that is legitimately slower (CPU
    /// inference) overrides it, so the machine never matches on `kind()` to
    /// learn how long to wait.
    fn answer_limits(&self) -> AnswerLimits {
        AnswerLimits::CLOUD
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_kinds_need_keys_and_deepgram_but_local_needs_neither() {
        // Local mode must start with no keys stored at all (§6.5); the cloud
        // pair must never start without theirs, or the first request fails
        // with a confusing 401 instead of the "No … API key set" error.
        for kind in [LlmProviderKind::Anthropic, LlmProviderKind::Groq] {
            assert!(kind.needs_cloud_keys(), "{kind:?} needs its provider key");
            assert!(kind.uses_deepgram(), "{kind:?} transcribes through Deepgram");
        }
        assert!(!LlmProviderKind::Local.needs_cloud_keys());
        assert!(!LlmProviderKind::Local.uses_deepgram());
    }

    /// The smallest possible provider: nothing overridden beyond the required
    /// methods, so it exercises the trait's defaults.
    struct BareProvider;

    #[async_trait]
    impl LlmProvider for BareProvider {
        async fn stream_answer(
            &self,
            _req: &AnswerRequest,
            _sink: Arc<dyn LlmSink>,
            _cancel: CancellationToken,
        ) -> AppResult<Answer> {
            Answer::from_terminal("Bare", String::new(), StopReason::Complete)
        }

        fn prewarm(&self) {}

        fn kind(&self) -> LlmProviderKind {
            LlmProviderKind::Anthropic
        }
    }

    #[test]
    fn stop_reason_distinguishes_only_the_token_limit_spellings() {
        // `max_tokens` (Anthropic), `length` (Groq, Ollama) are the capped
        // answer; every other reason is a model-chosen end. Mapping an
        // unknown future reason to "cut short" would mislabel whole answers.
        assert_eq!(StopReason::from_provider(Some("max_tokens")), StopReason::TokenLimit);
        assert_eq!(StopReason::from_provider(Some("length")), StopReason::TokenLimit);
        for raw in [Some("end_turn"), Some("stop"), Some("refusal"), Some("new_reason"), None] {
            assert_eq!(StopReason::from_provider(raw), StopReason::Complete, "{raw:?}");
        }
        assert_eq!(serde_json::to_string(&StopReason::TokenLimit).unwrap(), "\"token_limit\"");
        assert_eq!(serde_json::to_string(&StopReason::Complete).unwrap(), "\"complete\"");
    }

    #[test]
    fn a_terminal_event_without_usable_text_is_an_error_not_a_blank_answer() {
        // R2: a ping-only or role-only stream that reached its terminator used
        // to resolve Ok("") — rendered as the model silently saying nothing.
        let err = Answer::from_terminal("Groq", "  \n".into(), StopReason::Complete).unwrap_err();
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(err.message.contains("without any answer text"), "{}", err.message);
        let ok = Answer::from_terminal("Groq", "Hi".into(), StopReason::TokenLimit).unwrap();
        assert_eq!((ok.text.as_str(), ok.stop_reason), ("Hi", StopReason::TokenLimit));
    }

    #[test]
    fn answer_limits_default_to_cloud_pacing() {
        // A provider that does not override the method gets the 10 s / 60 s
        // pair. Defaulting to the LOCAL pair instead would let a wedged cloud
        // request sit for 90 s before the UI heard anything.
        assert_eq!(BareProvider.answer_limits(), AnswerLimits::CLOUD);
    }
}

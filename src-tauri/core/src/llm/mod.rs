//! Answer generation: prompt construction, SSE decoding, the two providers,
//! the shared retry policy, and origin pre-warming.

pub mod anthropic;
pub mod groq;
pub mod http;
pub mod prompt;
pub mod retry;
pub mod sse;
pub mod warm;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::error::AppResult;

pub use prompt::{
    build_system_prompt, build_user_message, AnswerStyle, Profile, SystemPrompt,
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
}

impl LlmProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LlmProviderKind::Anthropic => "anthropic",
            LlmProviderKind::Groq => "groq",
        }
    }

    /// The settings file is user-writable; an unknown value falls back rather
    /// than failing the whole load (§8).
    pub fn parse_or_default(raw: &str) -> Self {
        match raw {
            "groq" => LlmProviderKind::Groq,
            _ => LlmProviderKind::Anthropic,
        }
    }

    /// Used in "No {label} API key set…" messages.
    pub fn label(self) -> &'static str {
        match self {
            LlmProviderKind::Anthropic => "Anthropic",
            LlmProviderKind::Groq => "Groq",
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

/// Where answer deltas go as they stream.
pub trait LlmSink: Send + Sync + 'static {
    fn on_delta(&self, delta: String);
}

#[async_trait]
pub trait LlmProvider: Send + Sync + 'static {
    /// Stream an answer, pushing deltas to `sink`, and resolve with the
    /// complete answer.
    ///
    /// The returned string MUST equal the concatenation of every delta pushed to
    /// the sink, byte for byte — the UI builds its display from the deltas and
    /// then renders the returned answer, so any divergence shows up as text
    /// changing after it has been read.
    async fn stream_answer(
        &self,
        req: &AnswerRequest,
        sink: Arc<dyn LlmSink>,
        cancel: CancellationToken,
    ) -> AppResult<String>;

    /// Fire-and-forget warm of this provider's origin (§6.4). Must never block,
    /// throw, or log loudly.
    fn prewarm(&self);

    fn kind(&self) -> LlmProviderKind;
}

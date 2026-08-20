//! Session contract: the events the core emits, the metrics it measures, and
//! the traits every dependency of the state machine is injected through.
//!
//! The state machine (`machine.rs`) depends only on what is declared here, so
//! `cargo test` drives the whole pipeline with fakes — no network, no audio
//! device, no clock skew.

pub mod machine;

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::AppError;
use crate::llm::{AnswerRequest, LlmProvider};
use crate::stt::SttConnector;

/// Identifies one question/answer pipeline. Every event carries it so the
/// frontend can drop anything belonging to a session it is no longer tracking
/// (§4) — the single mechanism that makes supersession safe.
pub type SessionId = u64;

/// Latency measurements, all counted from the instant Stop was requested (§3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    /// Stop -> final transcript in hand. Exactly 0 for typed questions: there
    /// was no STT stage and billing one would be a lie.
    pub stt_finalize_ms: u64,
    /// Stop -> first answer token. Never 0 for a non-streaming provider — see
    /// `Metrics::finish`.
    pub first_token_ms: u64,
    /// Stop -> answer complete.
    pub total_ms: u64,
}

impl Metrics {
    /// Assemble the final metrics.
    ///
    /// `first_token_ms` of `None` means the provider returned a complete answer
    /// without ever streaming a delta. Reporting 0 there would render as
    /// "instant" and lie about the one number this app is judged on, so it
    /// collapses to `total_ms` instead (§3).
    pub fn finish(stt_finalize_ms: u64, first_token_ms: Option<u64>, total_ms: u64) -> Self {
        Self {
            stt_finalize_ms,
            first_token_ms: first_token_ms.unwrap_or(total_ms),
            total_ms,
        }
    }
}

/// Everything the core pushes to the frontend. Field names are camelCase on the
/// wire to match the TypeScript side (§4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionEvent {
    /// The full transcript so far — not a delta. `is_final` is true once
    /// Deepgram has committed the latest segment.
    #[serde(rename_all = "camelCase")]
    SttPartial { session_id: SessionId, text: String, is_final: bool },
    #[serde(rename_all = "camelCase")]
    LlmDelta { session_id: SessionId, delta: String },
    #[serde(rename_all = "camelCase")]
    LlmDone {
        session_id: SessionId,
        transcript: String,
        answer: String,
        metrics: Metrics,
    },
    #[serde(rename_all = "camelCase")]
    SessionError { session_id: SessionId, error: AppError },
    /// Drives the level meter, ~8/s. May be coalesced.
    #[serde(rename_all = "camelCase")]
    AudioLevel { session_id: SessionId, rms: f32 },
}

impl SessionEvent {
    pub fn session_id(&self) -> SessionId {
        match self {
            SessionEvent::SttPartial { session_id, .. }
            | SessionEvent::LlmDelta { session_id, .. }
            | SessionEvent::LlmDone { session_id, .. }
            | SessionEvent::SessionError { session_id, .. }
            | SessionEvent::AudioLevel { session_id, .. } => *session_id,
        }
    }

    /// The Tauri event name this maps to (§4).
    pub fn event_name(&self) -> &'static str {
        match self {
            SessionEvent::SttPartial { .. } => "stt:partial",
            SessionEvent::LlmDelta { .. } => "llm:delta",
            SessionEvent::LlmDone { .. } => "llm:done",
            SessionEvent::SessionError { .. } => "session:error",
            SessionEvent::AudioLevel { .. } => "audio:level",
        }
    }
}

/// Where session events go. The Tauri shell implements this by emitting to the
/// webview; tests implement it by pushing into a Vec.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: SessionEvent);
}

/// Everything the state machine needs to run a question, resolved fresh per
/// session so a settings change takes effect on the next recording.
pub struct SessionDeps {
    pub stt: Arc<dyn SttConnector>,
    pub llm: Arc<dyn LlmProvider>,
    /// Base request (model, prompt, profile) with the transcript left blank —
    /// the machine fills it in once the transcript is final.
    pub answer_request: AnswerRequest,
    pub events: Arc<dyn EventSink>,
}

/// Outcome of a `stop` request (§4). The distinction is load-bearing: every
/// other outcome of a stop arrives as an event, so a stop that was silently
/// ignored would leave the UI stuck in "Finalizing…" forever. This return value
/// is the only way the frontend learns that nothing is coming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The stop was accepted; finalize is running and events will follow.
    Taken,
    /// Unknown id, already ended, or already stopping. Nothing will be emitted.
    NotTaken,
}

/// Hard limits (§3). All enforced in the core so the frontend cannot drift.
pub mod limits {
    use std::time::Duration;

    /// Stop -> final transcript. Past this, answer with what we have.
    pub const STT_FINALIZE: Duration = Duration::from_secs(5);
    /// Opening the Deepgram socket.
    pub const STT_CONNECT: Duration = Duration::from_secs(5);
    /// Stop -> first answer token.
    pub const LLM_FIRST_TOKEN: Duration = Duration::from_secs(10);
    /// Stop -> answer complete.
    pub const LLM_TOTAL: Duration = Duration::from_secs(60);
    /// Recording hard cap; auto-stops and answers normally.
    pub const MAX_RECORDING: Duration = Duration::from_secs(120);

    /// Longest typed question accepted by `ask` (§4).
    pub const MAX_ASK_CHARS: usize = 8000;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_streaming_answer_reports_total_not_zero_for_first_token() {
        // A provider that returns the whole answer in one shot has a real
        // time-to-first-word: the moment the answer arrived. Reporting 0 would
        // paint "0.0s to first word", which is a lie about the headline metric.
        let m = Metrics::finish(300, None, 1400);
        assert_eq!(m.first_token_ms, 1400);
        assert_eq!(m.total_ms, 1400);
    }

    #[test]
    fn streaming_answer_keeps_its_measured_first_token() {
        let m = Metrics::finish(300, Some(950), 2200);
        assert_eq!(m.stt_finalize_ms, 300);
        assert_eq!(m.first_token_ms, 950);
        assert_eq!(m.total_ms, 2200);
    }

    #[test]
    fn typed_questions_report_zero_stt_time() {
        let m = Metrics::finish(0, Some(700), 1800);
        assert_eq!(m.stt_finalize_ms, 0);
    }

    #[test]
    fn every_event_carries_its_session_id_and_wire_name() {
        let cases: Vec<(SessionEvent, &str)> = vec![
            (SessionEvent::SttPartial { session_id: 7, text: "t".into(), is_final: true }, "stt:partial"),
            (SessionEvent::LlmDelta { session_id: 7, delta: "d".into() }, "llm:delta"),
            (
                SessionEvent::LlmDone {
                    session_id: 7,
                    transcript: "t".into(),
                    answer: "a".into(),
                    metrics: Metrics::finish(1, Some(2), 3),
                },
                "llm:done",
            ),
            (SessionEvent::SessionError { session_id: 7, error: AppError::aborted() }, "session:error"),
            (SessionEvent::AudioLevel { session_id: 7, rms: 0.5 }, "audio:level"),
        ];
        for (ev, name) in cases {
            assert_eq!(ev.session_id(), 7);
            assert_eq!(ev.event_name(), name);
        }
    }

    #[test]
    fn metrics_serialize_as_camel_case_for_the_frontend() {
        let json = serde_json::to_string(&Metrics::finish(1, Some(2), 3)).unwrap();
        assert!(json.contains("sttFinalizeMs"), "got {json}");
        assert!(json.contains("firstTokenMs"), "got {json}");
        assert!(json.contains("totalMs"), "got {json}");
    }
}

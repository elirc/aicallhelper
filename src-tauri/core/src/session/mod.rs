//! Session contract: the events the core emits, the metrics it measures, and
//! the traits every dependency of the state machine is injected through.
//!
//! The state machine (`machine.rs`) depends only on what is declared here, so
//! `cargo test` drives the whole pipeline with fakes — no network, no audio
//! device, no clock skew.

pub mod machine;

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::error::AppError;
use crate::llm::{AnswerRequest, LlmProvider, StopReason};
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
        /// Why generation stopped (R2) — `token_limit` is shown as "cut
        /// short". Separate from `metrics` on purpose: timing is not outcome.
        stop_reason: StopReason,
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

/// How a session ended — or that it has not yet (R1, ADR 015).
///
/// Recorded by the core per session id at the same instant the slot is
/// released, so "no longer active" and "has a terminal outcome" can never be
/// observed apart. `start_session`/`ask` return it next to the id, and the
/// frontend looks it up once on adoption (`session_outcome`), so an early
/// terminal event it could not match yet is never the only record of how the
/// session ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum SessionOutcome {
    /// Still running; events will follow.
    Active,
    /// `llm:done` was (or is being) emitted with exactly these fields.
    #[serde(rename_all = "camelCase")]
    Completed {
        transcript: String,
        answer: String,
        metrics: Metrics,
        stop_reason: StopReason,
    },
    /// `session:error` was emitted. `transcript` and `partial` are what the UI
    /// had been shown when it failed, so an adoption that missed the stream
    /// still keeps the right text, marked incomplete.
    #[serde(rename_all = "camelCase")]
    Failed { error: AppError, transcript: String, partial: String },
    /// Cancelled or superseded: silent by contract (§5.1, §5.10).
    Cancelled,
    /// Never started, or retired from the bounded log (`OUTCOME_RETENTION`).
    Unknown,
}

impl SessionOutcome {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, SessionOutcome::Active | SessionOutcome::Unknown)
    }
}

/// What `start_session` and `ask` resolve with: the new id AND its outcome as
/// of the moment the command answered. A session that already ended (an
/// immediate connect failure, a local oversize rejection, an instant answer)
/// reports that here instead of a bare id the UI would wait on forever.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStart {
    pub session_id: SessionId,
    pub outcome: SessionOutcome,
}

/// How many sessions' outcomes the core keeps, newest first. An outcome is
/// retired only when this many NEWER sessions have been claimed. The frontend
/// asks only about the attempt it is adopting, and runs one attempt at a time,
/// so a retirement can never hit a pending adoption; the bound exists so a
/// long call does not grow the log without limit.
pub const OUTCOME_RETENTION: usize = 16;

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
    /// CPU inference can take longer; cloud limits remain unchanged.
    pub const LOCAL_FIRST_TOKEN: Duration = Duration::from_secs(90);
    pub const LOCAL_TOTAL: Duration = Duration::from_secs(300);
    /// Recording hard cap; auto-stops and answers normally.
    pub const MAX_RECORDING: Duration = Duration::from_secs(120);

    /// Longest typed question accepted by `ask` (§4).
    pub const MAX_ASK_CHARS: usize = 8000;
}

/// The two answer-stage deadlines the state machine arms once the transcript
/// is final, both counted from the stop instant (§3).
///
/// A provider carries its own pair (`LlmProvider::answer_limits`) instead of
/// the machine matching on `kind()`: the local model runs on the CPU and
/// legitimately needs minutes where a cloud model gets seconds, and keying
/// that off the kind meant the machine had to know every provider's pacing —
/// a provider added without the matching arm silently inherited the 10 s
/// cloud cap and timed out on every answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnswerLimits {
    /// Stop -> first answer token.
    pub first_token: Duration,
    /// Stop -> answer complete.
    pub total: Duration,
}

impl AnswerLimits {
    /// Cloud providers (§3): 10 s to the first token, 60 s in total.
    pub const CLOUD: Self =
        Self { first_token: limits::LLM_FIRST_TOKEN, total: limits::LLM_TOTAL };
    /// Free local mode: CPU inference, 90 s to the first token, 300 s in total.
    pub const LOCAL: Self =
        Self { first_token: limits::LOCAL_FIRST_TOKEN, total: limits::LOCAL_TOTAL };
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
                    stop_reason: StopReason::Complete,
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

    #[test]
    fn session_outcomes_have_the_wire_shapes_the_frontend_switches_on() {
        // `src/types.ts` mirrors these by hand; a renamed tag or field would
        // make every adoption-time reconciliation fall through to "unknown".
        use serde_json::json;
        let start = SessionStart { session_id: 3, outcome: SessionOutcome::Active };
        assert_eq!(
            serde_json::to_value(&start).unwrap(),
            json!({ "sessionId": 3, "outcome": { "status": "active" } })
        );
        let done = SessionOutcome::Completed {
            transcript: "q".into(),
            answer: "a".into(),
            metrics: Metrics::finish(0, Some(2), 3),
            stop_reason: StopReason::TokenLimit,
        };
        assert_eq!(
            serde_json::to_value(&done).unwrap(),
            json!({
                "status": "completed", "transcript": "q", "answer": "a",
                "metrics": { "sttFinalizeMs": 0, "firstTokenMs": 2, "totalMs": 3 },
                "stopReason": "token_limit"
            })
        );
        let failed = SessionOutcome::Failed {
            error: AppError::internal("x"),
            transcript: "q".into(),
            partial: "half".into(),
        };
        assert_eq!(
            serde_json::to_value(&failed).unwrap(),
            json!({
                "status": "failed", "error": { "code": "internal", "message": "x" },
                "transcript": "q", "partial": "half"
            })
        );
        assert_eq!(serde_json::to_value(SessionOutcome::Cancelled).unwrap(), json!({ "status": "cancelled" }));
        assert_eq!(serde_json::to_value(SessionOutcome::Unknown).unwrap(), json!({ "status": "unknown" }));
        assert!(done.is_terminal() && failed.is_terminal() && SessionOutcome::Cancelled.is_terminal());
        assert!(!SessionOutcome::Active.is_terminal() && !SessionOutcome::Unknown.is_terminal());
    }

    #[test]
    fn answer_limits_presets_mirror_the_pinned_constants() {
        // The presets are the only route the deadlines take into the machine;
        // if one drifts from `limits::*` the numbers SPEC §3 promises and the
        // numbers actually enforced diverge without any test noticing.
        assert_eq!(AnswerLimits::CLOUD.first_token, limits::LLM_FIRST_TOKEN);
        assert_eq!(AnswerLimits::CLOUD.total, limits::LLM_TOTAL);
        assert_eq!(AnswerLimits::LOCAL.first_token, limits::LOCAL_FIRST_TOKEN);
        assert_eq!(AnswerLimits::LOCAL.total, limits::LOCAL_TOTAL);
        // Local is the slow path by construction: a "longer" pair that is
        // shorter than the cloud pair would be a copy-paste error.
        assert!(AnswerLimits::LOCAL.first_token > AnswerLimits::CLOUD.first_token);
        assert!(AnswerLimits::LOCAL.total > AnswerLimits::CLOUD.total);
    }
}

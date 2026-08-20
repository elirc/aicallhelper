//! SessionEvent -> webview bridge (§4).

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use app_core::session::{EventSink, SessionEvent, SessionId};

use crate::state::{release_capture_if, AppState};

/// Map a core event to its wire form: the Tauri event name plus the bare
/// camelCase payload object.
///
/// `SessionEvent` serializes with a `kind` tag for its own round-trips, but on
/// the wire the event NAME already carries that information and the frontend
/// expects the bare object — leaking the tag would silently change every
/// payload shape the UI destructures.
pub fn wire_payload(event: &SessionEvent) -> (&'static str, Value) {
    let name = event.event_name();
    let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.remove("kind");
    }
    (name, value)
}

pub struct TauriEventSink {
    app: AppHandle,
}

impl TauriEventSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl EventSink for TauriEventSink {
    fn emit(&self, event: SessionEvent) {
        // LlmDone and SessionError are terminal for a pipeline. If the loopback
        // capture still belongs to that session (hard-cap auto-stop, mid-stream
        // failure), release the device here — nothing later will.
        if matches!(
            event,
            SessionEvent::LlmDone { .. } | SessionEvent::SessionError { .. }
        ) {
            release_capture(&self.app, event.session_id());
        }

        let (name, payload) = wire_payload(&event);
        // Emission fails while the webview is reloading or gone; the pipeline
        // must outlive the UI, not the other way around.
        let _ = self.app.emit(name, payload);
    }
}

fn release_capture(app: &AppHandle, session_id: SessionId) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    release_capture_if(&state, session_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_core::error::{AppError, ErrorCode};
    use app_core::session::Metrics;
    use serde_json::json;

    #[test]
    fn stt_partial_maps_to_its_name_and_bare_payload() {
        let (name, payload) = wire_payload(&SessionEvent::SttPartial {
            session_id: 7,
            text: "hello".into(),
            is_final: false,
        });
        assert_eq!(name, "stt:partial");
        assert_eq!(
            payload,
            json!({ "sessionId": 7, "text": "hello", "isFinal": false })
        );
    }

    #[test]
    fn llm_delta_maps_to_its_name_and_bare_payload() {
        let (name, payload) = wire_payload(&SessionEvent::LlmDelta {
            session_id: 7,
            delta: "wo".into(),
        });
        assert_eq!(name, "llm:delta");
        assert_eq!(payload, json!({ "sessionId": 7, "delta": "wo" }));
    }

    #[test]
    fn llm_done_carries_camel_case_metrics() {
        let (name, payload) = wire_payload(&SessionEvent::LlmDone {
            session_id: 7,
            transcript: "q".into(),
            answer: "a".into(),
            metrics: Metrics::finish(10, Some(20), 30),
        });
        assert_eq!(name, "llm:done");
        assert_eq!(
            payload,
            json!({
                "sessionId": 7,
                "transcript": "q",
                "answer": "a",
                "metrics": { "sttFinalizeMs": 10, "firstTokenMs": 20, "totalMs": 30 }
            })
        );
    }

    #[test]
    fn session_error_carries_code_and_message() {
        let (name, payload) = wire_payload(&SessionEvent::SessionError {
            session_id: 7,
            error: AppError::new(ErrorCode::NoSpeech, "m"),
        });
        assert_eq!(name, "session:error");
        assert_eq!(
            payload,
            json!({ "sessionId": 7, "error": { "code": "no_speech", "message": "m" } })
        );
    }

    #[test]
    fn audio_level_carries_rms() {
        let (name, payload) = wire_payload(&SessionEvent::AudioLevel {
            session_id: 7,
            rms: 0.5,
        });
        assert_eq!(name, "audio:level");
        assert_eq!(payload, json!({ "sessionId": 7, "rms": 0.5 }));
    }

    #[test]
    fn no_payload_ever_leaks_the_kind_tag() {
        let events = [
            SessionEvent::SttPartial { session_id: 1, text: "t".into(), is_final: true },
            SessionEvent::LlmDelta { session_id: 1, delta: "d".into() },
            SessionEvent::LlmDone {
                session_id: 1,
                transcript: "t".into(),
                answer: "a".into(),
                metrics: Metrics::finish(1, Some(2), 3),
            },
            SessionEvent::SessionError { session_id: 1, error: AppError::aborted() },
            SessionEvent::AudioLevel { session_id: 1, rms: 0.0 },
        ];
        for ev in events {
            let (_, payload) = wire_payload(&ev);
            let obj = payload.as_object().expect("payload is an object");
            assert!(!obj.contains_key("kind"), "kind leaked in {ev:?}");
            assert!(obj.contains_key("sessionId"), "sessionId missing in {ev:?}");
        }
    }
}

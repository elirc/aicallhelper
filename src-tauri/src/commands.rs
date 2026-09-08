//! IPC surface (§4). Every command resolves with an `Envelope` — success and
//! failure both travel the resolve lane, so the frontend has exactly one
//! decode path and a *rejected* invoke can only ever mean the shell itself is
//! broken.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use app_core::audio::capture::WasapiLoopbackCapture;
use app_core::audio::AudioCapture;
use app_core::error::{no_llm_key_message, ErrorCode, MSG_NO_STT_KEY};
use app_core::llm::anthropic::AnthropicProvider;
use app_core::llm::groq::GroqProvider;
use app_core::llm::local::LocalProvider;
use app_core::stt::{SttConnector, local::LocalConnector};
use app_core::llm::{build_system_prompt, AnswerRequest, LlmProvider, LlmProviderKind, Profile};
use app_core::session::{limits, SessionDeps, SessionId, StopOutcome};
use app_core::store::{Settings, SettingsPatch, SettingsView};
use app_core::stt::deepgram::DeepgramConnector;
use app_core::AppError;

use crate::events::TauriEventSink;
use crate::hotkey::{self, HotkeyState};
use crate::state::{lock, release_capture_if, ActiveCapture, AppState, ForwardingAudioSink};
use crate::window;

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// The one wire shape every command returns:
/// `{ "ok": true, "value": <T> }` | `{ "ok": false, "error": { code, message } }`.
///
/// Untagged, so each variant serializes exactly its own fields and nothing
/// else. `ok` is a plain bool field (set by the constructors, which are the
/// only way to build one) because serde has no literal-value types.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Envelope<T> {
    Ok { ok: bool, value: T },
    Err { ok: bool, error: AppError },
}

impl<T> Envelope<T> {
    pub fn ok(value: T) -> Self {
        Envelope::Ok { ok: true, value }
    }

    pub fn err(error: AppError) -> Self {
        Envelope::Err { ok: false, error }
    }

    pub fn from_result(result: Result<T, AppError>) -> Self {
        match result {
            Ok(value) => Self::ok(value),
            Err(error) => Self::err(error),
        }
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Envelope<SettingsView> {
    Envelope::ok(lock(&state.settings).view())
}

#[tauri::command]
pub fn set_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    patch: SettingsPatch,
) -> Envelope<SettingsView> {
    let (view, old_hotkey, old_always_on_top) = {
        let store = lock(&state.settings);
        let before = store.view();
        if let Err(error) = store.apply_patch(patch) {
            return Envelope::err(error);
        }
        (store.view(), before.hotkey, before.always_on_top)
    };

    // Side effects derive from the FRESH state and run after the settings lock
    // is released — hotkey registration and window flags call into the OS and
    // must not hold a lock the event loop may also want.
    if view.hotkey != old_hotkey {
        let status = hotkey::apply_hotkey(&app, &view.hotkey);
        *lock(&state.hotkey) = status;
    }
    if view.always_on_top != old_always_on_top {
        if let Some(win) = app.get_webview_window(window::MAIN_WINDOW) {
            let _ = win.set_always_on_top(view.always_on_top);
        }
    }

    // The returned view is what the UI re-renders from: main is the source of
    // truth, the form is a proposal.
    Envelope::ok(view)
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn start_session(app: AppHandle) -> Envelope<SessionId> {
    Envelope::from_result(start_session_inner(&app).await)
}

async fn start_session_inner(app: &AppHandle) -> Result<SessionId, AppError> {
    let state = app.state::<AppState>();
    let settings: Settings = lock(&state.settings).get().clone();

    // Both keys are proven present BEFORE anything opens (§4): failing here
    // costs nothing, failing after the socket is up costs the user a recording.
    let deepgram_key = if settings.llm_provider == LlmProviderKind::Local {
        String::new()
    } else {
        present(settings.deepgram_key.as_deref())
            .ok_or_else(|| AppError::new(ErrorCode::NoSttKey, MSG_NO_STT_KEY))?.to_string()
    };
    let llm_key = required_llm_key(&settings)?;

    // Deps are built fresh from current settings per session, so a settings
    // change takes effect on the next recording without any restart.
    let deps = build_deps(app, &settings, deepgram_key, llm_key);
    // The answer request follows within seconds of a recording starting; warm
    // the origin now so the TLS handshake is off the stop-to-first-word path.
    deps.llm.prewarm();

    let session_id = state.sessions.start(deps).await?;

    // Stop the superseded capture under a SHORT lock, released before the
    // (blocking, ~100 ms) WASAPI open below. Holding this mutex across the
    // open — or across any event emission — is how the shell once deadlocked:
    // TauriEventSink::emit re-enters release_capture_if on terminal events,
    // which locks this same mutex on the same thread.
    if let Some(old) = lock(&state.audio).take() {
        old.handle.stop();
    }

    // Start capture only after the new session exists. Frames from a briefly
    // overlapping superseded capture carry the old id and are dropped by the
    // machine, so overlap is harmless — a gap would eat the first spoken words.
    let sink = Arc::new(ForwardingAudioSink {
        session_id,
        sessions: Arc::clone(&state.sessions),
    });
    match WasapiLoopbackCapture.start(sink) {
        Ok(handle) => {
            let mut audio = lock(&state.audio);
            // Install only while this session still owns the machine slot —
            // §5.2's "never install yourself over the winner", applied at the
            // shell layer. A start superseded during the device open must
            // neither steal the winner's capture nor leave its own running.
            if state.sessions.is_active(session_id) {
                if let Some(loser) = audio.replace(ActiveCapture { session_id, handle }) {
                    loser.handle.stop();
                }
            } else {
                drop(audio);
                handle.stop();
            }
        }
        Err(error) => {
            // A session with no audio can only ever end in a misleading
            // no_speech ("make sure call audio is playing" — for a missing
            // DEVICE). Fail the start honestly instead: the machine session is
            // cancelled silently and the device error travels back in the
            // command envelope, which the UI renders through its normal
            // start-failure path. An event emitted here instead would race the
            // invoke resolution and be dropped pre-adoption (§9).
            state.sessions.cancel(session_id);
            return Err(error);
        }
    }

    Ok(session_id)
}

#[tauri::command]
pub async fn stop_session(app: AppHandle, session_id: SessionId) -> Envelope<()> {
    let state = app.state::<AppState>();
    match state.sessions.stop(session_id).await {
        StopOutcome::Taken => {
            // Recording is over the moment stop is accepted; release the
            // loopback device now rather than when the answer lands.
            release_capture_if(&state, session_id);
            Envelope::ok(())
        }
        // This error IS the contract: every other stop outcome arrives as an
        // event, so this return is the only way the UI learns that nothing is
        // coming and can leave "Finalizing…".
        StopOutcome::NotTaken => Envelope::err(stop_not_taken_error()),
    }
}

pub fn stop_not_taken_error() -> AppError {
    AppError::internal(
        "Stop not taken: that session is unknown, already stopping, or already ended. \
         No further events will arrive for it.",
    )
}

#[tauri::command]
pub async fn ask(app: AppHandle, text: String) -> Envelope<SessionId> {
    Envelope::from_result(ask_inner(&app, text).await)
}

async fn ask_inner(app: &AppHandle, text: String) -> Result<SessionId, AppError> {
    let text = validate_ask_text(&text)?;

    let state = app.state::<AppState>();
    let settings: Settings = lock(&state.settings).get().clone();

    let llm_key = required_llm_key(&settings)?;

    // Ask supersedes whatever was live; the old session's capture would
    // otherwise keep the loopback device open feeding frames the machine
    // drops.
    if let Some(old) = lock(&state.audio).take() {
        old.handle.stop();
    }

    // Typed questions never touch STT, so a missing Deepgram key must not
    // block them — the connector exists only to satisfy the deps shape.
    let deepgram_key = settings.deepgram_key.clone().unwrap_or_default();
    let deps = build_deps(app, &settings, deepgram_key, llm_key);
    state.sessions.ask(&text, deps).await
}

/// Trim, then enforce 1..=MAX_ASK_CHARS. Counted in characters, not bytes:
/// the limit is about question size, and a multi-byte character is still one
/// character to the person who typed it.
pub fn validate_ask_text(text: &str) -> Result<String, AppError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(AppError::internal("Type a question first."));
    }
    if trimmed.chars().count() > limits::MAX_ASK_CHARS {
        return Err(AppError::internal(format!(
            "Question is too long — the limit is {} characters.",
            limits::MAX_ASK_CHARS
        )));
    }
    Ok(trimmed.to_string())
}

#[tauri::command]
pub fn cancel_session(app: AppHandle, session_id: SessionId) -> Envelope<()> {
    let state = app.state::<AppState>();
    // Fire-and-forget by contract: cancelling an id that no longer exists is
    // the normal supersession race, never an error — so there is no error path
    // out of this command at all.
    state.sessions.cancel(session_id);
    release_capture_if(&state, session_id);
    Envelope::ok(())
}

// ---------------------------------------------------------------------------
// Hotkey / links
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn hotkey_status(state: State<'_, AppState>) -> Envelope<HotkeyState> {
    Envelope::ok(lock(&state.hotkey).clone())
}

#[tauri::command]
pub fn open_external(url: String) -> Envelope<()> {
    if !window::is_safe_external_url(&url) {
        return Envelope::err(AppError::internal("Only https:// links can be opened."));
    }
    window::open_in_browser(&url);
    Envelope::ok(())
}

// ---------------------------------------------------------------------------
// Deps assembly
// ---------------------------------------------------------------------------

fn build_deps(
    app: &AppHandle,
    settings: &Settings,
    deepgram_key: String,
    llm_key: String,
) -> SessionDeps {
    let profile = Profile {
        resume: &settings.resume,
        job_description: &settings.job_description,
    };
    let system = build_system_prompt(profile, settings.answer_style);
    let llm: Arc<dyn LlmProvider> = match settings.llm_provider {
        LlmProviderKind::Anthropic => Arc::new(AnthropicProvider::new(llm_key)),
        LlmProviderKind::Groq => Arc::new(GroqProvider::new(llm_key)),
        LlmProviderKind::Local => Arc::new(LocalProvider),
    };
    let stt: Arc<dyn SttConnector> = if settings.llm_provider == LlmProviderKind::Local {
        Arc::new(LocalConnector)
    } else {
        Arc::new(DeepgramConnector::new(deepgram_key))
    };
    SessionDeps {
        stt,
        llm,
        answer_request: AnswerRequest::new(system),
        events: Arc::new(TauriEventSink::new(app.clone())),
    }
}

fn required_llm_key(settings: &Settings) -> Result<String, AppError> {
    if settings.llm_provider == LlmProviderKind::Local { return Ok(String::new()); }
    present(settings.active_llm_key()).map(str::to_owned)
        .ok_or_else(|| AppError::new(ErrorCode::NoLlmKey, no_llm_key_message(settings.llm_provider.label())))
}

fn present(key: Option<&str>) -> Option<&str> {
    key.map(str::trim).filter(|k| !k.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_answers_need_no_cloud_keys() {
        let mut settings = Settings::default();
        assert!(required_llm_key(&settings).is_err());
        settings.llm_provider = LlmProviderKind::Local;
        assert_eq!(required_llm_key(&settings).unwrap(), "");
    }

    // --- envelope wire shape: the frontend destructures these exact keys ---

    #[test]
    fn ok_envelope_serializes_to_the_exact_wire_shape() {
        let v = serde_json::to_value(Envelope::ok(42u64)).unwrap();
        assert_eq!(v, json!({ "ok": true, "value": 42 }));
    }

    #[test]
    fn ok_envelope_with_unit_value_still_carries_the_value_key() {
        // stop_session/cancel_session resolve with value:null — the key must
        // exist so `if (res.ok)` and `res.value` behave uniformly.
        let v = serde_json::to_value(Envelope::ok(())).unwrap();
        assert_eq!(v, json!({ "ok": true, "value": null }));
    }

    #[test]
    fn err_envelope_serializes_code_and_message() {
        let v = serde_json::to_value(Envelope::<u64>::err(AppError::new(
            ErrorCode::NoSttKey,
            "m",
        )))
        .unwrap();
        assert_eq!(
            v,
            json!({ "ok": false, "error": { "code": "no_stt_key", "message": "m" } })
        );
    }

    #[test]
    fn err_envelope_never_carries_a_value_key_and_ok_never_an_error_key() {
        let err = serde_json::to_value(Envelope::<u64>::err(AppError::aborted())).unwrap();
        assert!(err.get("value").is_none());
        let ok = serde_json::to_value(Envelope::ok(1u64)).unwrap();
        assert!(ok.get("error").is_none());
    }

    #[test]
    fn stop_not_taken_is_an_error_envelope() {
        let v = serde_json::to_value(Envelope::<()>::err(stop_not_taken_error())).unwrap();
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"]["code"], json!("internal"));
    }

    // --- ask validation ---

    #[test]
    fn ask_text_is_trimmed() {
        assert_eq!(validate_ask_text("  hi there \n").unwrap(), "hi there");
    }

    #[test]
    fn empty_or_whitespace_ask_is_rejected() {
        assert!(validate_ask_text("").is_err());
        assert!(validate_ask_text("   \n\t ").is_err());
    }

    #[test]
    fn ask_at_the_limit_passes_and_one_over_fails() {
        let at_limit = "a".repeat(limits::MAX_ASK_CHARS);
        assert!(validate_ask_text(&at_limit).is_ok());
        let over = "a".repeat(limits::MAX_ASK_CHARS + 1);
        assert!(validate_ask_text(&over).is_err());
    }

    #[test]
    fn ask_limit_counts_characters_not_bytes() {
        // 8000 four-byte characters is 32 KB of UTF-8 but exactly at the
        // character limit; a byte-counted check would wrongly reject it.
        let at_limit = "\u{1F600}".repeat(limits::MAX_ASK_CHARS);
        assert!(validate_ask_text(&at_limit).is_ok());
    }

    #[test]
    fn present_treats_blank_keys_as_missing() {
        // A key of spaces passes an is_some() check and then fails at the
        // provider with a confusing auth error; catch it at the gate instead.
        assert_eq!(present(None), None);
        assert_eq!(present(Some("")), None);
        assert_eq!(present(Some("   ")), None);
        assert_eq!(present(Some(" k ")), Some("k"));
    }
}

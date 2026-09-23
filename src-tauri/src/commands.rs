//! IPC surface (§4). Every command resolves with an `Envelope` — success and
//! failure both travel the resolve lane, so the frontend has exactly one
//! decode path and a *rejected* invoke can only ever mean the shell itself is
//! broken.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tokio::sync::oneshot;

use app_core::audio::capture::WasapiLoopbackCapture;
use app_core::audio::AudioCapture;
use app_core::error::{no_llm_key_message, ErrorCode, MSG_NO_STT_KEY};
use app_core::llm::anthropic::AnthropicProvider;
use app_core::llm::groq::GroqProvider;
use app_core::llm::local::{self, LocalProvider};
use app_core::llm::{
    build_system_prompt, AnswerRequest, AnswerStyle, BudgetStatus, LlmProvider, LlmProviderKind,
    LocalPromptBudget, SystemPrompt,
};
use app_core::session::{
    limits, SessionDeps, SessionId, SessionOutcome, SessionStart, StopOutcome,
};
use app_core::store::{
    CallProfile, CallProfilePatch, DesiredOsState, OsEffects, Settings, SettingsPatch, SettingsView,
};
use app_core::stt::deepgram::DeepgramConnector;
use app_core::stt::{local::LocalConnector, SttConnector};
use app_core::AppError;

use crate::events::TauriEventSink;
use crate::hotkey::{self, HotkeyState};
use crate::state::{lock, release_capture_if, ActiveCapture, AppState, ForwardingAudioSink};
use crate::window::{self, DockSize};

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

/// Async so the fsync'd write never runs on the event-loop thread (RS-3):
/// `apply_patch` does File::create + write + sync_all + rename, re-encrypts
/// every stored key through DPAPI and re-serializes every profile — 5-50 ms
/// of frozen paint per style-chip click when it ran as a sync command.
#[tauri::command]
pub async fn set_settings(app: AppHandle, patch: SettingsPatch) -> Envelope<SettingsView> {
    Envelope::from_result(set_settings_inner(app, patch).await)
}

async fn set_settings_inner(
    app: AppHandle,
    patch: SettingsPatch,
) -> Result<SettingsView, AppError> {
    // Lock + apply on the blocking pool: the store mutex is taken and
    // released entirely inside the task, so no std::Mutex is ever held across
    // an await, and a slow disk stalls a blocking thread rather than a
    // runtime worker. The revision compare happens inside `apply_patch`,
    // under the store's own write lock (R5, ADR 016).
    let worker = app.clone();
    let view = tokio::task::spawn_blocking(move || {
        let state = worker.state::<AppState>();
        let store = lock(&state.settings);
        store.apply_patch(patch)
    })
    .await
    // A JoinError means the task panicked; `lock` recovers a poisoned mutex,
    // so the next attempt has every chance — say so rather than "internal".
    .map_err(|_| AppError::internal("Could not save settings. Try again."))??;

    // Effects step (R5, ADR 016): bring the OS in line with the CURRENT
    // committed settings, not with this patch's before/after. Runs are
    // serialized inside the reconciler and each reads the store afresh, so
    // two saves whose effect steps run in the opposite order to their commits
    // still leave the newest hotkey registered, and a later style-only save
    // cannot skip an earlier save's hotkey change. On the blocking pool
    // because the hotkey call waits for the main thread. A failed commit
    // returned above: it changed nothing, so it has nothing to reconcile.
    let worker = app.clone();
    let _ = tokio::task::spawn_blocking(move || reconcile_os_state(&worker)).await;

    // The returned view is what the UI re-renders from: main is the source of
    // truth, the form is a proposal. It carries the revision this save
    // committed, which the UI uses to ignore a response older than a view it
    // already holds.
    Ok(view)
}

/// Apply the current committed settings' OS state: hotkey, always-on-top,
/// and the dock that choosing "under the camera" performs. Idempotent; see
/// `app_core::store::effects`.
fn reconcile_os_state(app: &AppHandle) {
    let state = app.state::<AppState>();
    let os = ShellOs { app };
    // The closure takes the settings lock only to read, inside the
    // reconciler's run lock and before any OS call, so no OS call ever
    // happens while the settings lock is held.
    state.effects.reconcile(|| DesiredOsState::of(&lock(&state.settings).get()), &os);
}

/// The Tauri side of `OsEffects`.
struct ShellOs<'a> {
    app: &'a AppHandle,
}

impl OsEffects for ShellOs<'_> {
    fn apply_hotkey(&self, accelerator: &str) -> bool {
        let status = apply_hotkey_on_main_thread(self.app, accelerator.to_string());
        let took = hotkey_took(&status);
        // Written inside the reconciler's run, so `hotkey_status` always
        // reports the registration of the newest settings that were applied.
        *lock(&self.app.state::<AppState>().hotkey) = status;
        took
    }

    fn set_always_on_top(&self, on: bool) {
        if let Some(win) = self.app.get_webview_window(window::MAIN_WINDOW) {
            // Safe from any thread: tauri dispatches window flags to the
            // event loop itself.
            let _ = win.set_always_on_top(on);
        }
    }

    fn dock_to_camera(&self) {
        if let Some(win) = self.app.get_webview_window(window::MAIN_WINDOW) {
            // Choosing "dock under the camera" demonstrates itself at once,
            // at the current size — the preset is the header button's job.
            // Best-effort: the setting is saved either way.
            let _ = window::dock_to_camera(&win, DockSize::Keep);
        }
    }
}

/// Whether the OS reflects the requested shortcut: it registered, or none
/// was requested. A refused registration is retried by the next effect run.
pub fn hotkey_took(status: &HotkeyState) -> bool {
    status.registered || status.accelerator.trim().is_empty()
}

/// Re-register the shortcut from the main thread and wait for the outcome.
///
/// Belt and braces: the pinned global-shortcut plugin does not strictly
/// require the main thread, but registering from the thread that owns the
/// plugin's hidden window is the documented-safe path on Windows
/// (RegisterHotKey binds a hot key to the calling thread's window), and the
/// failure mode of getting this wrong — every hotkey change silently reported
/// as `registered: false` — is exactly the dead key `hotkey_status` exists to
/// prevent. If the event loop refuses the hop (shutting down) or drops the
/// closure unrun, register from here rather than report a key nobody tried.
///
/// Blocking: called from the reconciler on a blocking-pool thread, which may
/// wait; never call it from the event loop, which is the thread it waits on.
fn apply_hotkey_on_main_thread(app: &AppHandle, accelerator: String) -> HotkeyState {
    let (tx, rx) = oneshot::channel();
    let main = app.clone();
    let on_main = accelerator.clone();
    let hop = app.run_on_main_thread(move || {
        let _ = tx.send(hotkey::apply_hotkey(&main, &on_main));
    });
    if hop.is_ok() {
        if let Ok(status) = rx.blocking_recv() {
            return status;
        }
    }
    hotkey::apply_hotkey(app, &accelerator)
}

// ---------------------------------------------------------------------------
// Local prompt budget (R4)
// ---------------------------------------------------------------------------

/// The local request budget for an UNSAVED profile draft plus style and
/// question, so Settings previews what the user is typing, not what was last
/// saved. Pure computation; async only to keep it off the event loop, since
/// a draft can carry two 200 000-character fields.
#[tauri::command]
pub async fn local_prompt_budget(
    profile: CallProfilePatch,
    answer_style: AnswerStyle,
    question: Option<String>,
) -> Envelope<LocalPromptBudget> {
    Envelope::ok(budget_for_draft(profile, answer_style, question.as_deref().unwrap_or("")))
}

/// Through `CallProfile::from`, the same lenient conversion a save uses, so
/// the preview reads the draft exactly as the store will.
fn budget_for_draft(profile: CallProfilePatch, style: AnswerStyle, question: &str) -> LocalPromptBudget {
    let profile = CallProfile::from(profile);
    app_core::llm::local_prompt_budget(profile.as_prompt(), style, question)
}

/// Refuse a local request that cannot fit BEFORE anything starts (R4): with
/// an empty question before recording (the known overhead — no transcript
/// of any length would fit), with the actual text before a typed ask. The
/// refusal is the gate's own error, word for word, and the gate in
/// `local::request_body` still checks the real transcript after Stop.
fn local_budget_check(settings: &Settings, question: &str) -> Result<(), AppError> {
    if !settings.llm_provider.caps_input_bytes() {
        return Ok(());
    }
    let budget = app_core::llm::local_prompt_budget(
        settings.active_profile().as_prompt(),
        settings.answer_style,
        question,
    );
    if budget.status == BudgetStatus::Over {
        return Err(local::oversize_error());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Resolves with the new id AND its outcome as of this moment (R1, ADR 015):
/// a session that already ended while the device was opening — an immediate
/// connect failure, say — says so here, because its terminal event may have
/// reached the webview before the UI could match its id.
#[tauri::command]
pub async fn start_session(app: AppHandle) -> Envelope<SessionStart> {
    Envelope::from_result(start_session_inner(&app).await)
}

async fn start_session_inner(app: &AppHandle) -> Result<SessionStart, AppError> {
    let state = app.state::<AppState>();
    let settings: Settings = lock(&state.settings).get();

    // Both keys are proven present BEFORE anything opens (§4): failing here
    // costs nothing, failing after the socket is up costs the user a recording.
    let deepgram_key = required_deepgram_key(&settings)?;
    let llm_key = required_llm_key(&settings)?;
    // A local profile too long for any question is refused before the
    // device opens, not after the user has spoken (R4).
    local_budget_check(&settings, "")?;

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
    // RS-2: the device open blocks for ~100 ms. On a runtime worker that also
    // parked the STT dial `start` just spawned in the worker's non-stealable
    // LIFO slot, so the socket did not begin dialling until the device was
    // open. The blocking pool keeps every worker free; audio is buffered
    // pre-open on the STT side, so nothing is lost either way.
    let opened = tokio::task::spawn_blocking(move || WasapiLoopbackCapture.start(sink))
        .await
        // A JoinError (the open panicked) folds into the same path as a
        // device error, so every failure mode still cancels the machine
        // session below instead of leaving it to end in a misleading
        // no_speech.
        .unwrap_or_else(|_| Err(capture_thread_failed()));
    match opened {
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

    // Read LAST, after the device open: that ~100 ms is exactly the window in
    // which the driver can already have failed or finished. The outcome is
    // recorded under the same lock that releases the slot, so this read is
    // consistent with the `is_active` check above.
    Ok(SessionStart { session_id, outcome: state.sessions.outcome(session_id) })
}

/// The capture thread panicked before it could report a device. Distinct
/// from a device error so the message says the one thing that helps.
pub fn capture_thread_failed() -> AppError {
    AppError::internal("The audio capture thread failed to start. Try restarting the app.")
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

/// Same contract as `start_session`: the id plus its outcome so far. A typed
/// question's answer task starts before this returns, so an instant local
/// oversize rejection — or an instant answer — is reported here (R1).
#[tauri::command]
pub async fn ask(app: AppHandle, text: String) -> Envelope<SessionStart> {
    Envelope::from_result(ask_inner(&app, text).await)
}

async fn ask_inner(app: &AppHandle, text: String) -> Result<SessionStart, AppError> {
    let text = validate_ask_text(&text)?;

    let state = app.state::<AppState>();
    let settings: Settings = lock(&state.settings).get();

    let llm_key = required_llm_key(&settings)?;
    // Checked before a session is claimed, so a question that cannot fit
    // starts nothing, and the error envelope keeps the typed text in the
    // box (R4).
    local_budget_check(&settings, &text)?;

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
    let session_id = state.sessions.ask(&text, deps).await?;
    Ok(SessionStart { session_id, outcome: state.sessions.outcome(session_id) })
}

/// The adoption-time reconciliation lookup (R1, ADR 015): the frontend calls
/// it once right after it adopts an id whose start envelope said `active`,
/// covering a session that ended between that envelope and the adoption.
/// Read-only and idempotent; ids outside the retained log are `unknown`.
#[tauri::command]
pub fn session_outcome(app: AppHandle, session_id: SessionId) -> Envelope<SessionOutcome> {
    Envelope::ok(app.state::<AppState>().sessions.outcome(session_id))
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
// Hotkey / window
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn hotkey_status(state: State<'_, AppState>) -> Envelope<HotkeyState> {
    Envelope::ok(lock(&state.hotkey).clone())
}

/// Sync on purpose: window operations belong on the event-loop thread, and
/// Tauri 2 runs non-async commands there. The header button always applies
/// the wide reading preset; launch-time docking (`DockSize::Keep`) lives in
/// `lib.rs`.
#[tauri::command]
pub fn dock_to_camera(app: AppHandle) -> Envelope<()> {
    let Some(win) = app.get_webview_window(window::MAIN_WINDOW) else {
        return Envelope::err(AppError::internal("The main window is not available."));
    };
    Envelope::from_result(window::dock_to_camera(&win, DockSize::Preset))
}

// ---------------------------------------------------------------------------
// Deps assembly
// ---------------------------------------------------------------------------

/// The prompt every answer in this session is grounded in: the ACTIVE
/// profile, borrowed (never cloned) into the builder. Kept apart from
/// `build_deps` so the "switching profiles re-grounds the next answer" rule
/// is testable without an `AppHandle`.
fn system_prompt_for(settings: &Settings) -> SystemPrompt {
    build_system_prompt(settings.active_profile().as_prompt(), settings.answer_style)
}

fn build_deps(
    app: &AppHandle,
    settings: &Settings,
    deepgram_key: String,
    llm_key: String,
) -> SessionDeps {
    let system = system_prompt_for(settings);
    let llm: Arc<dyn LlmProvider> = match settings.llm_provider {
        LlmProviderKind::Anthropic => Arc::new(AnthropicProvider::new(llm_key)),
        LlmProviderKind::Groq => Arc::new(GroqProvider::new(llm_key)),
        LlmProviderKind::Local => Arc::new(LocalProvider),
    };
    // The speech path is a capability of the provider (R5), not a second
    // match on the kind: a provider that does not transcribe through Deepgram
    // gets the loopback speech service that ships with free local mode.
    let stt: Arc<dyn SttConnector> = if settings.llm_provider.uses_deepgram() {
        Arc::new(DeepgramConnector::new(deepgram_key))
    } else {
        Arc::new(LocalConnector)
    };
    SessionDeps {
        stt,
        llm,
        answer_request: AnswerRequest::new(system),
        events: Arc::new(TauriEventSink::new(app.clone())),
    }
}

/// The Deepgram key a recording needs — or nothing, for a provider whose
/// speech never leaves the machine (§6.5). Keyed off `uses_deepgram()` rather
/// than `== Local` so a keyless provider added later is not refused by a
/// check written for the cloud pair (R5).
fn required_deepgram_key(settings: &Settings) -> Result<String, AppError> {
    if !settings.llm_provider.uses_deepgram() {
        return Ok(String::new());
    }
    present(settings.deepgram_key.as_deref())
        .map(str::to_owned)
        .ok_or_else(|| AppError::new(ErrorCode::NoSttKey, MSG_NO_STT_KEY))
}

fn required_llm_key(settings: &Settings) -> Result<String, AppError> {
    if !settings.llm_provider.needs_cloud_keys() {
        return Ok(String::new());
    }
    present(settings.active_llm_key()).map(str::to_owned).ok_or_else(|| {
        AppError::new(ErrorCode::NoLlmKey, no_llm_key_message(settings.llm_provider.label()))
    })
}

fn present(key: Option<&str>) -> Option<&str> {
    key.map(str::trim).filter(|k| !k.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_core::llm::CallType;
    use serde_json::json;

    // --- R4: local budget pre-checks ------------------------------------------

    fn local_settings(profiles: Vec<CallProfile>, active: &str) -> Settings {
        Settings {
            profiles,
            active_profile_id: active.into(),
            llm_provider: LlmProviderKind::Local,
            ..Settings::default()
        }
    }

    #[test]
    fn a_refused_hotkey_does_not_count_as_applied_but_a_disabled_one_does() {
        let state = |accelerator: &str, registered| HotkeyState { accelerator: accelerator.into(), registered };
        assert!(hotkey_took(&state("Ctrl+Shift+Space", true)));
        assert!(!hotkey_took(&state("Ctrl+Shift+Space", false)), "taken or unparseable: retry on the next save");
        assert!(hotkey_took(&HotkeyState::disabled()), "no shortcut requested is the requested state");
        assert!(hotkey_took(&state("  ", false)));
    }

    #[test]
    fn only_the_local_provider_caps_input_bytes() {
        assert!(LlmProviderKind::Local.caps_input_bytes());
        assert!(!LlmProviderKind::Anthropic.caps_input_bytes());
        assert!(!LlmProviderKind::Groq.caps_input_bytes());
    }

    #[test]
    fn the_budget_command_previews_the_unsaved_draft_exactly_as_a_save_reads_it() {
        // An unsaved draft with a callType string the store would read as
        // interview: the preview must read it the same way.
        let draft = CallProfilePatch {
            call_type: "bogus".into(),
            resume: "  Draft resume  ".into(),
            ..Default::default()
        };
        let got = budget_for_draft(draft, AnswerStyle::Balanced, "Hi?");
        let expected = app_core::llm::local_prompt_budget(
            app_core::llm::Profile { resume: "Draft resume", ..Default::default() },
            AnswerStyle::Balanced,
            "Hi?",
        );
        assert_eq!(got, expected);
        assert_eq!(got.profile_bytes, "Draft resume".len());
        assert_eq!(got.status, BudgetStatus::Ok);
    }

    #[test]
    fn a_local_profile_with_no_room_is_refused_before_recording_with_the_gates_error() {
        let huge = "x".repeat(local::MAX_INPUT_BYTES);
        let settings = local_settings(
            vec![CallProfile { resume: huge.clone(), ..CallProfile::empty("big", "Big") }],
            "big",
        );
        assert_eq!(local_budget_check(&settings, "").unwrap_err(), local::oversize_error());
        // Cloud providers have no byte cap: the same profile records.
        let cloud = Settings { llm_provider: LlmProviderKind::Anthropic, ..settings };
        assert!(local_budget_check(&cloud, "").is_ok());
    }

    #[test]
    fn a_typed_question_is_checked_at_its_real_size_before_any_session_starts() {
        let settings = local_settings(vec![CallProfile::empty("a", "A")], "a");
        assert!(local_budget_check(&settings, "").is_ok());
        let base = app_core::llm::local_prompt_budget(
            settings.active_profile().as_prompt(),
            settings.answer_style,
            "",
        );
        let room = base.remaining_bytes as usize;
        assert!(local_budget_check(&settings, &"q".repeat(room)).is_ok(), "exactly at the limit fits");
        assert_eq!(
            local_budget_check(&settings, &"q".repeat(room + 1)).unwrap_err(),
            local::oversize_error(),
            "one byte over is the gate's own refusal"
        );
    }

    #[test]
    fn the_pre_check_follows_the_active_profile_across_a_switch() {
        let long = CallProfile { resume: "x".repeat(local::MAX_INPUT_BYTES), ..CallProfile::empty("long", "Long") };
        let short = CallProfile { resume: "Short".into(), ..CallProfile::empty("short", "Short") };
        let mut settings = local_settings(vec![long, short], "short");
        assert!(local_budget_check(&settings, "").is_ok());
        settings.active_profile_id = "long".into();
        assert!(local_budget_check(&settings, "").is_err());
    }

    // --- key gates: capabilities of the provider, not `== Local` checks ---

    #[test]
    fn local_answers_need_no_cloud_keys() {
        let mut settings = Settings::default();
        assert!(required_llm_key(&settings).is_err());
        settings.llm_provider = LlmProviderKind::Local;
        assert_eq!(required_llm_key(&settings).unwrap(), "");
    }

    #[test]
    fn cloud_providers_are_refused_without_their_own_key() {
        // Each cloud provider is gated on ITS key: a stored Anthropic key
        // must not let a Groq session start and fail at the provider with a
        // confusing 401 instead of the "No Groq API key set…" nudge.
        let mut settings = Settings { anthropic_key: Some("a-key".into()), ..Settings::default() };
        assert_eq!(required_llm_key(&settings).unwrap(), "a-key");
        settings.llm_provider = LlmProviderKind::Groq;
        let err = required_llm_key(&settings).unwrap_err();
        assert_eq!(err.code, ErrorCode::NoLlmKey);
        assert_eq!(err.message, no_llm_key_message("Groq"));
        settings.groq_key = Some("  g-key ".into());
        assert_eq!(required_llm_key(&settings).unwrap(), "g-key");
    }

    #[test]
    fn deepgram_is_required_exactly_when_the_provider_transcribes_through_it() {
        // Local mode must start with no keys stored at all (§6.5); the cloud
        // pair must fail BEFORE the socket opens, with the pinned message.
        let mut settings = Settings::default();
        let err = required_deepgram_key(&settings).unwrap_err();
        assert_eq!((err.code, err.message.as_str()), (ErrorCode::NoSttKey, MSG_NO_STT_KEY));
        settings.deepgram_key = Some(" dg ".into());
        assert_eq!(required_deepgram_key(&settings).unwrap(), "dg");

        settings.llm_provider = LlmProviderKind::Groq;
        assert_eq!(required_deepgram_key(&settings).unwrap(), "dg");

        settings.llm_provider = LlmProviderKind::Local;
        settings.deepgram_key = None;
        assert_eq!(required_deepgram_key(&settings).unwrap(), "");
    }

    // --- the answer is grounded in the ACTIVE profile (§8) ---

    #[test]
    fn the_prompt_is_built_from_the_active_profile_only() {
        let mut settings = Settings {
            profiles: vec![
                CallProfile {
                    resume: "Backend engineer, tokio.".into(),
                    ..CallProfile::empty("backend", "Backend")
                },
                CallProfile {
                    call_type: CallType::Sales,
                    resume: "Account executive.".into(),
                    job_description: "Renewal with Initech.".into(),
                    ..CallProfile::empty("sales", "Sales")
                },
            ],
            active_profile_id: "backend".into(),
            ..Settings::default()
        };
        let backend = system_prompt_for(&settings);
        assert!(backend.cached_prefix.contains("Backend engineer, tokio."));
        assert!(!backend.cached_prefix.contains("Account executive."));

        // A switch is a cache write, not a per-question lookup: the same
        // settings with a different active id grounds the next session in
        // the other profile and nothing from the first one.
        settings.active_profile_id = "sales".into();
        let sales = system_prompt_for(&settings);
        assert!(sales.cached_prefix.contains("Account executive."));
        assert!(sales.cached_prefix.contains("Renewal with Initech."));
        assert!(!sales.cached_prefix.contains("Backend engineer, tokio."));
        assert_ne!(backend.cached_prefix, sales.cached_prefix);
        // Style rides outside the cached prefix (ADR 007), unchanged by the
        // profile switch.
        assert_eq!(backend.style_suffix, sales.style_suffix);
    }

    // --- envelope wire shape: the frontend destructures these exact keys ---

    #[test]
    fn ok_envelope_serializes_to_the_exact_wire_shape() {
        let v = serde_json::to_value(Envelope::ok(42u64)).unwrap();
        assert_eq!(v, json!({ "ok": true, "value": 42 }));
    }

    #[test]
    fn ok_envelope_with_unit_value_still_carries_the_value_key() {
        // stop_session/cancel_session/dock_to_camera resolve with value:null —
        // the key must exist so `if (res.ok)` and `res.value` behave uniformly.
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
    fn start_envelopes_carry_the_id_and_the_outcome_so_far() {
        // R1: `start_session`/`ask` no longer resolve with a bare id. The
        // frontend destructures `value.sessionId` and switches on
        // `value.outcome.status`; a bare number here would read as an
        // adoption with no outcome and fall back to waiting on events.
        let active = serde_json::to_value(Envelope::ok(SessionStart {
            session_id: 4,
            outcome: SessionOutcome::Active,
        }))
        .unwrap();
        assert_eq!(
            active,
            json!({ "ok": true, "value": { "sessionId": 4, "outcome": { "status": "active" } } })
        );
        let failed = serde_json::to_value(Envelope::ok(SessionStart {
            session_id: 5,
            outcome: SessionOutcome::Failed {
                error: AppError::new(ErrorCode::LlmHttp, "too big"),
                transcript: "q".into(),
                partial: String::new(),
            },
        }))
        .unwrap();
        // An already-failed session is still an ok envelope: the start itself
        // succeeded, and the UI settles the adopted attempt from the outcome.
        assert_eq!(failed["ok"], json!(true));
        assert_eq!(failed["value"]["outcome"]["status"], json!("failed"));
        assert_eq!(failed["value"]["outcome"]["error"]["code"], json!("llm_http"));
    }

    #[test]
    fn stop_not_taken_is_an_error_envelope() {
        let v = serde_json::to_value(Envelope::<()>::err(stop_not_taken_error())).unwrap();
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"]["code"], json!("internal"));
    }

    #[test]
    fn a_panicked_capture_open_is_an_internal_error_the_ui_can_show() {
        // RS-2 moved the WASAPI open onto the blocking pool; a JoinError there
        // must reach the UI as an ordinary start failure (so the machine
        // session is cancelled and the Record button recovers), never as a
        // rejected invoke. The message is what the user reads.
        let err = capture_thread_failed();
        assert_eq!(err.code, ErrorCode::Internal);
        assert_eq!(
            err.message,
            "The audio capture thread failed to start. Try restarting the app."
        );
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

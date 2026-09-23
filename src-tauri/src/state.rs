//! Shared shell state and the audio-frame bridge into the core.

use std::sync::{Arc, Mutex, MutexGuard};

use app_core::audio::{AudioHandle, AudioSink};
use app_core::session::machine::SessionManager;
use app_core::session::SessionId;
use app_core::store::{EffectReconciler, SettingsStore};
use app_core::AppError;

use crate::hotkey::HotkeyState;
use crate::window::{DebouncedSaver, ReloadLimiter};

pub struct AppState {
    pub settings: Mutex<SettingsStore>,
    pub sessions: Arc<SessionManager>,
    /// The one live loopback capture, tagged with the session it feeds.
    pub audio: Mutex<Option<ActiveCapture>>,
    pub hotkey: Mutex<HotkeyState>,
    /// Serializes and reconciles the OS side of settings: hotkey, window
    /// flag, dock (R5, ADR 016). Runs after every committed save.
    pub effects: EffectReconciler,
    pub bounds_saver: Arc<DebouncedSaver>,
    pub reload_limiter: Mutex<ReloadLimiter>,
}

pub struct ActiveCapture {
    pub session_id: SessionId,
    pub handle: Box<dyn AudioHandle>,
}

/// Lock that recovers from poisoning instead of panicking. A poisoned mutex
/// means some thread panicked mid-update; everything guarded here (settings
/// store, device handles, hotkey status) is valid at every intermediate step,
/// so recovering beats turning one panic into a cascade that takes every later
/// IPC command down with it.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Stop and drop the capture if it belongs to `session_id`. Guarded by id so a
/// stale release (an event from a superseded session arriving late) cannot
/// kill the capture a newer session is depending on.
pub fn release_capture_if(state: &AppState, session_id: SessionId) {
    let mut audio = lock(&state.audio);
    if audio.as_ref().is_some_and(|c| c.session_id == session_id) {
        if let Some(capture) = audio.take() {
            capture.handle.stop();
        }
    }
}

/// Forwards captured frames into the state machine.
pub struct ForwardingAudioSink {
    pub session_id: SessionId,
    pub sessions: Arc<SessionManager>,
}

impl AudioSink for ForwardingAudioSink {
    fn on_frame(&self, pcm: Vec<i16>, rms: f32) {
        // This runs on (or one hop from) the realtime capture thread; the push
        // must be the only work that happens here.
        self.sessions.push_audio(self.session_id, &pcm, rms);
    }

    fn on_error(&self, error: AppError) {
        // The device died. Routed through the machine rather than emitted
        // directly: the machine is phase-aware (§5.5's late-death rule applied
        // to audio — a device death after stop must not kill a streaming
        // answer), owns the one-error-per-session guarantee (§5.6), and drops
        // stale ids. Emitting from here bypassed all three.
        self.sessions.device_error(self.session_id, error);
    }
}

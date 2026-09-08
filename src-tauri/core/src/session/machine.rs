//! The session state machine (§5): ONE live question/answer pipeline at a
//! time, every dependency injected through `SessionDeps` so `cargo test`
//! drives every rule below with fakes — no network, no audio device.
//!
//! Every invariant here was purchased with a real bug in v2. The load-bearing
//! ones, in the order they bit:
//!
//! - **Supersession** (§5.1): `start`/`ask` aborts the active session, and
//!   every late event from the loser — including its `done`, including the
//!   socket death our own abort caused — is dropped by id.
//! - **Latest-start-wins** (§5.2): "newest" is claimed *before* the connect
//!   await, so a slow connect that resolves after losing tears its own stream
//!   down instead of installing itself over the winner.
//! - **Stop contract** (§5.3): `StopOutcome::NotTaken` emits nothing and is
//!   the only way the UI learns nothing is coming.
//! - **One error per stream, never after abort** (§5.5, §5.6): a stream death
//!   surfaces as exactly one error while the transcript still matters, and as
//!   nothing at all once it doesn't.
//! - **Slot release** (§5.11): whatever the outcome, the active slot is
//!   released exactly once, so the next session never supersedes a ghost.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot::error::TryRecvError;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::error::{AppError, ErrorCode, MSG_NO_SPEECH};
use crate::llm::{AnswerRequest, LlmProvider, LlmSink};
use crate::stt::{SttSink, SttStream};

use super::{limits, EventSink, Metrics, SessionDeps, SessionEvent, SessionId, StopOutcome};

/// The one-slot session registry. `Mutex<Option<Active>>` rather than a map:
/// the product rule is "one live pipeline", and encoding it in the type means
/// supersession cannot be forgotten on any path.
pub struct SessionManager {
    inner: Arc<Inner>,
}

struct Inner {
    slot: Mutex<Option<Active>>,
    next_id: AtomicU64,
}

/// Where the live session currently is. The phase decides what `stop` and
/// `push_audio` mean, so it lives inside the slot's lock — never in the
/// driver task, which learns about transitions strictly after they happened.
enum Phase {
    /// STT connect still in flight. No stream, no audio, `stop` is NotTaken.
    Connecting,
    /// Recording live. Audio routes here; `stop` is Taken exactly once.
    Recording { stream: Arc<dyn SttStream> },
    /// Stop accepted (or the question was typed): finalize/answer in flight.
    /// Audio is dropped — a late frame would race the CloseStream flush (§5.4).
    Finishing,
}

struct Active {
    id: SessionId,
    phase: Phase,
    /// Cancels every await this session's driver is parked on.
    cancel: CancellationToken,
    /// Suppresses this session's event emission the instant it stops mattering.
    gate: Arc<Gate>,
    /// Kept so `stop` can prewarm the LLM origin synchronously (§metrics) —
    /// that overlap with the finalize round-trip buys the ~1 s stop-to-first-word.
    llm: Arc<dyn LlmProvider>,
    /// Hands the driver the instant stop was requested. All metrics count from
    /// that instant, so it is captured where the request lands, not where the
    /// driver happens to resume.
    stop_tx: Option<oneshot::Sender<Instant>>,
}

/// Emission gate for one session. Killed on supersession, cancel, and error,
/// so a delta that races in after the session's fate was decided paints
/// nothing (§5.9) — and a superseded session's `done` never fires (§5.1).
struct Gate {
    id: SessionId,
    sink: Arc<dyn EventSink>,
    dead: AtomicBool,
}

impl Gate {
    fn new(id: SessionId, sink: Arc<dyn EventSink>) -> Arc<Self> {
        Arc::new(Self { id, sink, dead: AtomicBool::new(false) })
    }

    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }

    fn emit(&self, event: SessionEvent) {
        if !self.is_dead() {
            self.sink.emit(event);
        }
    }

    fn kill(&self) {
        self.dead.store(true, Ordering::Release);
    }
}

/// Dismantle a session that lost its slot (superseded or cancelled). Order
/// matters: the gate dies first so nothing the teardown itself provokes — the
/// socket death `abort` causes, a racing delta — can reach the UI (§5.1, §5.10).
fn teardown(active: Active) {
    active.gate.kill();
    active.cancel.cancel();
    if let Phase::Recording { stream } = &active.phase {
        stream.abort();
    }
    // Dropping `active` drops `stop_tx`, which unblocks a driver parked on the
    // stop signal so it can exit through its silent path.
}

impl Inner {
    fn claim_id(&self) -> SessionId {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn replace(&self, active: Active) -> Option<Active> {
        self.slot.lock().unwrap().replace(active)
    }

    /// Install the freshly connected stream — but only if this session is
    /// still the one in the slot (§5.2). A start that lost while connecting
    /// gets `false` and must tear its own stream down.
    fn install_stream(&self, id: SessionId, stream: Arc<dyn SttStream>) -> bool {
        let mut slot = self.slot.lock().unwrap();
        match slot.as_mut() {
            Some(active) if active.id == id && matches!(active.phase, Phase::Connecting) => {
                active.phase = Phase::Recording { stream };
                true
            }
            _ => false,
        }
    }

    /// Flip Recording -> Finishing for the auto-stop path (MAX_RECORDING).
    /// Returns false if the session already stopped, was cancelled, or lost.
    fn begin_finishing(&self, id: SessionId) -> bool {
        let mut slot = self.slot.lock().unwrap();
        match slot.as_mut() {
            Some(active) if active.id == id && matches!(active.phase, Phase::Recording { .. }) => {
                active.phase = Phase::Finishing;
                true
            }
            _ => false,
        }
    }

    /// The single point where a session leaves the slot on its own terms.
    /// Because it is keyed by id, every terminal path can call it and the slot
    /// is still released exactly once (§5.11) — a second call, or a call after
    /// supersession already emptied the slot, is a no-op.
    fn release_if_current(&self, id: SessionId) -> bool {
        let mut slot = self.slot.lock().unwrap();
        match slot.as_ref() {
            Some(active) if active.id == id => {
                *slot = None;
                true
            }
            _ => false,
        }
    }

    /// Terminal error path. Ownership of the slot is the once-only guarantee
    /// (§5.6): only the session that still holds the slot may speak, so a
    /// superseded session's socket death, or a second death of the same
    /// stream, emits nothing. `aborted` never surfaces — it is the pipeline's
    /// non-error way to unwind, and the UI must never render it.
    fn fail(&self, id: SessionId, gate: &Gate, error: AppError) {
        let owned = self.release_if_current(id);
        // Kill the gate before the error goes out so no delta paints after it
        // (§5.9), even one already in flight on another thread.
        gate.kill();
        if owned && !error.is_aborted() {
            gate.sink.emit(SessionEvent::SessionError { session_id: id, error });
        }
    }

    /// Terminal success path: release the slot, then emit `done` — but only if
    /// this session still owned the slot. A superseded session's `done` is the
    /// most misleading event there is (§5.1): it repaints an answer the user
    /// already abandoned.
    fn complete(&self, id: SessionId, gate: &Gate, event: SessionEvent) {
        if self.release_if_current(id) {
            gate.emit(event);
        }
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner { slot: Mutex::new(None), next_id: AtomicU64::new(0) }),
        }
    }

    /// Start a recording session: supersede whatever is live, then open the
    /// STT stream in the background. The id is returned before any network
    /// round-trip so the UI can label events immediately.
    ///
    /// The slot is claimed *before* the connect await (§5.2): a second Record
    /// press during the round-trip supersedes this one, and this one discovers
    /// it lost when its connect resolves.
    pub async fn start(&self, deps: SessionDeps) -> Result<SessionId, AppError> {
        let id = self.inner.claim_id();
        let cancel = CancellationToken::new();
        let gate = Gate::new(id, deps.events.clone());
        let (stop_tx, stop_rx) = oneshot::channel();

        let previous = self.inner.replace(Active {
            id,
            phase: Phase::Connecting,
            cancel: cancel.clone(),
            gate: gate.clone(),
            llm: deps.llm.clone(),
            stop_tx: Some(stop_tx),
        });
        if let Some(previous) = previous {
            teardown(previous);
        }

        // Prewarm on start: the TLS+TCP handshake overlaps the whole recording,
        // so by the time stop needs the origin it is already hot.
        deps.llm.prewarm();

        let inner = self.inner.clone();
        tokio::spawn(async move {
            drive_recording(inner, id, deps, cancel, gate, stop_rx).await;
        });
        Ok(id)
    }

    /// Stop the live recording and produce an answer. Taken exactly once per
    /// session; every NotTaken emits nothing (§5.3) — unknown id, already
    /// ended, already stopping, or still connecting all mean "nothing is
    /// coming", and this return value is the only channel that can say so.
    pub async fn stop(&self, id: SessionId) -> StopOutcome {
        let llm = {
            let mut slot = self.inner.slot.lock().unwrap();
            match slot.as_mut() {
                Some(active)
                    if active.id == id && matches!(active.phase, Phase::Recording { .. }) =>
                {
                    active.phase = Phase::Finishing;
                    // Send the stop instant while still holding the lock, so
                    // the driver can never observe Finishing without it.
                    if let Some(tx) = active.stop_tx.take() {
                        let _ = tx.send(Instant::now());
                    }
                    Some(active.llm.clone())
                }
                _ => None,
            }
        };
        match llm {
            Some(llm) => {
                // Prewarm on stop, before the finalize await runs anywhere:
                // the handshake overlaps the STT flush (§metrics).
                llm.prewarm();
                StopOutcome::Taken
            }
            None => StopOutcome::NotTaken,
        }
    }

    /// Answer a typed question. Validation happens BEFORE superseding (§5.8):
    /// garbage input must not kill a live recording. The trimmed question is
    /// then replayed as one final `SttPartial` so the UI renders typed and
    /// spoken questions through a single event shape, and `sttFinalizeMs` is
    /// exactly 0 — there was no STT stage and billing one would be a lie.
    pub async fn ask(&self, text: &str, deps: SessionDeps) -> Result<SessionId, AppError> {
        let question = text.trim().to_string();
        if question.is_empty() {
            return Err(AppError::internal("Type a question first."));
        }
        if question.chars().count() > limits::MAX_ASK_CHARS {
            return Err(AppError::internal(format!(
                "That question is too long — the limit is {} characters.",
                limits::MAX_ASK_CHARS
            )));
        }

        let id = self.inner.claim_id();
        let cancel = CancellationToken::new();
        let gate = Gate::new(id, deps.events.clone());

        let previous = self.inner.replace(Active {
            id,
            // A typed question was "stopped" the instant it was submitted:
            // stop must be NotTaken and audio must never route here.
            phase: Phase::Finishing,
            cancel: cancel.clone(),
            gate: gate.clone(),
            llm: deps.llm.clone(),
            stop_tx: None,
        });
        if let Some(previous) = previous {
            teardown(previous);
        }

        deps.llm.prewarm();

        // Metrics for a typed question count from submission — the moral
        // equivalent of the stop instant.
        let asked_at = Instant::now();
        let inner = self.inner.clone();
        tokio::spawn(async move {
            gate.emit(SessionEvent::SttPartial {
                session_id: id,
                text: question.clone(),
                is_final: true,
            });
            let request = deps.answer_request.with_transcript(question.as_str());
            run_answer(inner, id, deps.llm.clone(), request, question, cancel, gate, asked_at, 0)
                .await;
        });
        Ok(id)
    }

    /// Cancel is fire-and-forget and silent (§5.10): no done, no error, work
    /// aborted, slot released. Stale ids are ignored.
    pub fn cancel(&self, id: SessionId) {
        let removed = {
            let mut slot = self.inner.slot.lock().unwrap();
            match slot.as_ref() {
                Some(active) if active.id == id => slot.take(),
                _ => None,
            }
        };
        if let Some(active) = removed {
            teardown(active);
        }
    }

    /// Whether `id` currently owns the slot. The shell uses this to decide
    /// whether a freshly opened audio capture still has a session to feed —
    /// §5.2's "never install yourself over the winner", applied at the shell
    /// layer where the capture swap lives.
    pub fn is_active(&self, id: SessionId) -> bool {
        matches!(
            self.inner.slot.lock().unwrap().as_ref(),
            Some(active) if active.id == id
        )
    }

    /// Report a capture-device death (unplugged headset, disabled output).
    ///
    /// Phase-aware on purpose — the §5.5 rule for a late STT socket death,
    /// applied to audio: while the session still needs sound (connecting or
    /// recording), the device dying means the recording cannot continue and
    /// the session fails with the device error. Once stop was requested the
    /// audio stream's job is done — the capture stays physically open until
    /// the terminal event releases it, and a device death in that window must
    /// not kill an answer that is already streaming. Stale ids are ignored.
    pub fn device_error(&self, id: SessionId, error: AppError) {
        let target = {
            let slot = self.inner.slot.lock().unwrap();
            match slot.as_ref() {
                Some(active)
                    if active.id == id
                        && matches!(
                            active.phase,
                            Phase::Connecting | Phase::Recording { .. }
                        ) =>
                {
                    let stream = match &active.phase {
                        Phase::Recording { stream } => Some(stream.clone()),
                        _ => None,
                    };
                    Some((active.gate.clone(), active.cancel.clone(), stream))
                }
                _ => None,
            }
        };
        let Some((gate, cancel, stream)) = target else { return };
        // fail() first: it takes slot ownership exactly once, kills the gate,
        // and emits the one error (§5.6). The cancel/abort afterwards are pure
        // cleanup — the driver they wake exits through its silent paths.
        self.inner.fail(id, &gate, error);
        cancel.cancel();
        if let Some(stream) = stream {
            stream.abort();
        }
    }

    /// Route one audio frame. Accepted only for the live, not-yet-stopped
    /// session (§5.4): frames for stale ids are dropped, and frames arriving
    /// after stop was requested are dropped — they would race the CloseStream
    /// flush and smear the tail of one question into the next.
    pub fn push_audio(&self, id: SessionId, pcm: &[i16], rms: f32) {
        let target = {
            let slot = self.inner.slot.lock().unwrap();
            match slot.as_ref() {
                Some(active) if active.id == id => match &active.phase {
                    Phase::Recording { stream } => Some((stream.clone(), active.gate.clone())),
                    _ => None,
                },
                _ => None,
            }
        };
        if let Some((stream, gate)) = target {
            stream.send_audio(pcm);
            gate.emit(SessionEvent::AudioLevel { session_id: id, rms });
        }
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// What the STT stream pushes at us, funneled through one buffered channel so
/// an error that fires before the driver is listening — even inside the
/// connect call itself — is queued and delivered, never dropped (§5.6).
enum SttMsg {
    Transcript { text: String, is_final: bool },
    Error(AppError),
}

struct SessionSttSink {
    tx: mpsc::UnboundedSender<SttMsg>,
    /// The stream contract says at most one `on_error`; this makes it true
    /// even against a misbehaving implementation (§5.6).
    errored: AtomicBool,
}

impl SttSink for SessionSttSink {
    fn on_transcript(&self, text: String, is_final: bool) {
        let _ = self.tx.send(SttMsg::Transcript { text, is_final });
    }

    fn on_error(&self, error: AppError) {
        if !self.errored.swap(true, Ordering::AcqRel) {
            let _ = self.tx.send(SttMsg::Error(error));
        }
    }
}

/// Answer deltas flow through the gate (suppressed after any terminal event),
/// and the first one timestamps itself and disarms the first-token timeout.
struct SessionLlmSink {
    gate: Arc<Gate>,
    first: Mutex<Option<oneshot::Sender<Instant>>>,
}

impl LlmSink for SessionLlmSink {
    fn on_delta(&self, delta: String) {
        // A delta racing in after the session died must neither paint (§5.9)
        // nor disarm a timeout that already fired.
        if self.gate.is_dead() {
            return;
        }
        if let Some(tx) = self.first.lock().unwrap().take() {
            let _ = tx.send(Instant::now());
        }
        self.gate.emit(SessionEvent::LlmDelta { session_id: self.gate.id, delta });
    }
}

fn ms_between(from: Instant, to: Instant) -> u64 {
    to.duration_since(from).as_millis() as u64
}

fn ms_since(from: Instant) -> u64 {
    ms_between(from, Instant::now())
}

/// Drives one recording session: connect, record, finalize, answer. Runs as a
/// detached task; every exit path either emitted a terminal event through the
/// slot-ownership check or was silenced by losing the slot first.
async fn drive_recording(
    inner: Arc<Inner>,
    id: SessionId,
    deps: SessionDeps,
    cancel: CancellationToken,
    gate: Arc<Gate>,
    mut stop_rx: oneshot::Receiver<Instant>,
) {
    // ---- Connect -----------------------------------------------------------
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink: Arc<dyn SttSink> =
        Arc::new(SessionSttSink { tx, errored: AtomicBool::new(false) });

    // Deliberately NOT racing the cancel token here: a superseded connect is
    // allowed to finish its round-trip so that it can tear down the socket it
    // opened. The loss is discovered at install time (§5.2).
    let connected = tokio::time::timeout(limits::STT_CONNECT, deps.stt.connect(sink)).await;
    let stream: Arc<dyn SttStream> = match connected {
        Ok(Ok(stream)) => Arc::from(stream),
        Ok(Err(error)) => {
            // Silent if we already lost the slot — the failure of a session
            // nobody is watching is not news.
            inner.fail(id, &gate, error);
            return;
        }
        Err(_elapsed) => {
            inner.fail(
                id,
                &gate,
                AppError::new(
                    ErrorCode::SttConnect,
                    "Could not reach the transcription service. Check your network and try again.",
                ),
            );
            return;
        }
    };

    // Latest-start-wins (§5.2): if a newer start claimed the slot while we
    // were connecting, this stream must die by our own hand, silently — it
    // must never install itself over the winner or receive one frame of audio.
    if !inner.install_stream(id, stream.clone()) {
        stream.abort();
        return;
    }

    // ---- Record ------------------------------------------------------------
    let max_recording = tokio::time::sleep(limits::MAX_RECORDING);
    tokio::pin!(max_recording);
    let mut stt_open = true;

    let stopped_at = loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                // Superseded or cancelled mid-recording: silent (§5.1, §5.10).
                stream.abort();
                return;
            }
            stop = &mut stop_rx => match stop {
                Ok(at) => break at,
                // The sender only drops when the slot tore this session down;
                // exit through the same silent path as an explicit cancel.
                Err(_) => {
                    stream.abort();
                    return;
                }
            },
            msg = rx.recv(), if stt_open => match msg {
                Some(SttMsg::Transcript { text, is_final }) => {
                    gate.emit(SessionEvent::SttPartial { session_id: id, text, is_final });
                }
                Some(SttMsg::Error(error)) => {
                    // Mid-recording stream death (§5.5): one error, torn down.
                    // A silently truncated transcript answers the wrong question.
                    stream.abort();
                    inner.fail(id, &gate, error);
                    return;
                }
                None => stt_open = false,
            },
            _ = &mut max_recording => {
                // 120 s hard cap (§5.9): behave exactly like a user stop —
                // flip the slot to Finishing, prewarm, and answer normally.
                match stop_rx.try_recv() {
                    Ok(at) => break at,
                    Err(TryRecvError::Closed) => {
                        stream.abort();
                        return;
                    }
                    Err(TryRecvError::Empty) => {
                        if inner.begin_finishing(id) {
                            deps.llm.prewarm();
                            break Instant::now();
                        }
                        // Lost a race with a stop accepted this very instant;
                        // the instant was sent under the lock, so it is here.
                        match stop_rx.try_recv() {
                            Ok(at) => break at,
                            Err(_) => {
                                stream.abort();
                                return;
                            }
                        }
                    }
                }
            }
        }
    };

    // ---- Finalize ------------------------------------------------------------
    // The stop instant is time zero for every metric (§metrics).
    let finalize_deadline = stopped_at + limits::STT_FINALIZE;
    let mut finalize = stream.finalize();

    let transcript = loop {
        tokio::select! {
            // `biased` with the STT channel polled BEFORE the finalize future:
            // a stream that died mid-flush queues its error and then resolves
            // the finalize with a truncated transcript in the same instant.
            // Under an unbiased select the truncated text wins half the time
            // and the app answers the wrong question (§5.5); polling the
            // channel first makes the queued error always win.
            biased;
            _ = cancel.cancelled() => {
                stream.abort();
                return;
            }
            msg = rx.recv(), if stt_open => match msg {
                Some(SttMsg::Transcript { text, is_final }) => {
                    gate.emit(SessionEvent::SttPartial { session_id: id, text, is_final });
                }
                Some(SttMsg::Error(error)) => {
                    // Mid-finalize death (§5.5): still one honest stt error —
                    // NOT "no speech", which would send the user debugging
                    // their audio setup for a socket problem.
                    stream.abort();
                    inner.fail(id, &gate, error);
                    return;
                }
                None => stt_open = false,
            },
            result = &mut finalize => match result {
                Ok(text) => break text,
                Err(error) => {
                    stream.abort();
                    inner.fail(id, &gate, error);
                    return;
                }
            },
            _ = tokio::time::sleep_until(finalize_deadline) => {
                // §5.9: the finalize cap has its own code so the UI can say
                // what actually happened.
                stream.abort();
                inner.fail(
                    id,
                    &gate,
                    AppError::new(
                        ErrorCode::SttTimeout,
                        "Timed out waiting for the final transcript. Try again.",
                    ),
                );
                return;
            }
        }
    };
    let stt_finalize_ms = ms_since(stopped_at);

    // The transcript is final: the STT stream's job is done. Dropping the
    // receiver here is what makes a LATE socket close harmless (§5.5) — it can
    // no longer reach a session whose answer is already streaming.
    drop(rx);

    let transcript = transcript.trim().to_string();
    if transcript.is_empty() {
        // §5.7: never an LLM call on an empty prompt, and the exact canonical
        // message — this is the one error a user can fix themselves.
        inner.fail(id, &gate, AppError::new(ErrorCode::NoSpeech, MSG_NO_SPEECH));
        return;
    }

    let request = deps.answer_request.with_transcript(transcript.as_str());
    run_answer(
        inner,
        id,
        deps.llm.clone(),
        request,
        transcript,
        cancel,
        gate,
        stopped_at,
        stt_finalize_ms,
    )
    .await;
}

/// Streams the answer and applies the LLM timeouts (§5.9). Shared verbatim by
/// the recorded and typed paths — the only differences the UI can observe are
/// the transcript's origin and `sttFinalizeMs`.
#[allow(clippy::too_many_arguments)]
async fn run_answer(
    inner: Arc<Inner>,
    id: SessionId,
    llm: Arc<dyn LlmProvider>,
    request: AnswerRequest,
    transcript: String,
    cancel: CancellationToken,
    gate: Arc<Gate>,
    stopped_at: Instant,
    stt_finalize_ms: u64,
) {
    let (first_tx, mut first_rx) = oneshot::channel();
    let sink: Arc<dyn LlmSink> =
        Arc::new(SessionLlmSink { gate: gate.clone(), first: Mutex::new(Some(first_tx)) });

    let mut answer = llm.stream_answer(&request, sink, cancel.clone());

    let local = llm.kind() == crate::llm::LlmProviderKind::Local;
    let first_token_deadline = stopped_at + if local { limits::LOCAL_FIRST_TOKEN } else { limits::LLM_FIRST_TOKEN };
    let total_deadline = stopped_at + if local { limits::LOCAL_TOTAL } else { limits::LLM_TOTAL };
    let mut first_token_at: Option<Instant> = None;
    let mut first_armed = true;

    let result = loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                // Superseded or cancelled mid-answer: silent (§5.1, §5.10).
                gate.kill();
                return;
            }
            result = &mut answer => break result,
            first = &mut first_rx, if first_armed => {
                first_armed = false;
                // The first delta both timestamps the headline metric and
                // DISARMS the first-token timeout (§5.9): once tokens flow,
                // only the total cap applies.
                if let Ok(at) = first {
                    first_token_at = Some(at);
                }
            }
            _ = tokio::time::sleep_until(first_token_deadline), if first_token_at.is_none() => {
                cancel.cancel();
                inner.fail(
                    id,
                    &gate,
                    AppError::new(
                        ErrorCode::LlmFirstTokenTimeout,
                        "The model did not start answering in time. Try again.",
                    ),
                );
                return;
            }
            _ = tokio::time::sleep_until(total_deadline) => {
                cancel.cancel();
                inner.fail(
                    id,
                    &gate,
                    AppError::new(
                        ErrorCode::LlmTimeout,
                        "The answer took too long and was stopped. Try again.",
                    ),
                );
                return;
            }
        }
    };

    match result {
        Ok(answer) => {
            let total_ms = ms_since(stopped_at);
            let first_token_ms = first_token_at.map(|at| ms_between(stopped_at, at));
            let metrics = Metrics::finish(stt_finalize_ms, first_token_ms, total_ms);
            inner.complete(
                id,
                &gate,
                SessionEvent::LlmDone { session_id: id, transcript, answer, metrics },
            );
        }
        // Our own cancellation surfacing back through the provider — the abort
        // we caused must never be reported as an error (§5.1).
        Err(error) if error.is_aborted() => {}
        Err(error) => inner.fail(id, &gate, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use async_trait::async_trait;

    use crate::error::AppResult;
    use crate::llm::{build_system_prompt, AnswerStyle, LlmProviderKind, Profile};
    use crate::stt::SttConnector;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// Let already-spawned tasks run without moving the paused clock.
    async fn settle() {
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
    }

    /// Move the paused clock forward and let woken tasks run to quiescence.
    ///
    /// Implemented as a sleep, not `tokio::time::advance`: with the clock
    /// paused, a sleeping test auto-advances to each nearest timer IN ORDER,
    /// running the tasks it wakes at the correct virtual instant — which is
    /// exactly what the timestamp-sensitive metric tests need. A raw `advance`
    /// would leap the full duration first and stamp every intermediate event
    /// with the final time.
    async fn advance(d: Duration) {
        tokio::time::sleep(d).await;
        settle().await;
    }

    // ---- Event sink ---------------------------------------------------------

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<SessionEvent>>,
    }

    impl EventSink for RecordingSink {
        fn emit(&self, event: SessionEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    impl RecordingSink {
        fn for_id(&self, id: SessionId) -> Vec<SessionEvent> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.session_id() == id)
                .cloned()
                .collect()
        }

        fn kinds_for(&self, id: SessionId) -> Vec<&'static str> {
            self.for_id(id).iter().map(|e| e.event_name()).collect()
        }

        fn errors_for(&self, id: SessionId) -> Vec<AppError> {
            self.for_id(id)
                .into_iter()
                .filter_map(|e| match e {
                    SessionEvent::SessionError { error, .. } => Some(error),
                    _ => None,
                })
                .collect()
        }

        fn error_codes(&self, id: SessionId) -> Vec<ErrorCode> {
            self.errors_for(id).iter().map(|e| e.code).collect()
        }

        fn deltas_for(&self, id: SessionId) -> Vec<String> {
            self.for_id(id)
                .into_iter()
                .filter_map(|e| match e {
                    SessionEvent::LlmDelta { delta, .. } => Some(delta),
                    _ => None,
                })
                .collect()
        }

        fn partials_for(&self, id: SessionId) -> Vec<(String, bool)> {
            self.for_id(id)
                .into_iter()
                .filter_map(|e| match e {
                    SessionEvent::SttPartial { text, is_final, .. } => Some((text, is_final)),
                    _ => None,
                })
                .collect()
        }

        fn levels_for(&self, id: SessionId) -> Vec<f32> {
            self.for_id(id)
                .into_iter()
                .filter_map(|e| match e {
                    SessionEvent::AudioLevel { rms, .. } => Some(rms),
                    _ => None,
                })
                .collect()
        }

        fn done_for(&self, id: SessionId) -> Option<(String, String, Metrics)> {
            self.for_id(id).into_iter().find_map(|e| match e {
                SessionEvent::LlmDone { transcript, answer, metrics, .. } => {
                    Some((transcript, answer, metrics))
                }
                _ => None,
            })
        }
    }

    // ---- Scripted STT fake --------------------------------------------------

    #[derive(Default)]
    struct StreamHandle {
        aborted: AtomicBool,
        frames: Mutex<Vec<Vec<i16>>>,
        finalize_calls: AtomicUsize,
    }

    impl StreamHandle {
        fn aborted(&self) -> bool {
            self.aborted.load(Ordering::SeqCst)
        }

        fn frames(&self) -> Vec<Vec<i16>> {
            self.frames.lock().unwrap().clone()
        }

        fn finalize_calls(&self) -> usize {
            self.finalize_calls.load(Ordering::SeqCst)
        }
    }

    struct StreamScript {
        finalize_delay: Duration,
        finalize_result: AppResult<String>,
    }

    struct ConnectScript {
        delay: Duration,
        result: Result<StreamScript, AppError>,
        /// Pushed into the sink synchronously inside `connect`, before the
        /// stream is even returned — the "error before the handler registered"
        /// case of §5.6.
        error_before_return: Option<AppError>,
    }

    impl ConnectScript {
        fn ok(transcript: &str) -> Self {
            Self {
                delay: Duration::ZERO,
                result: Ok(StreamScript {
                    finalize_delay: Duration::ZERO,
                    finalize_result: Ok(transcript.to_string()),
                }),
                error_before_return: None,
            }
        }

        fn fail(error: AppError) -> Self {
            Self { delay: Duration::ZERO, result: Err(error), error_before_return: None }
        }

        fn connect_delay(mut self, d: Duration) -> Self {
            self.delay = d;
            self
        }

        fn finalize_delay(mut self, d: Duration) -> Self {
            if let Ok(script) = &mut self.result {
                script.finalize_delay = d;
            }
            self
        }

        fn dead_on_arrival(mut self, error: AppError) -> Self {
            self.error_before_return = Some(error);
            self
        }
    }

    struct FakeSttStream {
        handle: Arc<StreamHandle>,
        script: StreamScript,
    }

    #[async_trait]
    impl SttStream for FakeSttStream {
        fn send_audio(&self, pcm: &[i16]) {
            self.handle.frames.lock().unwrap().push(pcm.to_vec());
        }

        async fn finalize(&self) -> AppResult<String> {
            self.handle.finalize_calls.fetch_add(1, Ordering::SeqCst);
            if self.script.finalize_delay > Duration::ZERO {
                tokio::time::sleep(self.script.finalize_delay).await;
            }
            self.script.finalize_result.clone()
        }

        fn abort(&self) {
            self.handle.aborted.store(true, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct FakeSttConnector {
        scripts: Mutex<VecDeque<ConnectScript>>,
        /// Sinks and stream handles in connect-RESOLUTION order, so tests can
        /// drive a live stream (emit transcripts, kill the socket) directly.
        sinks: Mutex<Vec<Arc<dyn SttSink>>>,
        streams: Mutex<Vec<Arc<StreamHandle>>>,
    }

    impl FakeSttConnector {
        fn push(&self, script: ConnectScript) {
            self.scripts.lock().unwrap().push_back(script);
        }

        fn sink(&self, i: usize) -> Arc<dyn SttSink> {
            self.sinks.lock().unwrap()[i].clone()
        }

        fn stream(&self, i: usize) -> Arc<StreamHandle> {
            self.streams.lock().unwrap()[i].clone()
        }
    }

    #[async_trait]
    impl SttConnector for FakeSttConnector {
        async fn connect(&self, sink: Arc<dyn SttSink>) -> AppResult<Box<dyn SttStream>> {
            // Unscripted connects succeed with a boring transcript so tests
            // that don't care about STT stay terse.
            let script = self
                .scripts
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| ConnectScript::ok("the question"));
            if script.delay > Duration::ZERO {
                tokio::time::sleep(script.delay).await;
            }
            if let Some(error) = script.error_before_return {
                sink.on_error(error);
            }
            self.sinks.lock().unwrap().push(sink);
            match script.result {
                Ok(stream_script) => {
                    let handle = Arc::new(StreamHandle::default());
                    self.streams.lock().unwrap().push(handle.clone());
                    Ok(Box::new(FakeSttStream { handle, script: stream_script }))
                }
                Err(error) => Err(error),
            }
        }
    }

    // ---- Scripted LLM fake --------------------------------------------------

    enum LlmScript {
        /// Push each delta after its delay (cancel-aware, like a real SSE
        /// read loop), then resolve with the concatenation.
        Stream(Vec<(Duration, &'static str)>),
        /// The whole answer in one shot: no deltas ever.
        Full(Duration, &'static str),
        Fail(Duration, AppError),
        /// Deltas pushed from a DETACHED task while `stream_answer` itself
        /// parks on the cancel token — models a socket task that outlives the
        /// pipeline's interest, for the suppression tests (§5.9).
        DetachedDeltas(Vec<(Duration, &'static str)>),
    }

    #[derive(Default)]
    struct FakeLlm {
        scripts: Mutex<VecDeque<LlmScript>>,
        prewarms: AtomicUsize,
        calls: Mutex<Vec<String>>,
    }

    impl FakeLlm {
        fn push(&self, script: LlmScript) {
            self.scripts.lock().unwrap().push_back(script);
        }

        fn prewarms(&self) -> usize {
            self.prewarms.load(Ordering::SeqCst)
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LlmProvider for FakeLlm {
        async fn stream_answer(
            &self,
            req: &AnswerRequest,
            sink: Arc<dyn LlmSink>,
            cancel: CancellationToken,
        ) -> AppResult<String> {
            self.calls.lock().unwrap().push(req.transcript.clone());
            let script = self
                .scripts
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(LlmScript::Stream(vec![(Duration::ZERO, "ok")]));
            match script {
                LlmScript::Stream(deltas) => {
                    let mut full = String::new();
                    for (delay, delta) in deltas {
                        if delay > Duration::ZERO {
                            tokio::select! {
                                _ = cancel.cancelled() => return Err(AppError::aborted()),
                                _ = tokio::time::sleep(delay) => {}
                            }
                        }
                        sink.on_delta(delta.to_string());
                        full.push_str(delta);
                    }
                    Ok(full)
                }
                LlmScript::Full(delay, answer) => {
                    if delay > Duration::ZERO {
                        tokio::select! {
                            _ = cancel.cancelled() => return Err(AppError::aborted()),
                            _ = tokio::time::sleep(delay) => {}
                        }
                    }
                    Ok(answer.to_string())
                }
                LlmScript::Fail(delay, error) => {
                    if delay > Duration::ZERO {
                        tokio::time::sleep(delay).await;
                    }
                    Err(error)
                }
                LlmScript::DetachedDeltas(deltas) => {
                    tokio::spawn(async move {
                        for (delay, delta) in deltas {
                            tokio::time::sleep(delay).await;
                            sink.on_delta(delta.to_string());
                        }
                    });
                    cancel.cancelled().await;
                    Err(AppError::aborted())
                }
            }
        }

        fn prewarm(&self) {
            self.prewarms.fetch_add(1, Ordering::SeqCst);
        }

        fn kind(&self) -> LlmProviderKind {
            LlmProviderKind::Anthropic
        }
    }

    // ---- Harness ------------------------------------------------------------

    struct Harness {
        manager: SessionManager,
        stt: Arc<FakeSttConnector>,
        llm: Arc<FakeLlm>,
        events: Arc<RecordingSink>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                manager: SessionManager::new(),
                stt: Arc::new(FakeSttConnector::default()),
                llm: Arc::new(FakeLlm::default()),
                events: Arc::new(RecordingSink::default()),
            }
        }

        fn deps(&self) -> SessionDeps {
            SessionDeps {
                stt: self.stt.clone(),
                llm: self.llm.clone(),
                answer_request: AnswerRequest::new(build_system_prompt(
                    Profile::default(),
                    AnswerStyle::Balanced,
                )),
                events: self.events.clone(),
            }
        }

        async fn start(&self) -> SessionId {
            self.manager.start(self.deps()).await.expect("start")
        }

        async fn ask(&self, text: &str) -> Result<SessionId, AppError> {
            self.manager.ask(text, self.deps()).await
        }
    }

    // ---- Happy path ---------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn recording_happy_path_streams_partials_deltas_then_done() {
        // WHY: the baseline every other test degrades from — if the plain
        // record/stop/answer flow misorders its events, nothing else matters.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("What is ownership?"));
        h.llm.push(LlmScript::Stream(vec![
            (Duration::ZERO, "Ownership "),
            (Duration::ZERO, "is moves."),
        ]));

        let id = h.start().await;
        settle().await;
        h.stt.sink(0).on_transcript("What is".into(), false);
        h.stt.sink(0).on_transcript("What is ownership?".into(), true);
        settle().await;
        assert_eq!(
            h.events.partials_for(id),
            vec![("What is".to_string(), false), ("What is ownership?".to_string(), true)]
        );

        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        settle().await;

        assert_eq!(
            h.events.kinds_for(id),
            vec!["stt:partial", "stt:partial", "llm:delta", "llm:delta", "llm:done"]
        );
        let (transcript, answer, _) = h.events.done_for(id).expect("done");
        assert_eq!(transcript, "What is ownership?");
        assert_eq!(answer, "Ownership is moves.");
        assert!(h.events.errors_for(id).is_empty());
    }

    // ---- §5.1 supersession --------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn start_supersedes_live_recording_and_its_late_events_are_dropped() {
        // WHY: v2 painted a superseded session's transcript over the new one,
        // and reported the socket death its own abort caused as an error.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("one"));
        h.stt.push(ConnectScript::ok("two"));

        let a = h.start().await;
        settle().await;
        h.stt.sink(0).on_transcript("hel".into(), false);
        settle().await;
        assert_eq!(h.events.partials_for(a).len(), 1);

        let b = h.start().await;
        settle().await;
        assert!(h.stt.stream(0).aborted(), "superseding must tear the old stream down");

        // The old stream now speaks from beyond the grave: a late transcript,
        // then the socket death our abort caused. All of it must be dropped.
        let before = h.events.for_id(a).len();
        h.stt.sink(0).on_transcript("late words".into(), true);
        h.stt.sink(0).on_error(AppError::new(ErrorCode::SttError, "socket closed by abort"));
        settle().await;
        assert_eq!(h.events.for_id(a).len(), before, "superseded session must be fully silent");
        assert!(h.events.errors_for(a).is_empty());

        // And the winner is genuinely live.
        assert_eq!(h.manager.stop(b).await, StopOutcome::Taken);
        settle().await;
        assert!(h.events.done_for(b).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn superseded_sessions_done_is_never_emitted() {
        // WHY: a late `done` from a superseded session repaints an answer the
        // user already abandoned — the single most confusing failure in v2.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("first question"));
        h.llm.push(LlmScript::Stream(vec![(Duration::ZERO, "first"), (secs(10), " half")]));
        h.stt.push(ConnectScript::ok("second question"));

        let a = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(a).await, StopOutcome::Taken);
        settle().await;
        assert_eq!(h.events.deltas_for(a), vec!["first".to_string()], "answer is mid-stream");

        let b = h.start().await;
        settle().await;
        advance(secs(10)).await;

        assert!(h.events.done_for(a).is_none(), "superseded session's done must be dropped");
        assert!(h.events.errors_for(a).is_empty(), "and its abort must be silent");
        let _ = b;
    }

    // ---- §5.2 latest-start-wins ---------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn double_record_while_first_is_connecting_latest_start_wins() {
        // WHY: connect is a network round-trip; a second Record press during
        // it must win even though the first connect resolves later. The loser
        // once installed itself over the winner and swallowed its audio.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("first").connect_delay(ms(300)));
        h.stt.push(ConnectScript::ok("second").connect_delay(ms(50)));

        let a = h.start().await;
        settle().await;
        let b = h.start().await;
        settle().await;

        advance(ms(50)).await; // B connects and installs.
        advance(ms(250)).await; // A's connect resolves and discovers it lost.

        // Streams are recorded in resolution order: B's first, then A's.
        let b_stream = h.stt.stream(0);
        let a_stream = h.stt.stream(1);
        assert!(a_stream.aborted(), "the losing start must tear down its own stream");
        assert!(!b_stream.aborted());

        h.manager.push_audio(b, &[1, 2], 0.4);
        h.manager.push_audio(a, &[9, 9], 0.9);
        assert_eq!(b_stream.frames(), vec![vec![1, 2]]);
        assert!(a_stream.frames().is_empty(), "audio must never reach the loser");
        assert!(h.events.for_id(a).is_empty(), "the loser reports nothing, not even aborted");

        assert_eq!(h.manager.stop(b).await, StopOutcome::Taken);
        settle().await;
        assert!(h.events.done_for(b).is_some());
    }

    // ---- §5.3 stop contract -------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn stop_unknown_id_is_not_taken() {
        // WHY: a stop against nothing must say so via the return value —
        // emitting anything would animate a session that does not exist.
        let h = Harness::new();
        assert_eq!(h.manager.stop(42).await, StopOutcome::NotTaken);
        assert!(h.events.for_id(42).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn stop_while_connecting_is_not_taken_and_session_survives() {
        // WHY: there is no stream to finalize yet. Accepting the stop would
        // strand the UI in "Finalizing…" with no event ever coming.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q").connect_delay(ms(500)));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::NotTaken);
        assert!(h.events.for_id(id).is_empty(), "NotTaken must emit nothing");

        // The session was not harmed: once connected it stops normally.
        advance(ms(500)).await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        settle().await;
        assert!(h.events.done_for(id).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn second_stop_during_finalize_is_not_taken_and_finalize_runs_once() {
        // WHY: a double-tapped Stop once started a second finalize that raced
        // the first for the same socket.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q").finalize_delay(ms(200)));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        assert_eq!(h.manager.stop(id).await, StopOutcome::NotTaken);
        advance(ms(200)).await;

        assert!(h.events.done_for(id).is_some());
        assert_eq!(h.stt.stream(0).finalize_calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_after_done_is_not_taken() {
        // WHY: "already ended" is one of the silent NotTaken cases — the UI
        // uses the return value, not a ghost event, to reset.
        let h = Harness::new();
        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        settle().await;
        assert!(h.events.done_for(id).is_some());

        let before = h.events.for_id(id).len();
        assert_eq!(h.manager.stop(id).await, StopOutcome::NotTaken);
        assert_eq!(h.events.for_id(id).len(), before);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_after_error_teardown_is_not_taken() {
        // WHY: the error already ended the session; a stop that pretended to
        // take would promise a finalize that can never happen.
        let h = Harness::new();
        let id = h.start().await;
        settle().await;
        h.stt.sink(0).on_error(AppError::new(ErrorCode::SttError, "socket died"));
        settle().await;
        assert_eq!(h.events.error_codes(id), vec![ErrorCode::SttError]);

        assert_eq!(h.manager.stop(id).await, StopOutcome::NotTaken);
    }

    // ---- §5.4 audio routing -------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn audio_routes_to_live_session_and_emits_level() {
        // WHY: push_audio is the level meter's only source; a frame accepted
        // without its AudioLevel leaves the meter dead while recording.
        let h = Harness::new();
        let id = h.start().await;
        settle().await;

        h.manager.push_audio(id, &[1, 2, 3], 0.5);
        assert_eq!(h.stt.stream(0).frames(), vec![vec![1, 2, 3]]);
        assert_eq!(h.events.levels_for(id), vec![0.5]);
    }

    #[tokio::test(start_paused = true)]
    async fn audio_after_stop_requested_is_dropped() {
        // WHY: a frame sent after stop races the CloseStream flush and smears
        // the tail of this question into the transcript of the next.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q").finalize_delay(ms(200)));
        let id = h.start().await;
        settle().await;
        h.manager.push_audio(id, &[1], 0.2);
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);

        h.manager.push_audio(id, &[2], 0.9);
        assert_eq!(h.stt.stream(0).frames(), vec![vec![1]], "post-stop frames must be dropped");
        assert_eq!(h.events.levels_for(id), vec![0.2], "and no level event for them");
    }

    #[tokio::test(start_paused = true)]
    async fn audio_for_stale_or_connecting_ids_is_dropped() {
        // WHY: frames for a session that is not the live recording — still
        // connecting, or simply not the current id — must vanish, not misroute.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q").connect_delay(ms(100)));
        let id = h.start().await;
        settle().await;

        // Still connecting: no stream exists to receive this.
        h.manager.push_audio(id, &[7], 0.7);
        assert!(h.events.levels_for(id).is_empty());

        advance(ms(100)).await;
        // A stale id after connect: dropped by id.
        h.manager.push_audio(id + 999, &[8], 0.8);
        assert!(h.stt.stream(0).frames().is_empty());
        assert!(h.events.levels_for(id + 999).is_empty());
    }

    // ---- device errors (§5.5's rule applied to the capture) -----------------

    #[tokio::test(start_paused = true)]
    async fn device_error_while_recording_fails_the_session_once() {
        // WHY: an unplugged headset mid-recording means the recording cannot
        // continue; the user must get one honest device error, not a dead
        // level meter followed by a misleading no_speech at stop.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q"));
        let id = h.start().await;
        settle().await;

        h.manager.device_error(id, AppError::internal("No audio playback device is active."));
        settle().await;

        let errors = h.events.errors_for(id);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("playback device"));
        assert!(h.stt.stream(0).aborted(), "the STT stream must be torn down with the session");
        // Slot released exactly once: a stop now finds nothing.
        assert_eq!(h.manager.stop(id).await, StopOutcome::NotTaken);
    }

    #[tokio::test(start_paused = true)]
    async fn device_error_after_stop_does_not_kill_the_streaming_answer() {
        // WHY: once stop was requested the audio stream's job is done — the
        // capture stays physically open until the terminal event releases it,
        // and a Bluetooth dropout in that window must not destroy an answer
        // that is already on its way (the §5.5 late-socket rule, for audio).
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q").finalize_delay(ms(300)));
        h.llm.push(LlmScript::Stream(vec![(ms(100), "answer.")]));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);

        // Device dies while the finalize (and then the answer) is in flight.
        h.manager.device_error(id, AppError::internal("device gone"));
        advance(ms(600)).await;

        assert!(h.events.errors_for(id).is_empty(), "no error for a post-stop device death");
        let (_, answer, _) = h.events.done_for(id).expect("the answer must still complete");
        assert_eq!(answer, "answer.");
    }

    #[tokio::test(start_paused = true)]
    async fn device_error_for_a_stale_id_is_ignored() {
        // WHY: a device error from a superseded session's capture arriving
        // late must not touch the winner.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q"));
        let id = h.start().await;
        settle().await;

        h.manager.device_error(id + 999, AppError::internal("stale"));
        settle().await;
        assert!(h.events.errors_for(id).is_empty());
        assert!(h.events.errors_for(id + 999).is_empty());
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
    }

    #[tokio::test(start_paused = true)]
    async fn is_active_tracks_slot_ownership_across_supersession_and_completion() {
        // WHY: the shell installs the audio capture only for the session that
        // still owns the slot; a wrong answer here either steals the winner's
        // capture or strands the live session without audio (§5.2).
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("one"));
        h.stt.push(ConnectScript::ok("two"));
        h.llm.push(LlmScript::Stream(vec![(Duration::ZERO, "a")]));

        let a = h.start().await;
        settle().await;
        assert!(h.manager.is_active(a));

        let b = h.start().await;
        settle().await;
        assert!(!h.manager.is_active(a), "superseded sessions are not active");
        assert!(h.manager.is_active(b));

        assert_eq!(h.manager.stop(b).await, StopOutcome::Taken);
        settle().await;
        assert!(h.events.done_for(b).is_some());
        assert!(!h.manager.is_active(b), "completion releases the slot");
    }

    // ---- §5.5 / §5.6 STT error policy ---------------------------------------

    #[tokio::test(start_paused = true)]
    async fn mid_recording_stream_death_surfaces_one_stt_error_and_tears_down() {
        // WHY: a silently truncated transcript answers the wrong question, so
        // the death must surface — but exactly once, even if the stream
        // misbehaves and reports twice.
        let h = Harness::new();
        let id = h.start().await;
        settle().await;

        h.stt.sink(0).on_error(AppError::new(ErrorCode::SttError, "socket died"));
        h.stt.sink(0).on_error(AppError::new(ErrorCode::SttError, "socket died again"));
        settle().await;

        assert_eq!(h.events.error_codes(id), vec![ErrorCode::SttError]);
        assert!(h.stt.stream(0).aborted());
        assert!(h.events.done_for(id).is_none());
        assert!(h.llm.calls().is_empty(), "a dead stream must never reach the LLM");
    }

    #[tokio::test(start_paused = true)]
    async fn mid_finalize_stream_death_surfaces_stt_error() {
        // WHY: the flush window is where sockets actually die. Swallowing the
        // death here leaves the UI in "Finalizing…" forever.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q").finalize_delay(secs(3600)));
        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        settle().await;

        h.stt.sink(0).on_error(AppError::new(ErrorCode::SttError, "died during flush"));
        settle().await;

        assert_eq!(h.events.error_codes(id), vec![ErrorCode::SttError]);
        assert!(h.events.done_for(id).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn late_socket_death_after_finalize_does_not_kill_streaming_answer() {
        // WHY: once the transcript is final the STT stream's job is done. v2
        // let a late close tear down an answer already painting on screen.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("What is Rust?"));
        h.llm.push(LlmScript::Stream(vec![(ms(100), "Rust "), (ms(100), "is a language.")]));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        settle().await; // finalize (instant) done; answer streaming.

        h.stt.sink(0).on_error(AppError::new(ErrorCode::SttError, "late close"));
        settle().await;
        advance(ms(200)).await;

        let (_, answer, _) = h.events.done_for(id).expect("late death must not kill the answer");
        assert_eq!(answer, "Rust is a language.");
        assert!(h.events.errors_for(id).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn error_before_handler_registration_is_buffered_and_delivered() {
        // WHY: a stream can die inside the connect call, before anything is
        // listening. That early death must be queued and reported, not lost —
        // otherwise the UI records into a socket that no longer exists.
        let h = Harness::new();
        h.stt.push(
            ConnectScript::ok("q")
                .dead_on_arrival(AppError::new(ErrorCode::SttError, "died during connect")),
        );

        let id = h.start().await;
        settle().await;

        assert_eq!(h.events.error_codes(id), vec![ErrorCode::SttError]);
        assert!(h.stt.stream(0).aborted());
    }

    #[tokio::test(start_paused = true)]
    async fn no_error_after_cancel() {
        // WHY: "never after abort" — the abort caused the socket death, so
        // reporting it would blame the user's network for our own teardown.
        let h = Harness::new();
        let id = h.start().await;
        settle().await;
        h.manager.cancel(id);
        settle().await;

        h.stt.sink(0).on_error(AppError::new(ErrorCode::SttError, "closed by our abort"));
        settle().await;
        assert!(h.events.for_id(id).is_empty());
    }

    // ---- §5.7 empty transcript ----------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn whitespace_transcript_surfaces_no_speech_verbatim_and_never_calls_llm() {
        // WHY: an LLM call on an empty prompt answers a question nobody asked
        // and bills for it; and the message is canonical because it is the one
        // error the user can fix themselves (check call audio).
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("   \n\t  "));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        settle().await;

        let errors = h.events.errors_for(id);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, ErrorCode::NoSpeech);
        assert_eq!(errors[0].message, MSG_NO_SPEECH);
        assert!(h.llm.calls().is_empty());
        assert!(h.events.done_for(id).is_none());
    }

    // ---- §5.8 ask path ------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn ask_emits_final_partial_then_deltas_then_done_with_zero_stt_ms() {
        // WHY: the UI renders typed and spoken questions through one event
        // shape; and billing STT time for a question that never touched STT
        // would lie about the headline metric.
        let h = Harness::new();
        h.llm.push(LlmScript::Stream(vec![(ms(50), "An"), (ms(50), "swer.")]));

        let id = h.ask("  What is Rust?  ").await.expect("valid ask");
        settle().await;
        advance(ms(100)).await;

        assert_eq!(
            h.events.kinds_for(id),
            vec!["stt:partial", "llm:delta", "llm:delta", "llm:done"]
        );
        assert_eq!(
            h.events.partials_for(id),
            vec![("What is Rust?".to_string(), true)],
            "the trimmed question replays as one final partial"
        );
        let (transcript, answer, metrics) = h.events.done_for(id).expect("done");
        assert_eq!(transcript, "What is Rust?");
        assert_eq!(answer, "Answer.");
        assert_eq!(metrics.stt_finalize_ms, 0, "no STT stage happened; 0 is the only honest value");
        assert_eq!(metrics.first_token_ms, 50);
        assert_eq!(metrics.total_ms, 100);
        assert_eq!(h.llm.calls(), vec!["What is Rust?".to_string()]);
    }

    #[tokio::test(start_paused = true)]
    async fn ask_validates_input_before_superseding_the_live_session() {
        // WHY: a fat-fingered empty Ask once killed a live recording. Garbage
        // input must be rejected before any supersession happens.
        let h = Harness::new();
        let id = h.start().await;
        settle().await;

        assert!(h.ask("   \n ").await.is_err());
        let too_long = "x".repeat(limits::MAX_ASK_CHARS + 1);
        assert!(h.ask(&too_long).await.is_err());
        settle().await;

        assert!(!h.stt.stream(0).aborted(), "the live recording must survive garbage ask input");
        h.manager.push_audio(id, &[1], 0.1);
        assert_eq!(h.stt.stream(0).frames(), vec![vec![1]]);
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        settle().await;
        assert!(h.events.done_for(id).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn ask_accepts_exactly_max_chars() {
        // WHY: an off-by-one at the limit rejects the longest legal question.
        let h = Harness::new();
        let question = "y".repeat(limits::MAX_ASK_CHARS);
        let id = h.ask(&question).await.expect("boundary length must be accepted");
        settle().await;
        assert!(h.events.done_for(id).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn ask_over_recording_supersedes_it_silently() {
        // WHY: a valid typed question is a deliberate pivot; the recording it
        // replaces must die without a sound.
        let h = Harness::new();
        let a = h.start().await;
        settle().await;

        let b = h.ask("Typed question").await.expect("valid ask");
        settle().await;

        assert!(h.stt.stream(0).aborted());
        assert!(h.events.errors_for(a).is_empty(), "the superseded recording stays silent");
        assert!(h.events.done_for(b).is_some());
    }

    // ---- connect failures ---------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn connect_failure_surfaces_its_error_and_releases_the_slot() {
        // WHY: a start that can't open its socket must report and free the
        // slot — v2 left a phantom "connecting" session that blocked Record.
        let h = Harness::new();
        h.stt.push(ConnectScript::fail(AppError::new(ErrorCode::SttConnect, "dns failure")));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.events.error_codes(id), vec![ErrorCode::SttConnect]);
        assert_eq!(h.manager.stop(id).await, StopOutcome::NotTaken);

        // The slot is free: the next start proceeds normally.
        let id2 = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id2).await, StopOutcome::Taken);
    }

    #[tokio::test(start_paused = true)]
    async fn connect_timeout_surfaces_stt_connect() {
        // WHY: a hung connect with no cap is a Record button that never
        // answers and never fails.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q").connect_delay(secs(3600)));

        let id = h.start().await;
        settle().await;
        advance(limits::STT_CONNECT).await;

        assert_eq!(h.events.error_codes(id), vec![ErrorCode::SttConnect]);
    }

    // ---- §5.9 timeout interplay ---------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn finalize_timeout_reports_stt_timeout() {
        // WHY: a flush that hangs must fail with its own code — reporting it
        // as anything else sends the user debugging the wrong stage.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q").finalize_delay(secs(3600)));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        advance(limits::STT_FINALIZE).await;

        assert_eq!(h.events.error_codes(id), vec![ErrorCode::SttTimeout]);
        assert!(h.stt.stream(0).aborted(), "the timeout must abort the in-flight finalize");
        assert!(h.events.done_for(id).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn first_token_timeout_fires_and_late_deltas_are_suppressed() {
        // WHY: after the timeout error is on screen, a straggler delta that
        // still paints makes the UI show text under an error banner.
        let h = Harness::new();
        h.llm.push(LlmScript::DetachedDeltas(vec![(secs(11), "too late")]));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        settle().await;

        advance(limits::LLM_FIRST_TOKEN).await;
        assert_eq!(h.events.error_codes(id), vec![ErrorCode::LlmFirstTokenTimeout]);

        advance(secs(2)).await; // the detached socket task pushes its delta now
        assert!(h.events.deltas_for(id).is_empty(), "nothing may paint after the error");
        assert!(h.events.done_for(id).is_none());
        assert_eq!(h.events.errors_for(id).len(), 1, "and the error fires exactly once");
    }

    #[tokio::test(start_paused = true)]
    async fn first_delta_disarms_the_first_token_timeout() {
        // WHY: once tokens flow, only the total cap applies — killing a
        // healthy stream at the 10 s mark truncated real answers in v2.
        let h = Harness::new();
        h.llm.push(LlmScript::Stream(vec![(secs(9), "Nine"), (secs(3), " seconds later")]));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        settle().await;

        advance(secs(9)).await; // first delta lands just inside the cap
        advance(secs(3)).await; // second delta lands past it — legally

        let (_, answer, metrics) = h.events.done_for(id).expect("disarmed stream must finish");
        assert_eq!(answer, "Nine seconds later");
        assert_eq!(metrics.first_token_ms, 9_000);
        assert_eq!(metrics.total_ms, 12_000);
        assert!(h.events.errors_for(id).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn total_timeout_reports_llm_timeout() {
        // WHY: LLM_TOTAL runs to completion regardless of streaming progress;
        // an answer that trickles forever is worse than a clean failure.
        let h = Harness::new();
        h.llm.push(LlmScript::Stream(vec![(secs(1), "started"), (secs(120), " never lands")]));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        settle().await;

        advance(secs(1)).await;
        assert_eq!(h.events.deltas_for(id), vec!["started".to_string()]);
        advance(secs(59)).await; // the 60 s total deadline

        assert_eq!(h.events.error_codes(id), vec![ErrorCode::LlmTimeout]);
        assert!(h.events.done_for(id).is_none());
        assert_eq!(
            h.events.deltas_for(id),
            vec!["started".to_string()],
            "no delta paints after the timeout"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn max_recording_auto_stops_and_answers_normally() {
        // WHY: the 120 s cap is a stop, not a failure — the user gets an
        // answer for what was said, with metrics counted from the auto-stop.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("long monologue").finalize_delay(ms(100)));
        h.llm.push(LlmScript::Stream(vec![(ms(200), "the answer")]));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.llm.prewarms(), 1);

        advance(limits::MAX_RECORDING).await;
        assert_eq!(h.llm.prewarms(), 2, "auto-stop must prewarm exactly like a user stop");
        assert_eq!(
            h.manager.stop(id).await,
            StopOutcome::NotTaken,
            "the session already auto-stopped"
        );

        advance(ms(100)).await;
        advance(ms(200)).await;
        let (transcript, answer, metrics) = h.events.done_for(id).expect("normal answer");
        assert_eq!(transcript, "long monologue");
        assert_eq!(answer, "the answer");
        assert_eq!(metrics.stt_finalize_ms, 100);
        assert_eq!(metrics.first_token_ms, 300);
        assert_eq!(metrics.total_ms, 300);
    }

    // ---- §5.10 cancel -------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn cancel_is_silent_and_releases_the_slot() {
        // WHY: cancel means "pretend this never happened" — any event at all
        // makes the UI animate a session the user just dismissed.
        let h = Harness::new();
        let id = h.start().await;
        settle().await;
        h.manager.push_audio(id, &[1], 0.3);
        let before = h.events.for_id(id).len();

        h.manager.cancel(id);
        settle().await;

        assert!(h.stt.stream(0).aborted());
        assert_eq!(h.events.for_id(id).len(), before, "cancel emits nothing");
        assert_eq!(h.manager.stop(id).await, StopOutcome::NotTaken);

        // Slot released: the next session starts clean.
        let id2 = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id2).await, StopOutcome::Taken);
        settle().await;
        assert!(h.events.done_for(id2).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_during_finalize_is_silent() {
        // WHY: cancel-during-stop is the razor's edge — the finalize is in
        // flight and both its completion AND its timeout must be muzzled.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q").finalize_delay(secs(3600)));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        settle().await;

        h.manager.cancel(id);
        settle().await;
        advance(secs(10)).await; // past the finalize deadline: no stt_timeout may fire

        assert!(h.events.for_id(id).is_empty());
        assert!(h.stt.stream(0).aborted());
    }

    // ---- §5.11 slot release -------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn slot_release_after_done_leaves_no_ghost() {
        // WHY: if the slot lingered after done, the next start would "supersede"
        // a ghost — aborting a finished stream and muting nobody.
        let h = Harness::new();
        let a = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(a).await, StopOutcome::Taken);
        settle().await;
        assert!(h.events.done_for(a).is_some());

        let events_a = h.events.for_id(a).len();
        let b = h.start().await;
        settle().await;

        assert!(!h.stt.stream(0).aborted(), "a completed session must not be torn down as a ghost");
        assert_eq!(h.events.for_id(a).len(), events_a, "the finished session stays finished");
        assert_eq!(h.manager.stop(b).await, StopOutcome::Taken);
    }

    #[tokio::test(start_paused = true)]
    async fn slot_release_after_error_leaves_no_ghost() {
        // WHY: same rule on the error path — the slot must be free the moment
        // the error is emitted, or the next Record press fights a corpse.
        let h = Harness::new();
        let a = h.start().await;
        settle().await;
        h.stt.sink(0).on_error(AppError::new(ErrorCode::SttError, "died"));
        settle().await;
        assert_eq!(h.events.errors_for(a).len(), 1);

        let b = h.start().await;
        settle().await;
        assert_eq!(h.manager.stop(b).await, StopOutcome::Taken);
        settle().await;
        assert!(h.events.done_for(b).is_some());
        assert_eq!(h.events.errors_for(a).len(), 1, "no second error from the teardown");
    }

    // ---- metrics & prewarm --------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn metrics_are_measured_from_the_stop_instant() {
        // WHY: recording time is the user talking, not us working. Counting
        // it would make a long question look like a slow answer.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("Q").finalize_delay(ms(300)));
        h.llm.push(LlmScript::Stream(vec![(ms(500), "A"), (ms(200), "B")]));

        let id = h.start().await;
        settle().await;
        advance(secs(3)).await; // 3 s of recording — must not count
        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        advance(ms(300)).await;
        advance(ms(500)).await;
        advance(ms(200)).await;

        let (_, _, metrics) = h.events.done_for(id).expect("done");
        assert_eq!(metrics.stt_finalize_ms, 300);
        assert_eq!(metrics.first_token_ms, 800);
        assert_eq!(metrics.total_ms, 1_000);
    }

    #[tokio::test(start_paused = true)]
    async fn non_streaming_answer_reports_total_as_first_token() {
        // WHY: a provider that returns the whole answer in one shot has a real
        // time-to-first-word — the moment it arrived. 0 would render "instant".
        let h = Harness::new();
        h.llm.push(LlmScript::Full(ms(700), "whole answer"));

        let id = h.ask("Q").await.expect("valid ask");
        settle().await;
        advance(ms(700)).await;

        let (_, answer, metrics) = h.events.done_for(id).expect("done");
        assert_eq!(answer, "whole answer");
        assert!(h.events.deltas_for(id).is_empty());
        assert_eq!(metrics.first_token_ms, 700);
        assert_eq!(metrics.total_ms, 700);
    }

    #[tokio::test(start_paused = true)]
    async fn prewarm_fires_on_start_stop_and_ask() {
        // WHY: the stop-time prewarm overlapping the STT flush is what buys
        // the ~1 s stop-to-first-word; dropping any of the three shows up as
        // a cold TLS handshake on the critical path.
        let h = Harness::new();
        h.stt.push(ConnectScript::ok("q").finalize_delay(ms(100)));

        let id = h.start().await;
        settle().await;
        assert_eq!(h.llm.prewarms(), 1, "prewarm on start");

        assert_eq!(h.manager.stop(id).await, StopOutcome::Taken);
        // Asserted before the finalize resolves: the warm must overlap it.
        assert_eq!(h.llm.prewarms(), 2, "prewarm on stop, before awaiting the finalize");
        advance(ms(100)).await;
        assert!(h.events.done_for(id).is_some());

        h.ask("typed question").await.expect("valid ask");
        assert_eq!(h.llm.prewarms(), 3, "prewarm on ask");
    }

    #[tokio::test(start_paused = true)]
    async fn llm_provider_error_passes_through_and_releases_the_slot() {
        // WHY: provider errors carry their own closed-set codes (llm_http,
        // llm_auth…); re-wrapping them would break the UI's error routing.
        let h = Harness::new();
        h.llm.push(LlmScript::Fail(ms(100), AppError::new(ErrorCode::LlmHttp, "upstream 500")));

        let id = h.ask("Q").await.expect("valid ask");
        settle().await;
        advance(ms(100)).await;

        assert_eq!(h.events.error_codes(id), vec![ErrorCode::LlmHttp]);
        assert!(h.events.done_for(id).is_none());

        // Slot released: the next question proceeds.
        let id2 = h.ask("Again").await.expect("valid ask");
        settle().await;
        assert!(h.events.done_for(id2).is_some());
    }
}

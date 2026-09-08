//! Speech-to-text contract and the Deepgram implementation.

pub mod deepgram;
pub mod local;
pub mod frame;

use async_trait::async_trait;
use std::sync::Arc;

use crate::error::AppResult;
use crate::AppError;

pub use frame::{parse_frame, DeepgramFrame, TranscriptAccumulator};

/// Where a live STT stream pushes what it hears.
pub trait SttSink: Send + Sync + 'static {
    /// The full transcript so far (not a delta), and whether the latest segment
    /// is committed.
    fn on_transcript(&self, text: String, is_final: bool);
    /// The stream died. Implementations must call this at most once.
    ///
    /// It may still arrive concurrently with, or shortly after, `abort()`:
    /// an implementation reads its abort flag and then calls this, and a
    /// report already past that read cannot be recalled. So "no error after
    /// abort" is not something this trait can promise — consumers must gate
    /// on their own teardown state and drop a straggler rather than act on
    /// it. (The session machine does: the task that owns the receiving end
    /// has already returned, and its event gate is dead, so a late error can
    /// neither fail nor resurrect a session that was torn down.)
    fn on_error(&self, error: AppError);
}

/// A live speech-to-text stream.
#[async_trait]
pub trait SttStream: Send + Sync {
    /// Queue one ~128 ms frame of 16 kHz mono little-endian i16 PCM.
    /// Never blocks: audio arrives on a realtime callback thread.
    fn send_audio(&self, pcm: &[i16]);

    /// Ask the service to flush its tail and close, then resolve with the full
    /// transcript.
    ///
    /// Must be idempotent — a second call joins the first rather than starting a
    /// second finalize. Must return immediately (rather than burning the 5 s
    /// cap) if the stream has already died or can no longer open. A dial still
    /// in flight is neither: it may yet open onto audio already captured, so
    /// waiting it out within the cap is correct, not a stall.
    async fn finalize(&self) -> AppResult<String>;

    /// Tear down without finalizing. The resulting socket death must not be
    /// reported as an error — the caller caused it.
    fn abort(&self);
}

/// Opens STT streams. Injected so the state machine can be tested against a
/// scripted fake.
#[async_trait]
pub trait SttConnector: Send + Sync + 'static {
    async fn connect(&self, sink: Arc<dyn SttSink>) -> AppResult<Box<dyn SttStream>>;
}

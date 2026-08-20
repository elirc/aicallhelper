//! The Deepgram live-streaming connector (§6.1).
//!
//! One background task owns the socket end to end: it dials, flushes audio
//! that arrived while the socket was still opening, pumps audio and
//! keepalives out, pumps transcript frames in, and drains the tail on close.
//! A single owner means every failure has exactly one classification site, so
//! `SttSink::on_error` cannot fire twice. Silencing it after an abort is
//! best-effort by construction — the flag is read a moment before the call —
//! which is why `SttSink::on_error` documents that race and puts the
//! never-after-abort guarantee in the consumer.
//!
//! The capture side never waits on any of this: `send_audio` is one
//! allocation and an unbounded-channel push, because it runs on a realtime
//! audio callback thread where blocking causes glitches in the capture
//! itself.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch, Notify};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as WsRequest;
use tokio_tungstenite::tungstenite::http::{header, HeaderValue};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::error::{AppError, AppResult, ErrorCode, MSG_NO_STT_KEY};
use crate::session::limits;
use crate::stt::{parse_frame, DeepgramFrame, SttConnector, SttSink, SttStream, TranscriptAccumulator};

/// The exact URL from §6.1. `endpointing` and `no_delay` are deliberately
/// absent: this client stops when the user clicks Stop, never when the
/// endpointer speaks, so those knobs buy nothing here — and `no_delay` makes
/// smart_format commit entities (numbers, dates) before it is sure of them.
pub const DEEPGRAM_URL: &str = "wss://api.deepgram.com/v1/listen?model=nova-3&encoding=linear16&sample_rate=16000&channels=1&interim_results=true&smart_format=true";

const KEEPALIVE: &str = r#"{"type":"KeepAlive"}"#;
const CLOSE_STREAM: &str = r#"{"type":"CloseStream"}"#;

/// Deepgram kills a socket ~10 s after the last audio (NET-0001), and silence
/// during a call is normal — the user is listening, not talking. 8 s keeps us
/// safely inside that window without spamming.
const KEEPALIVE_PERIOD: Duration = Duration::from_secs(8);

/// Cap on audio buffered while the socket is still opening: 15 s of 16 kHz
/// mono i16. Past it the OLDEST frames are dropped — the newest audio is the
/// speech the user is asking about right now, and it is the connect that is
/// slow, not the speech that is wrong.
const PRE_OPEN_BUFFER_MAX_BYTES: usize = 15 * 16_000 * 2;

pub struct DeepgramConnector {
    api_key: String,
    url: String,
}

impl DeepgramConnector {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_url(api_key, DEEPGRAM_URL)
    }

    /// Same connector, different endpoint — how tests point it at a local
    /// scripted server instead of Deepgram.
    pub fn with_url(api_key: impl Into<String>, url: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), url: url.into() }
    }
}

#[async_trait]
impl SttConnector for DeepgramConnector {
    async fn connect(&self, sink: Arc<dyn SttSink>) -> AppResult<Box<dyn SttStream>> {
        // Fail before touching the network: dialing Deepgram just to be
        // rejected would turn "you never set a key" into a 5 s spinner ending
        // in a misleading connection error.
        let key = self.api_key.trim();
        if key.is_empty() {
            return Err(AppError::new(ErrorCode::NoSttKey, MSG_NO_STT_KEY));
        }
        let request = build_request(&self.url, key)?;

        let (audio_tx, audio_rx) = mpsc::unbounded_channel();
        let (done_tx, done_rx) = watch::channel(false);
        let shared = Arc::new(Shared {
            acc: Mutex::new(TranscriptAccumulator::new()),
            close_requested: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
            wake: Notify::new(),
        });

        let driver_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            run(request, driver_shared, sink, audio_rx).await;
            // Every finalize() — present and future — waits on this. Sent on
            // every exit path, including panicky ones (a dropped sender also
            // wakes the watch), so a stop can never hang on a dead driver.
            let _ = done_tx.send(true);
        });

        // Returned before the handshake resolves, deliberately: capture starts
        // the instant the user clicks Record, and the first words land while
        // the socket is still opening. They buffer in the channel and flush at
        // open rather than being lost.
        Ok(Box::new(DeepgramStream { audio_tx, shared, done: done_rx }))
    }
}

fn build_request(url: &str, key: &str) -> AppResult<WsRequest> {
    let mut request = url
        .into_client_request()
        .map_err(|e| connect_failed(format!("Invalid Deepgram URL: {e}.")))?;
    // Deepgram's websocket auth rides the subprotocol list: "token, <key>".
    // A key with header-illegal bytes can only be a paste accident; refusing
    // it here beats letting the handshake die with an opaque http error.
    let protocols = HeaderValue::from_str(&format!("token, {key}")).map_err(|_| {
        connect_failed("The Deepgram API key contains characters that cannot be sent in a header.")
    })?;
    request.headers_mut().insert(header::SEC_WEBSOCKET_PROTOCOL, protocols);
    Ok(request)
}

/// State the stream handle and the driver task share.
struct Shared {
    acc: Mutex<TranscriptAccumulator>,
    close_requested: AtomicBool,
    aborted: AtomicBool,
    /// Kicks the driver out of whatever it is awaiting so it re-reads the
    /// flags above.
    wake: Notify,
}

struct DeepgramStream {
    audio_tx: mpsc::UnboundedSender<Vec<u8>>,
    shared: Arc<Shared>,
    done: watch::Receiver<bool>,
}

#[async_trait]
impl SttStream for DeepgramStream {
    fn send_audio(&self, pcm: &[i16]) {
        // The realtime callback thread ends here: one allocation and an
        // unbounded push — no lock shared with the driver, nothing that
        // waits. If the driver is gone the send fails, and silently is
        // correct: the caller learns the stream ended through other channels.
        let _ = self.audio_tx.send(pcm_to_le_bytes(pcm));
    }

    async fn finalize(&self) -> AppResult<String> {
        // The flag, not this caller, triggers the CloseStream: however many
        // finalize() calls race, the driver crosses into its drain exactly
        // once, so exactly one CloseStream goes out and every caller joins
        // the same completion.
        self.shared.close_requested.store(true, Ordering::SeqCst);
        self.shared.wake.notify_one();

        // Already-done resolves instantly: a stream that never opened or has
        // already died must not make the user's stop wait out the cap for a
        // tail that cannot arrive.
        let mut done = self.done.clone();
        let _ = tokio::time::timeout(limits::STT_FINALIZE, done.wait_for(|d| *d)).await;

        Ok(self.shared.acc.lock().expect("transcript lock").finalized_text())
    }

    fn abort(&self) {
        // Just flags: the driver sees them and dies without a close
        // handshake. The same flag gates every report path, so the socket
        // death this causes is not reported — the caller asked for it. Not a
        // guarantee, though: a report already past that check still lands,
        // which is the race `SttSink::on_error` documents.
        self.shared.aborted.store(true, Ordering::SeqCst);
        self.shared.wake.notify_one();
    }
}

impl Drop for DeepgramStream {
    fn drop(&mut self) {
        // A handle dropped without finalize() is a caller that walked away;
        // treat it as an abort so the driver task cannot outlive its owner.
        self.abort();
    }
}

/// The driver task: owns the socket from dial to death.
async fn run(
    request: WsRequest,
    shared: Arc<Shared>,
    sink: Arc<dyn SttSink>,
    mut audio_rx: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    // -- Dialing. Only an abort abandons the dial: the caller wants nothing
    // back, so there is nothing to wait for. A stop must NOT abandon it. What
    // hangs on a still-resolving dial is not a server-side tail — it is the
    // user's ENTIRE captured question, sitting in the pre-open channel. On a
    // slow network (2-5 s of TLS/DNS) a short question is recorded and stopped
    // before the handshake lands; returning here would discard that audio,
    // finalize to "", and have the session tell a user whose audio flowed
    // perfectly to "make sure call audio is playing". So we keep awaiting: the
    // connect timeout bounds this task and finalize()'s own 5 s cap bounds the
    // caller, which leaves room to open, flush the buffer, CloseStream and
    // come back with a real transcript.
    let connect = tokio::time::timeout(limits::STT_CONNECT, connect_async(request));
    tokio::pin!(connect);
    let handshake = loop {
        if shared.aborted.load(Ordering::SeqCst) {
            return;
        }
        tokio::select! {
            result = &mut connect => break result,
            _ = shared.wake.notified() => {}
        }
    };

    // A dial that FAILS is reported even when a stop is already pending: the
    // recording is unusable BECAUSE the connect failed, and `stt_connect` says
    // exactly that, where silence would leave finalize returning "" and the
    // session blaming the user's audio for a network fault.
    let mut ws = match handshake {
        Ok(Ok((ws, _response))) => ws,
        Ok(Err(e)) => {
            report(&shared, &*sink, connect_failed(format!("Could not connect to Deepgram: {e}.")));
            return;
        }
        Err(_) => {
            report(
                &shared,
                &*sink,
                connect_failed(format!(
                    "Connecting to Deepgram timed out after {} seconds.",
                    limits::STT_CONNECT.as_secs()
                )),
            );
            return;
        }
    };

    // -- Flush audio captured while the socket was opening. Draining the
    // channel in one synchronous sweep before applying the cap keeps the
    // drop-the-oldest decision deterministic: everything sent before open is
    // on the table when the cap is applied.
    let mut pending: VecDeque<Vec<u8>> = VecDeque::new();
    let mut pending_bytes = 0usize;
    while let Ok(frame) = audio_rx.try_recv() {
        pending_bytes += frame.len();
        pending.push_back(frame);
        while pending_bytes > PRE_OPEN_BUFFER_MAX_BYTES {
            match pending.pop_front() {
                Some(oldest) => pending_bytes -= oldest.len(),
                None => break,
            }
        }
    }
    for frame in pending {
        if let Err(e) = ws.send(Message::Binary(frame)).await {
            report(&shared, &*sink, io_death(false, &e));
            return;
        }
    }

    // First tick at +8 s, not immediately: a KeepAlive right after open says
    // nothing Deepgram doesn't already know.
    let mut keepalive = tokio::time::interval_at(
        tokio::time::Instant::now() + KEEPALIVE_PERIOD,
        KEEPALIVE_PERIOD,
    );

    // "Established" = Deepgram has said something. A successful websocket
    // handshake proves nothing about the key: Deepgram accepts the upgrade
    // and then rejects bad keys by closing (1008 / DATA-xxxx), usually with
    // no Error frame at all. Only a first frame from the server separates
    // "connected" from "about to be rejected", so a close before that first
    // frame is reported as a connect failure, not a mid-call drop.
    let mut established = false;
    let mut audio_live = true;

    loop {
        if shared.aborted.load(Ordering::SeqCst) {
            return;
        }
        if shared.close_requested.load(Ordering::SeqCst) {
            break;
        }
        tokio::select! {
            _ = shared.wake.notified() => {}
            frame = audio_rx.recv(), if audio_live => match frame {
                Some(bytes) => {
                    if let Err(e) = ws.send(Message::Binary(bytes)).await {
                        report(&shared, &*sink, io_death(established, &e));
                        return;
                    }
                }
                // Every handle is gone; nothing more will arrive. Disarm the
                // branch or recv() returns None in a hot loop.
                None => audio_live = false,
            },
            _ = keepalive.tick() => {
                // Re-check on the tick itself: a stop can land while this
                // task is parked, and a KeepAlive written to a CLOSING socket
                // errors — fabricating a "lost connection" out of a stop that
                // is actually succeeding.
                if shared.close_requested.load(Ordering::SeqCst)
                    || shared.aborted.load(Ordering::SeqCst)
                {
                    continue;
                }
                if let Err(e) = ws.send(Message::Text(KEEPALIVE.to_string())).await {
                    report(&shared, &*sink, io_death(established, &e));
                    return;
                }
            }
            incoming = ws.next() => match incoming {
                Some(Ok(Message::Text(raw))) => {
                    established = true;
                    match parse_frame(&raw) {
                        DeepgramFrame::Error { detail } => {
                            report(&shared, &*sink, AppError::new(
                                ErrorCode::SttError,
                                format!("Deepgram reported an error: {detail}"),
                            ));
                            return;
                        }
                        frame => apply_and_emit(&shared, &*sink, &frame),
                    }
                }
                Some(Ok(Message::Close(close))) => {
                    report(&shared, &*sink, close_death(established, close));
                    return;
                }
                // Binary, ping, pong, raw frames: tungstenite answers pings
                // itself and Deepgram sends nothing else we care about.
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    report(&shared, &*sink, io_death(established, &e));
                    return;
                }
                None => {
                    report(&shared, &*sink, close_death(established, None));
                    return;
                }
            }
        }
    }

    // -- Finalize. The keepalive dies with its interval, before CloseStream
    // goes out: one sent after it can error on the CLOSING socket.
    drop(keepalive);
    let drain = async {
        // Frames captured between the pump's last poll and the stop are still
        // queued in the channel. Send them before CloseStream: they carry the
        // final words of the question — the part the user is asking about —
        // and dropping them silently truncates the transcript's tail.
        while let Ok(bytes) = audio_rx.try_recv() {
            if ws.send(Message::Binary(bytes)).await.is_err() {
                finalize_death(&shared, &*sink);
                return;
            }
        }

        // CloseStream makes the server flush held-back text (smart_format
        // holds entities like numbers until it is sure of them) and close
        // from its side.
        //
        // A death in here is NOT silent (§5.5): the flush was cut, so the
        // transcript may be missing its tail, and answering a silently
        // truncated question is answering the wrong question. Only the
        // server's own close — the normal end of a drain — is clean.
        // `finalize_death` bypasses the close_requested gate (the close was
        // requested, yes, but this is not the death our close causes — the
        // server flushing and closing IS the success path) while still
        // honouring abort.
        if ws.send(Message::Text(CLOSE_STREAM.to_string())).await.is_err() {
            finalize_death(&shared, &*sink);
            return;
        }
        while let Some(item) = ws.next().await {
            if shared.aborted.load(Ordering::SeqCst) {
                return;
            }
            match item {
                // An Error frame during the flush is a failed flush (§6.1
                // requires quoting its detail; §5.5 forbids swallowing it):
                // the server is telling us the tail is not coming, and the
                // clean Close that follows must not launder that into a
                // successful finalize.
                Ok(Message::Text(raw)) => match parse_frame(&raw) {
                    DeepgramFrame::Error { detail } => {
                        if !shared.aborted.load(Ordering::SeqCst) {
                            sink.on_error(AppError::new(
                                ErrorCode::SttError,
                                format!("Deepgram reported an error while finalizing: {detail}"),
                            ));
                        }
                        return;
                    }
                    frame => apply_and_emit(&shared, &*sink, &frame),
                },
                // The server-side close is the normal end of a drain: the
                // tail was flushed and the transcript is complete.
                Ok(Message::Close(_)) => return,
                Err(_) => {
                    finalize_death(&shared, &*sink);
                    return;
                }
                Ok(_) => {}
            }
        }
    };
    // The driver enforces the cap too: finalize() gives up waiting at the
    // cap, but this task must not linger forever on a server that never
    // closes.
    let _ = tokio::time::timeout(limits::STT_FINALIZE, drain).await;
}

/// Apply a parsed frame and, if the visible transcript changed, tell the sink.
fn apply_and_emit(shared: &Shared, sink: &dyn SttSink, frame: &DeepgramFrame) {
    let DeepgramFrame::Results { is_final, .. } = frame else { return };
    // The lock is released before the sink runs: a sink that blocks (or
    // panics) must not poison or hold up the transcript finalize() reads.
    let (changed, text) = {
        let mut acc = shared.acc.lock().expect("transcript lock");
        (acc.apply(frame), acc.text())
    };
    if changed {
        sink.on_transcript(text, *is_final);
    }
}

/// A socket death while the CloseStream flush was still in progress (§5.5).
/// Unlike `report`, only `aborted` silences this: `close_requested` is by
/// definition set during a drain, and gating on it here would silently
/// truncate the transcript — the exact failure §5.5 exists to prevent.
fn finalize_death(shared: &Shared, sink: &dyn SttSink) {
    if shared.aborted.load(Ordering::SeqCst) {
        return;
    }
    sink.on_error(AppError::new(
        ErrorCode::SttError,
        "Lost the Deepgram connection while finalizing the transcript. Try again.",
    ));
}

fn report(shared: &Shared, sink: &dyn SttSink, error: AppError) {
    // Gated on `aborted` ONLY — including the two dial failures, which no
    // longer have a gate of their own. At every call site CloseStream has not
    // been sent (it only goes out in the drain, after the pump breaks), so a
    // death here can never be "our own close's doing". Gating on
    // close_requested too would silently swallow a genuine death that raced a
    // Stop: before open, a failed connect becomes an empty transcript the
    // session reports as no_speech; after open, a truncated transcript with no
    // error — the exact §5.5 failure. (The one self-caused death the old broad
    // gate protected against — a KeepAlive written after CloseStream — is
    // structurally impossible: the keepalive interval is dropped before the
    // drain starts, and its tick re-checks the flags before sending.) "Every
    // report site returns from the driver immediately" is what keeps on_error
    // at-most-once; the never-after-abort half is only best-effort here (the
    // load and the call are not atomic) and is enforced by the consumer, as
    // `SttSink::on_error` documents.
    if shared.aborted.load(Ordering::SeqCst) {
        return;
    }
    sink.on_error(error);
}

fn connect_failed(detail: impl AsRef<str>) -> AppError {
    AppError::new(
        ErrorCode::SttConnect,
        format!("{} Check the API key and your network.", detail.as_ref()),
    )
}

fn close_death(established: bool, close: Option<CloseFrame<'static>>) -> AppError {
    // 1008 carries Deepgram's DATA-xxxx rejections (bad key, bad request) and
    // 1011 its NET-xxxx server faults. The code and reason are the only
    // diagnostics a reject-by-close ever offers, so quote them.
    let what = match &close {
        Some(frame) => format!("close code {} ({})", u16::from(frame.code), frame.reason),
        None => "the connection ended without a close frame".to_string(),
    };
    if established {
        AppError::new(ErrorCode::SttError, format!("Lost the Deepgram connection: {what}."))
    } else {
        connect_failed(format!("Deepgram closed the connection before it was ready: {what}."))
    }
}

fn io_death(established: bool, error: &WsError) -> AppError {
    if established {
        AppError::new(ErrorCode::SttError, format!("Lost the Deepgram connection: {error}."))
    } else {
        connect_failed(format!("The Deepgram connection failed before it was ready: {error}."))
    }
}

fn pcm_to_le_bytes(pcm: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for sample in pcm {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::tungstenite::handshake::server::{
        Request as HsRequest, Response as HsResponse,
    };
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::{accept_hdr_async, WebSocketStream};

    type ServerWs = WebSocketStream<TcpStream>;

    /// Sink that forwards every callback into a channel the test can await —
    /// polling shared Vecs would make every assertion a sleep-and-hope.
    struct TestSink {
        events: mpsc::UnboundedSender<SinkEvent>,
    }

    #[derive(Debug)]
    enum SinkEvent {
        Transcript(String, bool),
        Error(AppError),
    }

    impl SttSink for TestSink {
        fn on_transcript(&self, text: String, is_final: bool) {
            let _ = self.events.send(SinkEvent::Transcript(text, is_final));
        }
        fn on_error(&self, error: AppError) {
            let _ = self.events.send(SinkEvent::Error(error));
        }
    }

    fn test_sink() -> (Arc<TestSink>, mpsc::UnboundedReceiver<SinkEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(TestSink { events: tx }), rx)
    }

    async fn next_event(rx: &mut mpsc::UnboundedReceiver<SinkEvent>) -> SinkEvent {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for a sink event")
            .expect("sink channel closed with no event")
    }

    async fn bind() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        (listener, url)
    }

    /// Accept one websocket, capturing the offered Sec-WebSocket-Protocol so
    /// tests can assert the auth actually went over the wire.
    // The callback's Err type is tungstenite's ErrorResponse by contract —
    // its size is not ours to shrink, and this is test scaffolding.
    #[allow(clippy::result_large_err)]
    async fn accept(listener: &TcpListener) -> (ServerWs, Option<String>) {
        let (tcp, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .expect("client never dialed")
            .expect("tcp accept failed");
        let offered: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&offered);
        let ws = accept_hdr_async(tcp, move |req: &HsRequest, mut resp: HsResponse| {
            *seen.lock().unwrap() = req
                .headers()
                .get(header::SEC_WEBSOCKET_PROTOCOL)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            // Deepgram answers by selecting "token"; mirror that so a client
            // that validates the negotiated subprotocol accepts the handshake.
            resp.headers_mut()
                .insert(header::SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("token"));
            Ok(resp)
        })
        .await
        .expect("websocket accept failed");
        let offered = offered.lock().unwrap().clone();
        (ws, offered)
    }

    fn results_msg(transcript: &str, is_final: bool) -> Message {
        Message::Text(
            serde_json::json!({
                "type": "Results",
                "is_final": is_final,
                "channel": { "alternatives": [{ "transcript": transcript }] }
            })
            .to_string(),
        )
    }

    async fn connect_to(
        url: String,
    ) -> (Box<dyn SttStream>, mpsc::UnboundedReceiver<SinkEvent>) {
        let (sink, events) = test_sink();
        let stream = DeepgramConnector::with_url("test-key-123", url)
            .connect(sink)
            .await
            .expect("connect() itself must not fail");
        (stream, events)
    }

    #[test]
    fn the_default_connector_targets_the_documented_url() {
        let c = DeepgramConnector::new("k");
        assert_eq!(c.url, DEEPGRAM_URL);
        assert!(DEEPGRAM_URL.contains("model=nova-3"));
        assert!(DEEPGRAM_URL.contains("encoding=linear16"));
        assert!(DEEPGRAM_URL.contains("sample_rate=16000"));
        assert!(DEEPGRAM_URL.contains("channels=1"));
        assert!(DEEPGRAM_URL.contains("interim_results=true"));
        assert!(DEEPGRAM_URL.contains("smart_format=true"));
        // Deliberate omissions (§6.1) — pinning them so a well-meaning tweak
        // has to argue with this test first.
        assert!(!DEEPGRAM_URL.contains("endpointing"));
        assert!(!DEEPGRAM_URL.contains("no_delay"));
    }

    #[tokio::test]
    async fn an_empty_api_key_fails_fast_without_a_socket() {
        let (listener, url) = bind().await;
        for key in ["", "   "] {
            let (sink, _events) = test_sink();
            let err = DeepgramConnector::with_url(key, url.clone())
                .connect(sink)
                .await
                .err()
                .expect("an empty key must not connect");
            assert_eq!(err.code, ErrorCode::NoSttKey);
            assert_eq!(err.message, MSG_NO_STT_KEY);
        }
        // No dial was ever attempted, not merely a fast failure after one.
        tokio::time::timeout(Duration::from_millis(200), listener.accept())
            .await
            .expect_err("a socket was opened despite the missing key");
    }

    #[tokio::test]
    async fn the_subprotocol_header_carries_the_token() {
        let (listener, url) = bind().await;
        let (stream, _events) = connect_to(url).await;
        let (_server, offered) = accept(&listener).await;
        assert_eq!(offered.as_deref(), Some("token, test-key-123"));
        stream.abort();
    }

    #[tokio::test]
    async fn happy_path_accumulates_and_finalize_returns_the_tail() {
        let (listener, url) = bind().await;
        let (stream, mut events) = connect_to(url).await;
        let (mut server, _) = accept(&listener).await;

        server.send(results_msg("tell me", false)).await.unwrap();
        server
            .send(results_msg("Tell me about yourself.", true))
            .await
            .unwrap();

        let SinkEvent::Transcript(t1, f1) = next_event(&mut events).await else {
            panic!("expected a transcript event");
        };
        assert_eq!((t1.as_str(), f1), ("tell me", false));
        let SinkEvent::Transcript(t2, f2) = next_event(&mut events).await else {
            panic!("expected a transcript event");
        };
        assert_eq!((t2.as_str(), f2), ("Tell me about yourself.", true));

        let server_task = async {
            let saw_close_stream =
                match server.next().await.expect("client hung up early").unwrap() {
                    Message::Text(t) if t == CLOSE_STREAM => true,
                    other => panic!("unexpected frame before CloseStream: {other:?}"),
                };
            // The smart_format hold-back: text the server only releases once
            // CloseStream arrives. It must still make the returned transcript.
            server
                .send(results_msg("And why this role?", true))
                .await
                .unwrap();
            server.send(Message::Close(None)).await.unwrap();
            saw_close_stream
        };
        let (finalized, saw_close_stream) = tokio::time::timeout(
            Duration::from_secs(10),
            futures_util::future::join(stream.finalize(), server_task),
        )
        .await
        .expect("finalize hung");
        assert!(saw_close_stream);
        assert_eq!(
            finalized.unwrap(),
            "Tell me about yourself. And why this role?"
        );
    }

    #[tokio::test]
    async fn audio_is_sent_as_binary_little_endian_i16() {
        let (listener, url) = bind().await;
        let (stream, _events) = connect_to(url).await;
        let (mut server, _) = accept(&listener).await;

        let pcm: Vec<i16> = vec![0, 1, -1, i16::MAX, i16::MIN, 12345, -12345];
        stream.send_audio(&pcm);

        let msg = tokio::time::timeout(Duration::from_secs(5), server.next())
            .await
            .expect("no audio arrived")
            .unwrap()
            .unwrap();
        let Message::Binary(bytes) = msg else {
            panic!("audio must be a binary frame, got {msg:?}");
        };
        let expected: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(bytes, expected);
        stream.abort();
    }

    #[tokio::test]
    async fn audio_sent_before_open_is_flushed_in_order_at_open() {
        let (listener, url) = bind().await;
        let (stream, _events) = connect_to(url).await;

        // The server has not accepted yet, so the socket cannot be open: these
        // frames land in the pre-open buffer, deterministically.
        stream.send_audio(&[1i16; 8]);
        stream.send_audio(&[2i16; 8]);
        stream.send_audio(&[3i16; 8]);

        let (mut server, _) = accept(&listener).await;
        for expected in [1i16, 2, 3] {
            let msg = tokio::time::timeout(Duration::from_secs(5), server.next())
                .await
                .expect("buffered audio never flushed")
                .unwrap()
                .unwrap();
            let Message::Binary(bytes) = msg else {
                panic!("expected binary audio, got {msg:?}");
            };
            let want: Vec<u8> = [expected; 8].iter().flat_map(|s| s.to_le_bytes()).collect();
            assert_eq!(bytes, want, "frame {expected} out of order or corrupted");
        }
        stream.abort();
    }

    #[tokio::test]
    async fn pre_open_buffer_drops_the_oldest_beyond_fifteen_seconds() {
        let (listener, url) = bind().await;
        let (stream, _events) = connect_to(url).await;

        // 120 frames x 2048 samples = 245,760 samples against a 240,000 cap:
        // exactly the 3 oldest frames must go.
        for i in 0..120i16 {
            stream.send_audio(&vec![i; 2048]);
        }

        let (mut server, _) = accept(&listener).await;
        let mut seen = Vec::new();
        for _ in 0..117 {
            let msg = tokio::time::timeout(Duration::from_secs(5), server.next())
                .await
                .expect("flush stopped early")
                .unwrap()
                .unwrap();
            let Message::Binary(bytes) = msg else {
                panic!("expected binary audio, got {msg:?}");
            };
            assert_eq!(bytes.len(), 2048 * 2);
            seen.push(i16::from_le_bytes([bytes[0], bytes[1]]));
        }
        // Oldest dropped, newest kept: losing the newest would lose the very
        // speech the user is asking about.
        assert_eq!(seen.first(), Some(&3));
        assert_eq!(seen.last(), Some(&119));
        assert!(seen.windows(2).all(|w| w[1] == w[0] + 1), "frames reordered: {seen:?}");
        stream.abort();
    }

    #[tokio::test]
    async fn keepalives_flow_on_an_idle_open_socket() {
        let (listener, url) = bind().await;
        let (stream, _events) = connect_to(url).await;
        let (mut server, _) = accept(&listener).await;

        // Pause only after the handshake: the paused clock auto-advances
        // whenever the runtime is idle, and doing that mid-dial would fire the
        // 5 s connect timeout before the loopback bytes ever moved.
        tokio::time::pause();
        for _ in 0..2 {
            let msg = tokio::time::timeout(Duration::from_secs(60), server.next())
                .await
                .expect("no keepalive within a minute of virtual time")
                .expect("socket ended")
                .expect("socket error");
            assert_eq!(msg, Message::Text(KEEPALIVE.to_string()));
        }
        stream.abort();
    }

    #[tokio::test]
    async fn no_keepalive_is_sent_once_close_is_requested() {
        let (listener, url) = bind().await;
        let (stream, mut events) = connect_to(url).await;
        let (mut server, _) = accept(&listener).await;

        // Prove the driver is past the dial before stopping: the keepalive
        // interval only exists after open, so a stop landing while
        // connect_async is still resolving would be waited out and open
        // straight into the drain — no keepalive window to leak into, and
        // nothing this test asserts would mean anything. On a loaded machine
        // that raced.
        server.send(results_msg("warm", true)).await.unwrap();
        let SinkEvent::Transcript(..) = next_event(&mut events).await else {
            panic!("expected a transcript event");
        };

        // Park the clock just shy of the first keepalive (+8 s), then stop.
        // The server acknowledges CloseStream only after a VIRTUAL 2 s of
        // silence — so the clock must cross +8 s inside the drain, and a
        // keepalive interval that survived the stop would provably tick in
        // that window and land in `texts_after_close`. Ending the drain with
        // the server's own Close frame (instead of letting the drain time
        // out) keeps this deterministic: the earlier version relied on the
        // 5 s drain cap racing real socket readiness under the paused clock,
        // which auto-advance can win on Windows and kill the connection
        // before the server task ever reads the CloseStream.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(7)).await;

        let server_task = async {
            let mut close_streams = 0u32;
            let mut texts_after_close = Vec::new();
            loop {
                match server.next().await {
                    Some(Ok(Message::Text(t))) => {
                        if t == CLOSE_STREAM {
                            close_streams += 1;
                            // Hold the socket open across the +8 s mark — the
                            // silent window where a leaked keepalive fires —
                            // then close like Deepgram does.
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            let _ = server.send(Message::Close(None)).await;
                        } else if close_streams > 0 {
                            texts_after_close.push(t);
                        } else {
                            panic!("unexpected text before CloseStream: {t}");
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
            (close_streams, texts_after_close)
        };
        let (finalized, (close_streams, texts_after_close)) =
            futures_util::future::join(stream.finalize(), server_task).await;
        assert!(finalized.is_ok());
        assert_eq!(close_streams, 1);
        assert!(
            texts_after_close.is_empty(),
            "sent after CloseStream: {texts_after_close:?}"
        );
    }

    #[tokio::test]
    async fn finalize_is_idempotent_and_sends_one_close_stream() {
        let (listener, url) = bind().await;
        let (stream, mut events) = connect_to(url).await;
        let (mut server, _) = accept(&listener).await;

        server.send(results_msg("Question one.", true)).await.unwrap();
        // Wait for the commit so both finalizes race the close, not the apply.
        let SinkEvent::Transcript(..) = next_event(&mut events).await else {
            panic!("expected a transcript event");
        };

        let server_task = async {
            let mut close_streams = 0u32;
            loop {
                match server.next().await {
                    Some(Ok(Message::Text(t))) => {
                        if t == CLOSE_STREAM {
                            close_streams += 1;
                            if close_streams == 1 {
                                let _ = server.send(Message::Close(None)).await;
                            }
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
            close_streams
        };
        let ((a, b), close_streams) = tokio::time::timeout(
            Duration::from_secs(10),
            futures_util::future::join(
                futures_util::future::join(stream.finalize(), stream.finalize()),
                server_task,
            ),
        )
        .await
        .expect("concurrent finalizes hung");
        assert_eq!(a.unwrap(), "Question one.");
        assert_eq!(b.unwrap(), "Question one.");
        assert_eq!(close_streams, 1, "the second finalize started its own close");
    }

    #[tokio::test]
    async fn a_stop_during_the_dial_still_delivers_the_buffered_question() {
        // WHY: connect() resolves before the handshake, so a stop routinely
        // lands while the socket is still opening (2-5 s of TLS/DNS on a slow
        // network is enough to record a short question and press Stop). What
        // the pre-open channel holds at that moment is the user's ENTIRE
        // question; abandoning the dial discarded it, finalized to "", and had
        // the session answer "Make sure call audio is playing" for audio that
        // flowed perfectly.
        let (listener, url) = bind().await;
        let (stream, mut events) = connect_to(url).await;

        // Nothing has accepted, so the socket provably is not open: these
        // frames are the captured question sitting in the pre-open buffer.
        stream.send_audio(&[1i16; 8]);
        stream.send_audio(&[2i16; 8]);

        let server_task = async {
            // `join` polls finalize first, so close_requested is set before
            // this handshake can complete: the dial resolves INTO a pending
            // stop, which is the case under test — no timing, no sleeps.
            let (mut server, _) = accept(&listener).await;
            let mut buffered = Vec::new();
            loop {
                match server.next().await.expect("client hung up early").unwrap() {
                    Message::Binary(bytes) => {
                        buffered.push(i16::from_le_bytes([bytes[0], bytes[1]]))
                    }
                    Message::Text(t) if t == CLOSE_STREAM => break,
                    other => panic!("unexpected frame before CloseStream: {other:?}"),
                }
            }
            // The tail the server was holding: it can only reach the user if
            // the dial was seen through and CloseStream actually went out.
            server
                .send(results_msg("Tell me about yourself.", true))
                .await
                .unwrap();
            server.send(Message::Close(None)).await.unwrap();
            buffered
        };
        let (finalized, buffered) = tokio::time::timeout(
            Duration::from_secs(10),
            futures_util::future::join(stream.finalize(), server_task),
        )
        .await
        .expect("finalize hung on a dial that a stop was waiting out");

        assert_eq!(buffered, vec![1, 2], "the pre-open buffer never reached the wire");
        assert_eq!(finalized.unwrap(), "Tell me about yourself.");
        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(event, SinkEvent::Error(_)),
                "a dial that completed normally surfaced an error: {event:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_dial_that_fails_after_a_stop_surfaces_stt_connect() {
        // WHY: the stop is pending, but the recording is unusable BECAUSE the
        // connect failed. Silence here (the old dial-phase gate) left finalize
        // returning "" and the session reporting no_speech — sending the user
        // to debug their call audio for a network fault.
        let (listener, url) = bind().await;
        let (stream, mut events) = connect_to(url).await;
        stream.send_audio(&[9i16; 8]);

        let server_task = async {
            // Accept the TCP connection and drop it without ever answering the
            // upgrade: a dial failure driven by the server, so no test waits on
            // the 5 s connect timeout to decide the outcome.
            let (tcp, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .expect("client never dialed")
                .expect("tcp accept failed");
            drop(tcp);
        };
        let (finalized, ()) = tokio::time::timeout(
            Duration::from_secs(10),
            futures_util::future::join(stream.finalize(), server_task),
        )
        .await
        .expect("finalize hung on a failed dial");
        assert_eq!(finalized.unwrap(), "");

        // The driver reports strictly before it exits and finalize joins that
        // exit, so every event it will ever send is already queued here.
        let mut errors = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let SinkEvent::Error(err) = event {
                errors.push(err);
            }
        }
        assert_eq!(errors.len(), 1, "expected exactly one error, got {errors:?}");
        assert_eq!(errors[0].code, ErrorCode::SttConnect);
        assert!(
            errors[0].message.ends_with("Check the API key and your network."),
            "wrong tail: {}",
            errors[0].message
        );
    }

    #[tokio::test]
    async fn an_abort_during_the_dial_abandons_it_silently() {
        // The counterpart to the stop case: an abort means nobody is waiting
        // for this transcript, so there is nothing worth waiting out — the
        // dial is dropped where it stands and the death the caller asked for
        // is not news. The listener never accepts, so the handshake provably
        // cannot have completed.
        let (_listener, url) = bind().await;
        let (stream, mut events) = connect_to(url).await;
        stream.send_audio(&[7i16; 160]);

        stream.abort();
        // finalize() is only the join point for the driver's exit here (the
        // abort flag is already set, and it is the one the dial loop reads).
        let started = std::time::Instant::now();
        let text = tokio::time::timeout(Duration::from_secs(10), stream.finalize())
            .await
            .expect("finalize hung after an aborted dial")
            .unwrap();
        assert_eq!(text, "");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "an aborted dial was waited out instead of abandoned: {:?}",
            started.elapsed()
        );
        assert!(events.try_recv().is_err(), "an aborted dial surfaced an error");
    }

    #[tokio::test]
    async fn close_1008_before_open_is_a_connect_failure_and_finalize_is_instant() {
        let (listener, url) = bind().await;
        let (stream, mut events) = connect_to(url).await;
        let (mut server, _) = accept(&listener).await;

        // Deepgram's reject-by-close: handshake accepted, then 1008 with a
        // DATA-xxxx reason and no Error frame at all.
        server
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Policy,
                reason: "DATA-0000 Invalid credentials".into(),
            })))
            .await
            .unwrap();

        let SinkEvent::Error(err) = next_event(&mut events).await else {
            panic!("expected an error event");
        };
        assert_eq!(err.code, ErrorCode::SttConnect);
        assert!(err.message.contains("1008"), "no close code in: {}", err.message);
        assert!(
            err.message.contains("DATA-0000 Invalid credentials"),
            "no close reason in: {}",
            err.message
        );
        assert!(
            err.message.ends_with("Check the API key and your network."),
            "wrong tail: {}",
            err.message
        );

        // Already dead: finalize must not wait out the cap for a tail that
        // cannot arrive.
        let started = std::time::Instant::now();
        assert_eq!(stream.finalize().await.unwrap(), "");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "finalize burned the cap on a dead stream: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_mid_stream_error_frame_surfaces_stt_error_with_the_detail() {
        let (listener, url) = bind().await;
        let (stream, mut events) = connect_to(url).await;
        let (mut server, _) = accept(&listener).await;

        // Establish first so this reads as a mid-call failure, not a rejected
        // connect.
        server.send(results_msg("hello", false)).await.unwrap();
        let SinkEvent::Transcript(..) = next_event(&mut events).await else {
            panic!("expected a transcript event");
        };

        server
            .send(Message::Text(
                r#"{"type":"Error","code":"DATA-0000","description":"Bad audio format"}"#.into(),
            ))
            .await
            .unwrap();

        let SinkEvent::Error(err) = next_event(&mut events).await else {
            panic!("expected an error event");
        };
        assert_eq!(err.code, ErrorCode::SttError);
        assert!(err.message.contains("DATA-0000"), "detail lost: {}", err.message);
        assert!(err.message.contains("Bad audio format"), "detail lost: {}", err.message);
        drop(stream);
    }

    #[tokio::test]
    async fn abort_reports_no_error() {
        let (listener, url) = bind().await;
        let (stream, mut events) = connect_to(url).await;
        let (mut server, _) = accept(&listener).await;

        server.send(results_msg("hi", false)).await.unwrap();
        let SinkEvent::Transcript(..) = next_event(&mut events).await else {
            panic!("expected a transcript event");
        };

        stream.abort();
        // Wait until the server observes the teardown: the driver reports (or
        // not) strictly before it drops the socket, so once the death is
        // visible here, any error would already be in the channel.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match server.next().await {
                    Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                }
            }
        })
        .await
        .expect("server never saw the abort");

        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(event, SinkEvent::Error(_)),
                "abort surfaced an error: {event:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_error_frame_during_the_drain_surfaces_stt_error() {
        // WHY (§6.1/§5.5): Deepgram can answer CloseStream with an Error frame
        // and then a clean Close. Swallowing the Error and treating the Close
        // as a successful finalize hands back a truncated transcript — or, if
        // empty, sends the user debugging their call audio ("no speech") for a
        // server-side flush failure.
        let (listener, url) = bind().await;
        let (stream, mut events) = connect_to(url).await;
        let (mut server, _) = accept(&listener).await;

        server.send(results_msg("Question so far.", true)).await.unwrap();
        let SinkEvent::Transcript(..) = next_event(&mut events).await else {
            panic!("expected a transcript event");
        };

        let server_task = async {
            loop {
                match server.next().await.expect("client hung up early").unwrap() {
                    Message::Text(t) if t == CLOSE_STREAM => break,
                    Message::Text(_) | Message::Binary(_) => {}
                    other => panic!("unexpected frame: {other:?}"),
                }
            }
            server
                .send(Message::Text(
                    r#"{"type":"Error","description":"tail flush failed","variant":"NET-0002"}"#
                        .to_string(),
                ))
                .await
                .unwrap();
            let _ = server.send(Message::Close(None)).await;
        };
        let (finalized, ()) = futures_util::future::join(stream.finalize(), server_task).await;

        // finalize still resolves with what was heard — the error is the
        // machine's cue to fail the session, not a reason to lose the text.
        assert_eq!(finalized.unwrap(), "Question so far.");
        let err = loop {
            match next_event(&mut events).await {
                SinkEvent::Error(err) => break err,
                SinkEvent::Transcript(..) => {}
            }
        };
        assert_eq!(err.code, ErrorCode::SttError);
        assert!(err.message.contains("while finalizing"), "wrong phase: {}", err.message);
        assert!(err.message.contains("NET-0002"), "detail lost: {}", err.message);
        assert!(err.message.contains("tail flush failed"), "detail lost: {}", err.message);
    }

    #[tokio::test]
    async fn audio_queued_at_stop_is_flushed_before_close_stream() {
        // WHY: frames captured between the pump's last poll and the stop are
        // the final words of the question — exactly what the user is asking
        // about. The old drain dropped them at the loop break; every one must
        // reach the wire before CloseStream.
        let (listener, url) = bind().await;
        let (stream, mut events) = connect_to(url).await;
        let (mut server, _) = accept(&listener).await;

        server.send(results_msg("start", true)).await.unwrap();
        let SinkEvent::Transcript(..) = next_event(&mut events).await else {
            panic!("expected a transcript event");
        };

        // No await between these sends: on the current-thread test runtime the
        // driver stays parked, so every frame is still queued when finalize
        // flips the flag and the driver breaks straight into the drain.
        for i in 0..50i16 {
            stream.send_audio(&[i]);
        }

        let server_task = async {
            let mut binaries_before_close_stream = 0u32;
            loop {
                match server.next().await.expect("client hung up early").unwrap() {
                    Message::Binary(_) => binaries_before_close_stream += 1,
                    Message::Text(t) if t == CLOSE_STREAM => break,
                    Message::Text(_) => {}
                    other => panic!("unexpected frame: {other:?}"),
                }
            }
            let _ = server.send(Message::Close(None)).await;
            binaries_before_close_stream
        };
        let (finalized, flushed) =
            futures_util::future::join(stream.finalize(), server_task).await;

        assert!(finalized.is_ok());
        assert_eq!(flushed, 50, "queued frames must be flushed before CloseStream");
    }
}

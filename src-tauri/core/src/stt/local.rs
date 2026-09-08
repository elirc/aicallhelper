//! Moonshine speech over loopback. PCM callbacks never wait on inference.
use super::{SttConnector, SttSink, SttStream};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::session::limits;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{mpsc, watch, Notify};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

pub const URL: &str = "ws://127.0.0.1:8765/transcribe";
pub struct LocalConnector;
struct Shared {
    finish: AtomicBool,
    overflow: AtomicBool,
    wake: Notify,
    abort: CancellationToken,
}
struct LocalStream {
    audio: mpsc::Sender<Vec<u8>>,
    shared: Arc<Shared>,
    result: watch::Receiver<Option<AppResult<String>>>,
}
fn failure(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::SttError, message)
}

#[async_trait]
impl SttConnector for LocalConnector {
    async fn connect(&self, sink: Arc<dyn SttSink>) -> AppResult<Box<dyn SttStream>> {
        connect_to(URL.to_owned(), sink).await
    }
}

async fn connect_to(url: String, sink: Arc<dyn SttSink>) -> AppResult<Box<dyn SttStream>> {
    let (audio, rx) = mpsc::channel(128);
    let (result_tx, result) = watch::channel(None);
    let shared = Arc::new(Shared {
        finish: AtomicBool::new(false),
        overflow: AtomicBool::new(false),
        wake: Notify::new(),
        abort: CancellationToken::new(),
    });
    let driver = shared.clone();
    tokio::spawn(async move {
        let result = tokio::select! {
            biased;
            _ = driver.abort.cancelled() => Err(AppError::aborted()),
            result = run(&url, rx, &driver, &*sink) => result,
        };
        if let Err(error) = &result {
            if !driver.abort.is_cancelled() {
                sink.on_error(error.clone());
            }
        }
        let _ = result_tx.send(Some(result));
    });
    Ok(Box::new(LocalStream {
        audio,
        shared,
        result,
    }))
}

#[async_trait]
impl SttStream for LocalStream {
    fn send_audio(&self, pcm: &[i16]) {
        if self.shared.finish.load(Ordering::Acquire) || self.shared.abort.is_cancelled() {
            return;
        }
        // Reserve before constructing the frame. Closing the receiver at Stop
        // waits for all outstanding reservations, so accepted tail audio flushes.
        match self.audio.try_reserve() {
            Ok(slot) => {
                let bytes = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
                slot.send(bytes);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.shared.overflow.store(true, Ordering::Release);
                self.shared.wake.notify_one();
            }
            Err(_) => {}
        }
    }
    async fn finalize(&self) -> AppResult<String> {
        self.shared.finish.store(true, Ordering::Release);
        self.shared.wake.notify_one();
        let mut result = self.result.clone();
        let completion =
            tokio::time::timeout(limits::STT_FINALIZE, result.wait_for(Option::is_some)).await;
        match completion {
            Ok(Ok(value)) => value.as_ref().unwrap().clone(),
            Ok(Err(_)) => Err(failure("Local speech service stopped unexpectedly.")),
            Err(_) => {
                self.shared.abort.cancel();
                Err(AppError::new(
                    ErrorCode::SttTimeout,
                    "Local transcription took too long to finish. Try a shorter recording.",
                ))
            }
        }
    }
    fn abort(&self) {
        self.shared.abort.cancel();
    }
}
impl Drop for LocalStream {
    fn drop(&mut self) {
        self.abort();
    }
}

async fn run(
    url: &str,
    mut audio: mpsc::Receiver<Vec<u8>>,
    shared: &Shared,
    sink: &dyn SttSink,
) -> AppResult<String> {
    let handshake = async {
        let (mut ws, _) =
            connect_async(url).await.map_err(|_| {
                AppError::new(ErrorCode::SttConnect,
            "Local speech is unavailable. Open Settings and start and warm free local mode.")
            })?;
        match ws.next().await {
            Some(Ok(Message::Text(raw))) => {
                let v: Value = serde_json::from_str(&raw)
                    .map_err(|_| failure("Invalid local speech handshake."))?;
                if v["type"] != "ready" || v["protocol"] != 1 {
                    return Err(failure("Local speech is busy or incompatible. Wait for the previous recording to finish, then retry."));
                }
            }
            _ => return Err(failure("Local speech closed before it was ready.")),
        }
        Ok(ws)
    };
    let mut ws = tokio::time::timeout(limits::STT_CONNECT, handshake)
        .await
        .map_err(|_| {
            AppError::new(
                ErrorCode::SttConnect,
                "Local speech did not become ready. Start and warm it in Settings.",
            )
        })??;
    loop {
        if shared.overflow.load(Ordering::Acquire) {
            return Err(failure(
                "Local speech could not keep up with audio. Close other CPU-heavy apps and retry.",
            ));
        }
        if shared.finish.load(Ordering::Acquire) {
            break;
        }
        tokio::select! {
            _ = shared.wake.notified() => {},
            frame = audio.recv() => match frame {
                Some(bytes) => ws.send(Message::Binary(bytes)).await.map_err(|_| failure("Lost the local speech connection."))?,
                None => return Err(AppError::aborted()),
            },
            incoming = ws.next() => { read_frame(incoming, sink, false)?; }
        }
    }
    audio.close();
    let drain = async {
        while let Some(bytes) = audio.recv().await {
            ws.send(Message::Binary(bytes))
                .await
                .map_err(|_| failure("Lost local speech while sending the final audio."))?;
        }
        if shared.overflow.load(Ordering::Acquire) {
            return Err(failure(
                "Local speech fell behind; the recording was stopped.",
            ));
        }
        ws.send(Message::Text(r#"{"type":"finish"}"#.to_owned()))
            .await
            .map_err(|_| failure("Could not finalize local speech."))?;
        loop {
            if let Some(text) = read_frame(ws.next().await, sink, true)? {
                let _ = ws.close(None).await;
                return Ok(text);
            }
        }
    };
    tokio::time::timeout(limits::STT_FINALIZE, drain)
        .await
        .map_err(|_| failure("Local speech did not finish in time."))?
}

fn read_frame(
    incoming: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    sink: &dyn SttSink,
    finishing: bool,
) -> AppResult<Option<String>> {
    match incoming {
        Some(Ok(Message::Text(raw))) => {
            let v: Value = serde_json::from_str(&raw)
                .map_err(|_| failure("Local speech returned invalid text."))?;
            match v["type"].as_str() {
                Some("transcript") => {
                    let text = v["text"].as_str().ok_or_else(|| failure("Local speech returned an invalid transcript."))?;
                    sink.on_transcript(text.to_owned(), v["final"].as_bool().unwrap_or(false));
                    Ok(None)
                }
                Some("done") if finishing => Ok(Some(v["text"].as_str()
                    .ok_or_else(|| failure("Local speech returned an invalid final transcript."))?.to_owned())),
                Some("error") => Err(failure("Local speech could not transcribe the recording. Check the speech service log and retry.")),
                _ => Err(failure("Unexpected response from local speech.")),
            }
        }
        Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => Ok(None),
        _ => Err(failure(
            "Local speech disconnected before the transcript was complete.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    #[derive(Default)]
    struct Sink {
        transcripts: Mutex<Vec<String>>,
        errors: Mutex<Vec<AppError>>,
    }
    impl SttSink for Sink {
        fn on_transcript(&self, t: String, _: bool) {
            self.transcripts.lock().unwrap().push(t);
        }
        fn on_error(&self, e: AppError) {
            self.errors.lock().unwrap().push(e);
        }
    }
    #[tokio::test]
    async fn early_stop_flushes_audio_and_concurrent_finalize_joins_once() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let sink = Arc::new(Sink::default());
        let stream = connect_to(
            format!("ws://{}", listener.local_addr().unwrap()),
            sink.clone(),
        )
        .await
        .unwrap();
        stream.send_audio(&[1, -2]);
        let server = async {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            ws.send(Message::Text(r#"{"type":"ready","protocol":1}"#.into()))
                .await
                .unwrap();
            assert_eq!(
                ws.next().await.unwrap().unwrap(),
                Message::Binary(vec![1, 0, 254, 255])
            );
            assert_eq!(
                ws.next().await.unwrap().unwrap(),
                Message::Text(r#"{"type":"finish"}"#.into())
            );
            ws.send(Message::Text(
                r#"{"type":"transcript","text":"Hello","final":false}"#.into(),
            ))
            .await
            .unwrap();
            ws.send(Message::Text(
                r#"{"type":"done","text":"Hello world"}"#.into(),
            ))
            .await
            .unwrap();
            let _ = ws.next().await;
        };
        let ((a, b), ()) = tokio::join!(
            async { tokio::join!(stream.finalize(), stream.finalize()) },
            server
        );
        assert_eq!(a.unwrap(), "Hello world");
        assert_eq!(b.unwrap(), "Hello world");
        assert!(sink.errors.lock().unwrap().is_empty());
        assert_eq!(*sink.transcripts.lock().unwrap(), vec!["Hello"]);
    }
    #[tokio::test]
    async fn disconnect_does_not_succeed_with_partial_text() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let sink = Arc::new(Sink::default());
        let stream = connect_to(
            format!("ws://{}", listener.local_addr().unwrap()),
            sink.clone(),
        )
        .await
        .unwrap();
        let server = async {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            ws.send(Message::Text(r#"{"type":"ready","protocol":1}"#.into()))
                .await
                .unwrap();
            let _ = ws.next().await;
            ws.send(Message::Close(None)).await.unwrap();
        };
        let (result, ()) = tokio::join!(stream.finalize(), server);
        assert!(result.is_err());
        assert_eq!(sink.errors.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn abort_ends_pending_connection_without_error_event() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let sink = Arc::new(Sink::default());
        let stream = connect_to(
            format!("ws://{}", listener.local_addr().unwrap()),
            sink.clone(),
        )
        .await
        .unwrap();
        stream.abort();
        assert!(stream.finalize().await.unwrap_err().is_aborted());
        assert!(sink.errors.lock().unwrap().is_empty());
    }
}

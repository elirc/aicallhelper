//! Groq provider (§6.3): streams answers from gpt-oss over Groq's
//! OpenAI-compatible chat completions API.
//!
//! The same two invariants as the Anthropic provider hold here:
//!
//! * **Every text delta reaches the sink the moment it is decoded** — the
//!   product promise is first-word latency, so nothing is ever batched.
//! * **The returned answer is the byte-for-byte concatenation of the deltas**
//!   pushed to the sink, so the panel never shows text that later changes.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::error::{no_llm_key_message, AppError, AppResult, ErrorCode};
use crate::llm::{
    http, retry, warm, AnswerRequest, LlmProvider, LlmProviderKind, LlmSink, SseDecoder, SseEvent,
    MAX_ANSWER_TOKENS,
};

/// Pinned in exactly one place. Groq retires models on short notice, so a 404
/// from this provider most likely means this constant needs updating — the
/// 404 mapping below says exactly that.
pub const GROQ_MODEL: &str = "openai/gpt-oss-120b";

const DEFAULT_BASE_URL: &str = "https://api.groq.com";

/// Kept as constants so the retry predicate can recognize a connection-level
/// failure by equality instead of by fragile substring matching.
const MSG_CONNECT_FAILED: &str = "Could not reach Groq. Check your internet connection.";
const MSG_STREAM_DROPPED: &str =
    "The connection dropped while the answer was streaming. Try again.";

pub struct GroqProvider {
    api_key: String,
    base_url: String,
}

impl GroqProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), base_url: DEFAULT_BASE_URL.to_string() }
    }

    /// Escape hatch so tests can point the provider at a local server.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    fn request_body(&self, req: &AnswerRequest) -> Value {
        json!({
            "model": GROQ_MODEL,
            "stream": true,
            "temperature": 0.7,
            // Spoken answers are short; an uncapped completion is pure tail
            // latency (§6.2).
            "max_completion_tokens": MAX_ANSWER_TOKENS,
            // gpt-oss is a reasoning model, and reasoning is the enemy of
            // time-to-first-word: every reasoning token is a token the user
            // waits through before the first spoken word exists. These two
            // are the supported knobs for this model family. Do NOT send
            // `reasoning_format` — that is a Qwen-family knob.
            "reasoning_effort": "low",
            "include_reasoning": false,
            "messages": [
                // Groq takes a single system string; joined() keeps the two
                // halves reading exactly as the two Anthropic blocks do.
                {"role": "system", "content": req.system.joined()},
                {"role": "user", "content": req.user_message()}
            ],
        })
    }

    /// One attempt: send the request and drain the SSE stream into the sink.
    async fn stream_once(
        &self,
        body: &Value,
        sink: &Arc<dyn LlmSink>,
        cancel: &CancellationToken,
        delivered: &retry::Attempt,
    ) -> AppResult<String> {
        let request = http::shared_client()
            .post(format!("{}/openai/v1/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(body);

        // Abort is checked before any failure is mapped: a user who pressed
        // stop must see silence, not a scary network error their own cancel
        // caused. `biased` makes the cancel arm win a tie.
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(AppError::aborted()),
            result = request.send() => {
                result.map_err(|_| AppError::new(ErrorCode::LlmHttp, MSG_CONNECT_FAILED))?
            }
        };

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            // The body read is raced against cancel like every other await: a
            // user who pressed stop must not wait on a slow error body just to
            // have the result thrown away.
            let body_text = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AppError::aborted()),
                text = response.text() => text.unwrap_or_default(),
            };
            return Err(map_status(status, &body_text));
        }

        let mut decoder = SseDecoder::new();
        let mut answer = String::new();
        let mut saw_event = false;
        // Box::pin because reqwest only promises `impl Stream`; pinning here
        // keeps `next()` usable without caring whether that type is Unpin.
        let mut stream = Box::pin(response.bytes_stream());

        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AppError::aborted()),
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = next else { break };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(_) => {
                    if cancel.is_cancelled() {
                        return Err(AppError::aborted());
                    }
                    return Err(AppError::new(ErrorCode::LlmHttp, MSG_STREAM_DROPPED));
                }
            };
            for event in decoder.feed(&chunk) {
                saw_event = true;
                apply_event(&event, sink, delivered, &mut answer);
            }
        }
        // A stream can end without a trailing newline; the flushed remainder
        // still carries real events (see sse.rs).
        for event in decoder.finish() {
            saw_event = true;
            apply_event(&event, sink, delivered, &mut answer);
        }

        if !saw_event {
            // A 200 with no SSE events is a broken response, not an empty
            // answer — returning Ok("") here would render as the model
            // silently saying nothing.
            return Err(AppError::new(
                ErrorCode::LlmHttp,
                "Groq returned an empty response (HTTP 200 with an empty body). Try again.",
            ));
        }
        Ok(answer)
    }
}

/// Interpret one OpenAI-style SSE event. The delta is
/// `choices[0].delta.content`; everything else (role priming, finish_reason
/// chunks, usage frames) carries no answer text and is ignored.
fn apply_event(
    event: &SseEvent,
    sink: &Arc<dyn LlmSink>,
    delivered: &retry::Attempt,
    answer: &mut String,
) {
    // `data: [DONE]` is a sentinel to SKIP, not a terminator: bytes after it
    // in the same chunk are still real events. SseDecoder already handles the
    // framing; stopping here would truncate whatever followed.
    if event.is_done_sentinel() {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
        // A single mangled event must not cost the whole answer.
        return;
    };
    if let Some(text) = value["choices"][0]["delta"]["content"].as_str() {
        if text.is_empty() {
            // The role-priming first chunk carries an empty content string;
            // forwarding it wakes the UI for nothing and cannot change the
            // returned answer.
            return;
        }
        delivered.mark_delta_emitted();
        answer.push_str(text);
        // Pushed the moment it is decoded — never batched.
        sink.on_delta(text.to_string());
    }
}

#[async_trait]
impl LlmProvider for GroqProvider {
    async fn stream_answer(
        &self,
        req: &AnswerRequest,
        sink: Arc<dyn LlmSink>,
        cancel: CancellationToken,
    ) -> AppResult<String> {
        if self.api_key.trim().is_empty() {
            // Checked before any network work: a missing key is a settings
            // problem, and opening a connection to discover it wastes the
            // user's time on the worst possible screen.
            return Err(AppError::new(
                ErrorCode::NoLlmKey,
                no_llm_key_message(LlmProviderKind::Groq.label()),
            ));
        }

        // Built once so a retry resends byte-identical bytes (§6.4).
        let body = self.request_body(req);
        let delivered = Arc::new(retry::Attempt::new());

        // Retryable = a connection-level failure: the dial never completed, or
        // the stream dropped. Both are §6.4's "failed at the connection level"
        // — and the dropped-stream case is only safe to retry because
        // `with_retry_once` vetoes any retry after a delta reached the sink
        // (resending after text was painted would concatenate two answers).
        // An HTTP-status failure is deterministic and retrying it just
        // doubles the wait.
        let is_retryable = |err: &AppError| {
            err.code == ErrorCode::LlmHttp
                && (err.message == MSG_CONNECT_FAILED || err.message == MSG_STREAM_DROPPED)
        };
        let run = |_attempt_index: u32| {
            let sink = Arc::clone(&sink);
            let cancel = cancel.clone();
            let delivered = Arc::clone(&delivered);
            let body = &body;
            async move { self.stream_once(body, &sink, &cancel, &delivered).await }
        };
        retry::with_retry_once(&cancel, &delivered, is_retryable, run).await
    }

    fn prewarm(&self) {
        warm::prewarm(&self.base_url);
    }

    fn kind(&self) -> LlmProviderKind {
        LlmProviderKind::Groq
    }
}

/// HTTP status -> user-facing error. Every message names the actual status —
/// a 403 labelled 401 sends the user debugging the wrong thing.
fn map_status(status: u16, body: &str) -> AppError {
    match status {
        401 | 403 => AppError::new(
            ErrorCode::LlmAuth,
            format!("Groq rejected the API key ({status}). Check it in Settings."),
        ),
        429 => AppError::new(
            ErrorCode::LlmRateLimit,
            "Groq rate limit hit (429). Wait a moment before asking again.",
        ),
        // Groq retires models on short notice, so a 404 here most likely
        // means the pinned model is gone, not that the URL is wrong.
        404 => AppError::new(
            ErrorCode::LlmHttp,
            format!(
                "Groq returned 404 for model \"{GROQ_MODEL}\" — the model may have been retired — update the pinned model constant."
            ),
        ),
        s if s >= 500 => AppError::new(
            ErrorCode::LlmHttp,
            format!("Groq is unavailable (HTTP {s}). Try again in a moment."),
        ),
        other => AppError::new(
            ErrorCode::LlmHttp,
            format!("Groq returned HTTP {other}: {}", snippet(body)),
        ),
    }
}

/// Error bodies can be whole HTML pages; 300 chars is enough to act on and
/// short enough to render. Truncation walks back to a char boundary so a
/// multi-byte character split at the limit cannot panic the error path.
fn snippet(body: &str) -> String {
    const MAX: usize = 300;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "(empty body)".to_string();
    }
    if trimmed.len() <= MAX {
        return trimmed.to_string();
    }
    let mut end = MAX;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{build_system_prompt, AnswerStyle, Profile};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;

    // ---- test doubles ------------------------------------------------------

    #[derive(Default)]
    struct RecordingSink {
        deltas: StdMutex<Vec<String>>,
    }

    impl LlmSink for RecordingSink {
        fn on_delta(&self, delta: String) {
            self.deltas.lock().unwrap().push(delta);
        }
    }

    /// Cancels the token on the first delta, to exercise mid-stream abort.
    struct CancellingSink {
        cancel: CancellationToken,
        deltas: StdMutex<Vec<String>>,
    }

    impl LlmSink for CancellingSink {
        fn on_delta(&self, delta: String) {
            self.deltas.lock().unwrap().push(delta);
            self.cancel.cancel();
        }
    }

    fn request() -> AnswerRequest {
        let system = build_system_prompt(
            Profile { resume: "Ten years of Rust.", job_description: "Staff engineer." },
            AnswerStyle::Balanced,
        );
        AnswerRequest::new(system).with_transcript("Tell me about yourself.")
    }

    // ---- local HTTP server: we control the exact bytes ---------------------

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            let n = socket.read(&mut tmp).await.unwrap();
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&buf[..pos]).into_owned();
                let content_length = header
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                let body_end = pos + 4 + content_length;
                while buf.len() < body_end {
                    let n = socket.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                return String::from_utf8_lossy(&buf).into_owned();
            }
            if n == 0 {
                return String::from_utf8_lossy(&buf).into_owned();
            }
        }
    }

    /// Accept one connection, capture the raw request, write `response`, close.
    async fn serve_once(response: Vec<u8>) -> (String, oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let captured = read_request(&mut socket).await;
            socket.write_all(&response).await.unwrap();
            socket.flush().await.unwrap();
            let _ = socket.shutdown().await;
            let _ = tx.send(captured);
        });
        (format!("http://{addr}"), rx)
    }

    /// Write `head`, then hold the connection open forever. Used to prove that
    /// cancellation does not depend on the server sending more bytes.
    async fn serve_then_stall(head: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut socket).await;
            socket.write_all(&head).await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(600)).await;
            drop(socket);
        });
        format!("http://{addr}")
    }

    fn http_response(status: u16, reason: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn sse_response(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// Promise more bytes than are sent, then close: the client observes a
    /// connection cut mid-body rather than a clean end of stream.
    fn dropped_sse_response(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
            body.len() + 64
        )
        .into_bytes()
    }

    fn delta_event(text: &str) -> String {
        format!(
            "data: {{\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\n"
        )
    }

    async fn run(provider: &GroqProvider, sink: Arc<RecordingSink>) -> AppResult<String> {
        provider
            .stream_answer(&request(), sink, CancellationToken::new())
            .await
    }

    /// Accept exactly two sequential connections, answering each with its own
    /// scripted response; returns both captured raw requests.
    async fn serve_twice(
        first: Vec<u8>,
        second: Vec<u8>,
    ) -> (String, oneshot::Receiver<(String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let mut captured = Vec::new();
            for response in [first, second] {
                let (mut socket, _) = listener.accept().await.unwrap();
                captured.push(read_request(&mut socket).await);
                socket.write_all(&response).await.unwrap();
                socket.flush().await.unwrap();
                let _ = socket.shutdown().await;
            }
            let mut it = captured.into_iter();
            let _ = tx.send((it.next().unwrap(), it.next().unwrap()));
        });
        (format!("http://{addr}"), rx)
    }

    async fn status_error(status: u16, reason: &str, body: &str) -> AppError {
        let (base, _rx) = serve_once(http_response(status, reason, body)).await;
        let provider = GroqProvider::new("test-key").with_base_url(base);
        run(&provider, Arc::new(RecordingSink::default()))
            .await
            .unwrap_err()
    }

    // ---- happy path --------------------------------------------------------

    #[tokio::test]
    async fn happy_path_streams_every_delta_and_returns_their_exact_concatenation() {
        let body = format!(
            "data: {{\"id\":\"chatcmpl-1\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"\"}}}}]}}\n\n\
             {}{}\
             data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
             data: [DONE]\n\n",
            delta_event("Hel"),
            delta_event("lo"),
        );
        let (base, _rx) = serve_once(sse_response(&body)).await;
        let provider = GroqProvider::new("test-key").with_base_url(base);
        let sink = Arc::new(RecordingSink::default());
        let answer = run(&provider, sink.clone()).await.unwrap();

        let deltas = sink.deltas.lock().unwrap().clone();
        assert_eq!(deltas, vec!["Hel".to_string(), "lo".to_string()]);
        // The invariant that stops text changing after the user has read it:
        // the returned answer is byte-identical to what streamed into the panel.
        assert_eq!(answer.as_bytes(), deltas.concat().as_bytes());
        assert_eq!(answer, "Hello");
    }

    #[tokio::test]
    async fn done_sentinel_followed_by_more_data_in_the_same_chunk_does_not_truncate() {
        // Everything arrives in ONE write: a decoder or provider loop that
        // treats [DONE] as a terminator drops the trailing " world".
        let body = format!("{}data: [DONE]\n\n{}", delta_event("Hello"), delta_event(" world"));
        let (base, _rx) = serve_once(sse_response(&body)).await;
        let provider = GroqProvider::new("test-key").with_base_url(base);
        let sink = Arc::new(RecordingSink::default());
        let answer = run(&provider, sink.clone()).await.unwrap();
        assert_eq!(answer, "Hello world");
        assert_eq!(answer.as_bytes(), sink.deltas.lock().unwrap().concat().as_bytes());
    }

    // ---- request shape -----------------------------------------------------

    #[tokio::test]
    async fn request_pins_model_streaming_reasoning_knobs_and_omits_reasoning_format() {
        let (base, rx) = serve_once(sse_response(&delta_event("ok"))).await;
        let provider = GroqProvider::new("test-key").with_base_url(base);
        let req = request();
        provider
            .stream_answer(&req, Arc::new(RecordingSink::default()), CancellationToken::new())
            .await
            .unwrap();

        let raw = rx.await.unwrap();
        assert!(raw.contains("Bearer test-key"), "Authorization header missing");

        let json_start = raw.find("\r\n\r\n").unwrap() + 4;
        let v: Value = serde_json::from_str(&raw[json_start..]).unwrap();

        assert_eq!(v["model"], GROQ_MODEL);
        assert_eq!(v["stream"], true);
        assert_eq!(v["temperature"], 0.7);
        assert_eq!(v["max_completion_tokens"], MAX_ANSWER_TOKENS);
        assert_eq!(v["reasoning_effort"], "low");
        assert_eq!(v["include_reasoning"], false);
        // reasoning_format is a Qwen-family knob; sending it to gpt-oss is at
        // best ignored and at worst a request error on a future API revision.
        assert!(v.get("reasoning_format").is_none(), "reasoning_format must not be sent");

        // System prompt goes over as ONE string via joined().
        let messages = v["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], req.system.joined().as_str());
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], req.user_message().as_str());
    }

    // ---- error mapping: one test per status --------------------------------

    #[tokio::test]
    async fn status_401_maps_to_llm_auth_and_names_the_status() {
        let err = status_error(401, "Unauthorized", "{}").await;
        assert_eq!(err.code, ErrorCode::LlmAuth);
        assert!(err.message.contains("401"), "message was: {}", err.message);
    }

    #[tokio::test]
    async fn status_403_maps_to_llm_auth_and_reports_403_not_401() {
        // A 403 labelled 401 sends the user debugging the wrong thing.
        let err = status_error(403, "Forbidden", "{}").await;
        assert_eq!(err.code, ErrorCode::LlmAuth);
        assert!(err.message.contains("403"), "message was: {}", err.message);
        assert!(!err.message.contains("401"), "message was: {}", err.message);
    }

    #[tokio::test]
    async fn status_429_maps_to_llm_rate_limit_and_names_the_status() {
        let err = status_error(429, "Too Many Requests", "{}").await;
        assert_eq!(err.code, ErrorCode::LlmRateLimit);
        assert!(err.message.contains("429"), "message was: {}", err.message);
    }

    #[tokio::test]
    async fn status_404_maps_to_llm_http_and_points_at_the_pinned_model_constant() {
        let err = status_error(404, "Not Found", "{}").await;
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(err.message.contains("404"), "message was: {}", err.message);
        assert!(err.message.contains("retired"), "message was: {}", err.message);
        assert!(
            err.message.contains("pinned model constant"),
            "message was: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn status_500_maps_to_llm_http_groq_unavailable() {
        let err = status_error(500, "Internal Server Error", "{}").await;
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(err.message.contains("500"), "message was: {}", err.message);
        assert!(err.message.contains("unavailable"), "message was: {}", err.message);
    }

    #[tokio::test]
    async fn status_503_also_maps_to_llm_http_groq_unavailable() {
        let err = status_error(503, "Service Unavailable", "{}").await;
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(err.message.contains("503"), "message was: {}", err.message);
        assert!(err.message.contains("unavailable"), "message was: {}", err.message);
    }

    #[tokio::test]
    async fn connection_failure_maps_to_llm_http_with_actionable_message() {
        // Bind a port then release it so nothing is listening there.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let provider = GroqProvider::new("test-key").with_base_url(format!("http://{addr}"));
        let err = run(&provider, Arc::new(RecordingSink::default()))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(
            err.message.contains("Could not reach Groq"),
            "message was: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn empty_200_body_is_llm_http_not_a_crash_and_not_a_silent_empty_answer() {
        let (base, _rx) = serve_once(sse_response("")).await;
        let provider = GroqProvider::new("test-key").with_base_url(base);
        let err = run(&provider, Arc::new(RecordingSink::default()))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::LlmHttp);
    }

    #[tokio::test]
    async fn mid_stream_connection_drop_maps_to_llm_http_with_dropped_wording() {
        let (base, _rx) = serve_once(dropped_sse_response(&delta_event("Hel"))).await;
        let provider = GroqProvider::new("test-key").with_base_url(base);
        let sink = Arc::new(RecordingSink::default());
        let err = run(&provider, sink.clone()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(
            err.message.contains("connection dropped"),
            "message was: {}",
            err.message
        );
        // The delta that made it through was still delivered live.
        assert_eq!(sink.deltas.lock().unwrap().clone(), vec!["Hel".to_string()]);
    }

    #[tokio::test]
    async fn stream_drop_before_any_delta_is_retried_once_with_an_identical_body() {
        // WHY (§6.4): a 200 whose stream dies before ANY answer text reached
        // the sink is a connection-level failure with nothing at risk of
        // duplication — the one case where a silent retry beats surfacing an
        // error one second into the latency window. The concatenation guard
        // (delta emitted -> no retry) is what makes this safe, and the
        // mid_stream_connection_drop test above proves that guard's side.
        let first = dropped_sse_response(": stream cut before any delta\n\n");
        let second = sse_response(&format!("{}data: [DONE]\n\n", delta_event("Recovered.")));
        let (base, requests) = serve_twice(first, second).await;

        let provider = GroqProvider::new("test-key").with_base_url(base);
        let sink = Arc::new(RecordingSink::default());
        let answer = run(&provider, sink.clone()).await.expect("retry must recover");

        assert_eq!(answer, "Recovered.");
        assert_eq!(sink.deltas.lock().unwrap().clone(), vec!["Recovered.".to_string()]);
        let (req1, req2) = requests.await.expect("both connections served");
        let body_of = |raw: &str| raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        assert_eq!(body_of(&req1), body_of(&req2), "the retried body must be byte-identical");
    }

    // ---- cancellation ------------------------------------------------------

    #[tokio::test]
    async fn cancellation_mid_stream_returns_aborted_never_an_http_error() {
        // Server sends one delta then stalls with the socket held open; the
        // sink cancels on that first delta. If cancellation required the
        // server to send more bytes, this test would hang.
        let head = dropped_sse_response(&delta_event("Hel"));
        let base = serve_then_stall(head).await;
        let provider = GroqProvider::new("test-key").with_base_url(base);

        let cancel = CancellationToken::new();
        let sink = Arc::new(CancellingSink {
            cancel: cancel.clone(),
            deltas: StdMutex::new(Vec::new()),
        });
        let err = provider
            .stream_answer(&request(), sink, cancel)
            .await
            .unwrap_err();
        assert!(err.is_aborted(), "expected aborted, got: {err}");
        assert_ne!(err.code, ErrorCode::LlmHttp);
    }

    // ---- key handling ------------------------------------------------------

    #[tokio::test]
    async fn empty_api_key_returns_no_llm_key_without_connecting() {
        // base_url points at a dead port: if the provider tried to connect it
        // would surface a connection error, not NoLlmKey.
        let provider = GroqProvider::new("").with_base_url("http://127.0.0.1:1");
        let err = run(&provider, Arc::new(RecordingSink::default()))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NoLlmKey);
        assert!(err.message.contains("Groq"), "message was: {}", err.message);
    }
}

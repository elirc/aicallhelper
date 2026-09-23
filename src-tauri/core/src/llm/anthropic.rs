//! Anthropic provider (§6.2): streams answers from Claude over the Messages
//! API.
//!
//! Two invariants shape this file:
//!
//! * **Every text delta reaches the sink the moment it is decoded.** The whole
//!   product promise is first-word latency; batching deltas here would throw
//!   away everything the streaming pipeline bought.
//! * **The returned answer is the byte-for-byte concatenation of the deltas.**
//!   The UI renders the deltas live and then the returned string; any
//!   divergence shows up to the user as text changing after they have read it.
//! * **Only `message_stop` finishes an answer (R2).** The `stop_reason` in
//!   `message_delta` is metadata about WHY the model stopped; it does not prove
//!   the final protocol event arrived. A clean end of stream before
//!   `message_stop` is an incomplete answer, and nothing after `message_stop`
//!   is read.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::error::{no_llm_key_message, AppError, AppResult, ErrorCode};
use crate::llm::{
    ended_early, http, malformed_frame, oversized_frame, retry, warm, Answer, AnswerRequest,
    LlmProvider, LlmProviderKind, LlmSink, SseDecoder, SseEvent, StopReason, MAX_ANSWER_TOKENS,
};

/// Used in the R2 outcome messages ("Anthropic stopped sending …").
const LABEL: &str = "Anthropic";

/// Pinned in exactly one place. Haiku because time-to-first-word is the
/// product; a bigger model buys quality this use case cannot spend.
pub const ANTHROPIC_MODEL: &str = "claude-haiku-4-5";

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Kept as constants so the retry predicate can recognize a connection-level
/// failure by equality instead of by fragile substring matching.
const MSG_CONNECT_FAILED: &str = "Could not reach Anthropic. Check your internet connection.";
const MSG_STREAM_DROPPED: &str =
    "The connection to Anthropic dropped while the answer was streaming. Try again.";

pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    /// `usage.cache_read_input_tokens` from the most recent response — the
    /// only honest answer to "did the prompt cache actually engage?" (see the
    /// note in `request_body`, where the 4096-token minimum makes the
    /// breakpoint a likely no-op).
    ///
    /// Deliberately **not** surfaced in the product: nothing reads the getter
    /// outside tests, and a provider is built per session, so this reports on
    /// one session and then drops. It exists for the tests that pin the
    /// parsing and for attaching a debugger when the cache math is in doubt.
    /// Surfacing it in the metrics panel is the natural follow-up (IDEAS #3).
    /// The number is safe to log; the prompt itself never is.
    last_cache_read_input_tokens: Mutex<Option<u64>>,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            last_cache_read_input_tokens: Mutex::new(None),
        }
    }

    /// Escape hatch so tests can point the provider at a local server.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Whether the last response reported a prompt-cache read — for tests and
    /// debugger sessions (see the field doc: nothing in the product reads it).
    /// `None` until a response has carried the field.
    pub fn last_cache_read_input_tokens(&self) -> Option<u64> {
        *self.last_cache_read_input_tokens.lock().unwrap()
    }

    fn request_body(&self, req: &AnswerRequest) -> Value {
        // The system prompt goes over as TWO blocks with the cache breakpoint
        // after the first: the breakpoint sits AFTER the resume+JD block and
        // the style policy sits after it, so flipping answer style never
        // invalidates the cached profile (§3).
        //
        // Honesty note: Haiku 4.5's minimum cacheable prefix is 4096 tokens,
        // so a typical 1–2K-token profile makes this cache_control marker a
        // silent no-op — the request still works, it just isn't cached. It
        // starts paying at roughly 16K+ characters of profile (writes cost
        // 1.25x, reads 0.1x, 5-minute TTL). `usage.cache_read_input_tokens`
        // in the response tells the truth about whether it engaged; we parse
        // it (see apply_event) and expose it for tests and debugging.
        json!({
            "model": ANTHROPIC_MODEL,
            // Spoken answers are short; an uncapped completion is pure tail
            // latency (§6.2).
            "max_tokens": MAX_ANSWER_TOKENS,
            "stream": true,
            "system": [
                {
                    "type": "text",
                    "text": req.system.cached_prefix,
                    "cache_control": {"type": "ephemeral"}
                },
                {
                    "type": "text",
                    "text": req.system.style_suffix
                }
            ],
            "messages": [
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
    ) -> AppResult<Answer> {
        let request = http::shared_client()
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
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
            // have the result thrown away. It is also capped while reading
            // (R2): an error page is never held whole.
            let body_text = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AppError::aborted()),
                text = http::read_error_body(response) => text,
            };
            return Err(map_status(status, &body_text));
        }

        let mut decoder = SseDecoder::new();
        let mut progress = StreamProgress::default();
        // Box::pin because reqwest only promises `impl Stream`; pinning here
        // keeps `next()` usable without caring whether that type is Unpin.
        let mut stream = Box::pin(response.bytes_stream());

        'read: loop {
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
            // On overflow, the events framed before the oversized line are
            // still applied (so the kept partial text does not depend on
            // chunking), and the stream fails right after them.
            let (events, overflowed) = match decoder.feed(&chunk) {
                Ok(events) => (events, false),
                Err(overflow) => (overflow.decoded, true),
            };
            for event in events {
                self.apply_event(&event, sink, delivered, &mut progress)?;
                if progress.finished {
                    // `message_stop` is the last protocol event: nothing after
                    // it may become answer text, in this chunk or a later one.
                    break 'read;
                }
            }
            if overflowed {
                return Err(oversized_frame(LABEL));
            }
        }
        if !progress.finished {
            // A stream can end without a trailing newline; the flushed
            // remainder still carries real events (see sse.rs).
            let (events, overflowed) = match decoder.finish() {
                Ok(events) => (events, false),
                Err(overflow) => (overflow.decoded, true),
            };
            for event in events {
                self.apply_event(&event, sink, delivered, &mut progress)?;
                if progress.finished {
                    break;
                }
            }
            if overflowed && !progress.finished {
                return Err(oversized_frame(LABEL));
            }
        }
        progress.conclude()
    }

    /// Interpret one SSE event. Only `content_block_delta` / `text_delta`
    /// carries answer text; the final answer is the concatenation of ALL text
    /// deltas across ALL content blocks joined with NOTHING between blocks,
    /// so block boundaries are invisible here on purpose.
    fn apply_event(
        &self,
        event: &SseEvent,
        sink: &Arc<dyn LlmSink>,
        delivered: &retry::Attempt,
        progress: &mut StreamProgress,
    ) -> AppResult<()> {
        progress.saw_event = true;
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            // Every Anthropic event is a JSON object. One that does not parse
            // may have carried answer text; skipping it and later reporting a
            // finished answer is the silent truncation R2 exists to prevent.
            return Err(malformed_frame(LABEL));
        };
        match value["type"].as_str() {
            Some("content_block_delta") => {
                if value["delta"]["type"].as_str() == Some("text_delta") {
                    let Some(text) = value["delta"]["text"].as_str() else {
                        // A text delta with no text is a malformed known frame.
                        return Err(malformed_frame(LABEL));
                    };
                    if text.is_empty() {
                        // Forwarding an empty delta wakes the UI for
                        // nothing and cannot change the returned answer.
                        return Ok(());
                    }
                    delivered.mark_delta_emitted();
                    progress.answer.push_str(text);
                    // Pushed the moment it is decoded — never batched.
                    sink.on_delta(text.to_string());
                }
                // Other delta types (thinking, citations, input_json) carry
                // no spoken answer text and are harmless.
            }
            Some("error") => {
                // Mid-stream server error. Surface the detail; without it the
                // user sees a truncated answer and blames the app.
                let detail = value["error"]["message"].as_str().unwrap_or("unknown error");
                return Err(AppError::new(
                    ErrorCode::LlmHttp,
                    format!("Anthropic reported an error mid-stream: {detail}"),
                ));
            }
            Some("message_start") | Some("message_delta") => {
                // The only trustworthy signal for whether the prompt cache
                // engaged (see the honesty note in request_body). The count is
                // exposed for tests and debugging; the prompt itself never is.
                let usage = if value["type"] == "message_start" {
                    &value["message"]["usage"]
                } else {
                    // Why the model stopped: metadata only. It does NOT
                    // complete the answer — only `message_stop` does (R2).
                    if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                        progress.stop_reason = Some(reason.to_string());
                    }
                    &value["usage"]
                };
                if let Some(n) = usage["cache_read_input_tokens"].as_u64() {
                    *self.last_cache_read_input_tokens.lock().unwrap() = Some(n);
                }
            }
            Some("message_stop") => progress.finished = true,
            // content_block_start / content_block_stop / ping, and any event
            // type added after this was written: structure and keep-alives,
            // no answer text. Harmless by design.
            _ => {}
        }
        Ok(())
    }
}

/// What one attempt has seen so far. The four R2 facts — protocol completion,
/// answer text, stop reason, and whether anything arrived at all — are kept
/// apart so no one of them can stand in for another.
#[derive(Default)]
struct StreamProgress {
    answer: String,
    stop_reason: Option<String>,
    /// `message_stop` arrived.
    finished: bool,
    saw_event: bool,
}

impl StreamProgress {
    /// Classify the attempt once the stream is over (R2).
    fn conclude(self) -> AppResult<Answer> {
        if !self.finished {
            if !self.saw_event {
                // A 200 with no SSE events is a broken response, not an empty
                // answer — returning Ok("") here would render as the model
                // silently saying nothing.
                return Err(AppError::new(
                    ErrorCode::LlmHttp,
                    "Anthropic returned an empty response. Try again.",
                ));
            }
            return Err(ended_early(LABEL));
        }
        Answer::from_terminal(
            LABEL,
            self.answer,
            StopReason::from_provider(self.stop_reason.as_deref()),
        )
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn stream_answer(
        &self,
        req: &AnswerRequest,
        sink: Arc<dyn LlmSink>,
        cancel: CancellationToken,
    ) -> AppResult<Answer> {
        if self.api_key.trim().is_empty() {
            // Checked before any network work: a missing key is a settings
            // problem, and opening a connection to discover it wastes the
            // user's time on the worst possible screen.
            return Err(AppError::new(
                ErrorCode::NoLlmKey,
                no_llm_key_message(LlmProviderKind::Anthropic.label()),
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
        LlmProviderKind::Anthropic
    }
}

/// HTTP status -> user-facing error. Every message names the actual status so
/// the user (and our own logs) never debug the wrong failure.
fn map_status(status: u16, body: &str) -> AppError {
    match status {
        401 => AppError::new(
            ErrorCode::LlmAuth,
            "Anthropic rejected the API key (401). Check it in Settings.",
        ),
        403 => AppError::new(
            ErrorCode::LlmAuth,
            "Anthropic refused the request (403): this API key is not allowed to use the model. Check it in Settings.",
        ),
        429 => AppError::new(
            ErrorCode::LlmRateLimit,
            "Anthropic rate limit hit (429). Wait a moment, or check your credit balance.",
        ),
        529 => AppError::new(
            ErrorCode::LlmHttp,
            "Anthropic is overloaded (529). Try again in a moment.",
        ),
        other => AppError::new(
            ErrorCode::LlmHttp,
            format!("Anthropic returned HTTP {other}: {}", snippet(body)),
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
            Profile { resume: "Ten years of Rust.", job_description: "Staff engineer.", ..Default::default() },
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
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}\n\n"
        )
    }

    const MESSAGE_STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

    fn message_delta(stop_reason: &str) -> String {
        format!(
            "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{stop_reason}\"}},\"usage\":{{\"output_tokens\":2}}}}\n\n"
        )
    }

    /// Resolves with the answer TEXT — the stop reason has its own tests.
    async fn run(
        provider: &AnthropicProvider,
        sink: Arc<RecordingSink>,
    ) -> AppResult<String> {
        run_full(provider, sink).await.map(|a| a.text)
    }

    async fn run_full(
        provider: &AnthropicProvider,
        sink: Arc<RecordingSink>,
    ) -> AppResult<Answer> {
        provider
            .stream_answer(&request(), sink, CancellationToken::new())
            .await
    }

    async fn run_body(body: &str) -> (AppResult<Answer>, Vec<String>) {
        let (base, _rx) = serve_once(sse_response(body)).await;
        let provider = AnthropicProvider::new("test-key").with_base_url(base);
        let sink = Arc::new(RecordingSink::default());
        let result = run_full(&provider, sink.clone()).await;
        let deltas = sink.deltas.lock().unwrap().clone();
        (result, deltas)
    }

    async fn status_error(status: u16, reason: &str, body: &str) -> AppError {
        let (base, _rx) = serve_once(http_response(status, reason, body)).await;
        let provider = AnthropicProvider::new("test-key").with_base_url(base);
        run(&provider, Arc::new(RecordingSink::default()))
            .await
            .unwrap_err()
    }

    // ---- happy path --------------------------------------------------------

    #[tokio::test]
    async fn happy_path_streams_every_delta_and_returns_their_exact_concatenation() {
        let body = format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_1\",\"usage\":{{\"input_tokens\":10,\"cache_read_input_tokens\":42}}}}}}\n\n\
             event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
             {}{}\
             event: ping\ndata: {{\"type\":\"ping\"}}\n\n\
             event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
             event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":2}}}}\n\n\
             event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
            delta_event("Hel"),
            delta_event("lo"),
        );
        let (base, _rx) = serve_once(sse_response(&body)).await;
        let provider = AnthropicProvider::new("test-key").with_base_url(base);
        assert_eq!(provider.last_cache_read_input_tokens(), None);

        let sink = Arc::new(RecordingSink::default());
        let answer = run(&provider, sink.clone()).await.unwrap();

        let deltas = sink.deltas.lock().unwrap().clone();
        assert_eq!(deltas, vec!["Hel".to_string(), "lo".to_string()]);
        // The invariant that stops text changing after the user has read it:
        // the returned answer is byte-identical to what streamed into the panel.
        assert_eq!(answer.as_bytes(), deltas.concat().as_bytes());
        assert_eq!(answer, "Hello");
        // The response reported a cache read, and the getter exposes it.
        assert_eq!(provider.last_cache_read_input_tokens(), Some(42));
    }

    #[tokio::test]
    async fn text_deltas_across_multiple_content_blocks_join_with_nothing_between() {
        // Two content blocks; a joiner of any kind would produce "AB?CD".
        let body = format!(
            "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
             {}\
             event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
             event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":1,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
             {}\
             event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":1}}\n\n\
             event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
            delta_event("AB"),
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"CD\"}}\n\n",
        );
        let (base, _rx) = serve_once(sse_response(&body)).await;
        let provider = AnthropicProvider::new("test-key").with_base_url(base);
        let sink = Arc::new(RecordingSink::default());
        let answer = run(&provider, sink.clone()).await.unwrap();
        assert_eq!(answer, "ABCD");
        assert_eq!(answer.as_bytes(), sink.deltas.lock().unwrap().concat().as_bytes());
    }

    // ---- request shape -----------------------------------------------------

    #[tokio::test]
    async fn request_pins_model_streaming_max_tokens_and_the_two_system_blocks() {
        let (base, rx) = serve_once(sse_response(&format!("{}{MESSAGE_STOP}", delta_event("ok")))).await;
        let provider = AnthropicProvider::new("test-key").with_base_url(base);
        let req = request();
        provider
            .stream_answer(&req, Arc::new(RecordingSink::default()), CancellationToken::new())
            .await
            .unwrap();

        let raw = rx.await.unwrap();
        assert!(raw.contains("test-key"), "x-api-key header missing");
        assert!(raw.contains("2023-06-01"), "anthropic-version header missing");

        let json_start = raw.find("\r\n\r\n").unwrap() + 4;
        let v: Value = serde_json::from_str(&raw[json_start..]).unwrap();

        assert_eq!(v["model"], ANTHROPIC_MODEL);
        assert_eq!(v["stream"], true);
        assert_eq!(v["max_tokens"], MAX_ANSWER_TOKENS);

        // Two system blocks with cache_control on the FIRST only: the cache
        // breakpoint sits after the profile, the style policy after it.
        let system = v["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);
        assert_eq!(system[0]["text"], req.system.cached_prefix.as_str());
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(system[1]["text"], req.system.style_suffix.as_str());
        assert!(
            system[1].get("cache_control").is_none(),
            "a breakpoint on the style block would invalidate the profile cache on every style flip"
        );

        let messages = v["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], req.user_message().as_str());
    }

    // ---- error mapping: one test per status --------------------------------

    #[tokio::test]
    async fn status_401_maps_to_llm_auth_and_names_the_status() {
        let err = status_error(401, "Unauthorized", "{\"error\":{\"message\":\"bad key\"}}").await;
        assert_eq!(err.code, ErrorCode::LlmAuth);
        assert!(err.message.contains("401"), "message was: {}", err.message);
        assert!(err.message.contains("Settings"), "message was: {}", err.message);
    }

    #[tokio::test]
    async fn status_403_maps_to_llm_auth_and_names_the_status() {
        let err = status_error(403, "Forbidden", "{}").await;
        assert_eq!(err.code, ErrorCode::LlmAuth);
        assert!(err.message.contains("403"), "message was: {}", err.message);
    }

    #[tokio::test]
    async fn status_429_maps_to_llm_rate_limit_and_names_the_status() {
        let err = status_error(429, "Too Many Requests", "{}").await;
        assert_eq!(err.code, ErrorCode::LlmRateLimit);
        assert!(err.message.contains("429"), "message was: {}", err.message);
    }

    #[tokio::test]
    async fn status_529_maps_to_llm_http_overloaded_and_names_the_status() {
        let err = status_error(529, "Overloaded", "{}").await;
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(err.message.contains("529"), "message was: {}", err.message);
        assert!(err.message.contains("overloaded"), "message was: {}", err.message);
    }

    #[tokio::test]
    async fn other_status_maps_to_llm_http_with_status_and_body_snippet() {
        let err = status_error(500, "Internal Server Error", "{\"error\":\"kaboom detail\"}").await;
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(err.message.contains("500"), "message was: {}", err.message);
        assert!(err.message.contains("kaboom detail"), "message was: {}", err.message);
    }

    #[tokio::test]
    async fn long_error_bodies_are_truncated_in_the_message() {
        let big = "x".repeat(2000);
        let err = status_error(500, "Internal Server Error", &big).await;
        assert_eq!(err.code, ErrorCode::LlmHttp);
        // 300 chars of body plus the fixed prefix — never the whole page.
        assert!(err.message.len() < 400, "message length was {}", err.message.len());
    }

    #[tokio::test]
    async fn error_event_mid_stream_surfaces_as_llm_http_quoting_the_detail() {
        let body = format!(
            "{}event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}}}\n\n",
            delta_event("partial")
        );
        let (base, _rx) = serve_once(sse_response(&body)).await;
        let provider = AnthropicProvider::new("test-key").with_base_url(base);
        let err = run(&provider, Arc::new(RecordingSink::default()))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(err.message.contains("Overloaded"), "message was: {}", err.message);
    }

    #[tokio::test]
    async fn connection_failure_maps_to_llm_http_with_actionable_message() {
        // Bind a port then release it so nothing is listening there.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let provider = AnthropicProvider::new("test-key").with_base_url(format!("http://{addr}"));
        let err = run(&provider, Arc::new(RecordingSink::default()))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(
            err.message.contains("Could not reach Anthropic"),
            "message was: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn empty_200_body_is_llm_http_not_a_silent_empty_answer() {
        let (base, _rx) = serve_once(sse_response("")).await;
        let provider = AnthropicProvider::new("test-key").with_base_url(base);
        let err = run(&provider, Arc::new(RecordingSink::default()))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::LlmHttp);
    }

    #[tokio::test]
    async fn mid_stream_connection_drop_maps_to_llm_http() {
        let (base, _rx) = serve_once(dropped_sse_response(&delta_event("Hel"))).await;
        let provider = AnthropicProvider::new("test-key").with_base_url(base);
        let sink = Arc::new(RecordingSink::default());
        let err = run(&provider, sink.clone()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(
            err.message.contains("dropped while the answer was streaming"),
            "message was: {}",
            err.message
        );
        // The delta that made it through was still delivered live.
        assert_eq!(sink.deltas.lock().unwrap().clone(), vec!["Hel".to_string()]);
    }

    // ---- cancellation ------------------------------------------------------

    #[tokio::test]
    async fn cancellation_mid_stream_returns_aborted_never_an_http_error() {
        // Server sends one delta then stalls with the socket held open; the
        // sink cancels on that first delta. If cancellation required the
        // server to send more bytes, this test would hang.
        let head = dropped_sse_response(&delta_event("Hel"));
        let base = serve_then_stall(head).await;
        let provider = AnthropicProvider::new("test-key").with_base_url(base);

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
        let provider = AnthropicProvider::new("").with_base_url("http://127.0.0.1:1");
        let err = run(&provider, Arc::new(RecordingSink::default()))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NoLlmKey);
        assert!(err.message.contains("Anthropic"), "message was: {}", err.message);
    }

    // ---- R2: protocol completion vs stop reason ------------------------------

    #[tokio::test]
    async fn max_tokens_stop_completes_with_a_token_limit_reason() {
        // A capped answer is kept and labelled "cut short" — it must not be
        // reported as a failure, and must not be mistaken for a normal end.
        let body = format!("{}{}{MESSAGE_STOP}", delta_event("Long answer"), message_delta("max_tokens"));
        let (result, deltas) = run_body(&body).await;
        let answer = result.expect("a capped answer is still an answer");
        assert_eq!(answer.text, "Long answer");
        assert_eq!(answer.stop_reason, StopReason::TokenLimit);
        assert_eq!(deltas.concat(), answer.text);
        let (normal, _) = run_body(&format!("{}{}{MESSAGE_STOP}", delta_event("x"), message_delta("end_turn"))).await;
        assert_eq!(normal.unwrap().stop_reason, StopReason::Complete);
    }

    #[tokio::test]
    async fn a_stop_reason_without_message_stop_is_still_incomplete() {
        // The refinement over the first plan: `stop_reason` is metadata, not
        // proof that the final protocol event arrived.
        let body = format!("{}{}", delta_event("Almost"), message_delta("end_turn"));
        let (result, deltas) = run_body(&body).await;
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(err.message.contains("before the answer was finished"), "{}", err.message);
        // The streamed text was delivered and stays on screen.
        assert_eq!(deltas, vec!["Almost".to_string()]);
    }

    #[tokio::test]
    async fn clean_eof_before_message_stop_fails_but_keeps_the_streamed_text() {
        let (result, deltas) = run_body(&format!("{}{}", delta_event("Hel"), delta_event("lo"))).await;
        assert!(result.unwrap_err().message.contains("incomplete"));
        assert_eq!(deltas.concat(), "Hello");
    }

    #[tokio::test]
    async fn metadata_only_stream_is_an_explicit_failure_not_a_blank_answer() {
        // Ping-only and message_start-only responses used to resolve Ok("").
        let body = format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{}}}}}}\n\n\
             event: ping\ndata: {{\"type\":\"ping\"}}\n\n{}{MESSAGE_STOP}",
            message_delta("end_turn")
        );
        let (result, deltas) = run_body(&body).await;
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(err.message.contains("without any answer text"), "{}", err.message);
        assert!(deltas.is_empty());
    }

    #[tokio::test]
    async fn nothing_after_message_stop_is_consumed() {
        let body = format!("{}{MESSAGE_STOP}{}", delta_event("Done."), delta_event(" EXTRA"));
        let (result, deltas) = run_body(&body).await;
        assert_eq!(result.unwrap().text, "Done.");
        assert_eq!(deltas, vec!["Done.".to_string()], "no delta after the terminator may paint");
    }

    #[tokio::test]
    async fn malformed_known_frames_fail_instead_of_silently_dropping_text() {
        // Unparseable JSON, and a text_delta with no text: each could have
        // been answer text, so neither may be skipped on the way to "done".
        for bad in [
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_de\n\n".to_string(),
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\"}}\n\n".to_string(),
        ] {
            let body = format!("{}{bad}{}{MESSAGE_STOP}", delta_event("A"), delta_event("B"));
            let (result, deltas) = run_body(&body).await;
            let err = result.unwrap_err();
            assert!(err.message.contains("malformed"), "{}", err.message);
            assert_eq!(deltas, vec!["A".to_string()], "text before the bad frame is kept");
        }
    }

    #[tokio::test]
    async fn unknown_event_types_and_non_text_deltas_are_harmless() {
        let body = format!(
            "event: brand_new\ndata: {{\"type\":\"brand_new_event\",\"x\":1}}\n\n\
             event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}}}\n\n\
             {}{MESSAGE_STOP}",
            delta_event("Fine")
        );
        let (result, deltas) = run_body(&body).await;
        assert_eq!(result.unwrap().text, "Fine");
        assert_eq!(deltas, vec!["Fine".to_string()]);
    }

    #[tokio::test]
    async fn an_oversized_frame_is_refused_while_reading() {
        // 1.1 MiB of one line, then the server stalls with the socket open:
        // only a cap enforced while reading can return here instead of
        // waiting (and buffering) forever.
        let mut head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 10000000\r\n\r\n{}data: ",
            delta_event("Start")
        )
        .into_bytes();
        head.extend_from_slice(&vec![b'x'; 1_100_000]);
        let base = serve_then_stall(head).await;
        let provider = AnthropicProvider::new("test-key").with_base_url(base);
        let sink = Arc::new(RecordingSink::default());
        let err = tokio::time::timeout(Duration::from_secs(20), run(&provider, sink.clone()))
            .await
            .expect("the cap must end the read")
            .unwrap_err();
        assert!(err.message.contains("oversized"), "{}", err.message);
        assert_eq!(sink.deltas.lock().unwrap().clone(), vec!["Start".to_string()]);
    }

    #[tokio::test]
    async fn an_endless_error_body_is_read_only_up_to_the_cap() {
        // A 500 promising 10 MB that sends 64 KiB and stalls: the old
        // `response.text()` waited for the rest forever.
        let mut head = b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 10000000\r\n\r\n".to_vec();
        head.extend_from_slice(&vec![b'e'; 64 * 1024]);
        let base = serve_then_stall(head).await;
        let provider = AnthropicProvider::new("test-key").with_base_url(base);
        let err = tokio::time::timeout(
            Duration::from_secs(20),
            run(&provider, Arc::new(RecordingSink::default())),
        )
        .await
        .expect("a capped error body read must return")
        .unwrap_err();
        assert!(err.message.contains("HTTP 500"), "{}", err.message);
        assert!(err.message.len() < 400, "message length was {}", err.message.len());
    }
}

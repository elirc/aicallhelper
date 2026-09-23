//! Free, loopback-only Ollama answers. Never uses keys or cloud fallbacks.
//!
//! The R2 completion contract applies here exactly as it does to the cloud
//! pair: the `done: true` frame is the protocol terminator (nothing after it
//! is read), `done_reason` is metadata (`length` = cut short), an error frame
//! or an end of stream before `done` keeps the streamed text and fails, and a
//! `done` with no usable text is an explicit failure.
use super::prompt::request_input_bytes;
use super::{Answer, AnswerRequest, LlmProvider, LlmProviderKind, LlmSink, StopReason};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::session::AnswerLimits;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const MODEL: &str = "qwen3.5:2b";
pub const ORIGIN: &str = "http://127.0.0.1:11434";
pub const MAX_INPUT_BYTES: usize = 7000;
const MAX_FRAME_BYTES: usize = 256 * 1024;

/// Shown when the cached prefix plus the question would not fit the local
/// context (PROF-08). Names every profile field that counts toward the cap:
/// after call profiles, focus and extra instructions ride in the prefix too,
/// and a message that only mentioned the resume sent users trimming the
/// wrong field.
const OVERSIZE_MESSAGE: &str = "Free local mode supports about 7 KB of combined instructions, profile (resume, job description, focus, extra instructions) and question. Shorten the active profile in Settings or use a cloud model.";

pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .build()
            .expect("local HTTP client")
    })
}

pub struct LocalProvider;
impl Default for LocalProvider {
    fn default() -> Self {
        Self
    }
}

fn http_failure(status: u16) -> AppError {
    match status {
        404 => local_error("Qwen3.5 2B is not installed. Run free voice setup to download it."),
        500 => local_error("Ollama could not load or run the local model. Close unused apps to free several GB of RAM, then start and warm free mode again. Check ollama.log if this continues."),
        _ => local_error(format!("Ollama returned HTTP {status}. Check the local service and retry.")),
    }
}

fn local_error(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::LlmHttp, message)
}

/// The refusal for a request over `MAX_INPUT_BYTES`. Public so the shell's
/// pre-checks (before recording, before a typed ask) refuse with the same
/// code and the same words as this gate.
pub fn oversize_error() -> AppError {
    local_error(OVERSIZE_MESSAGE)
}

pub fn request_body(req: &AnswerRequest) -> AppResult<Value> {
    // Conservatively budget by UTF-8 bytes so even non-English input fits.
    // Do not silently truncate a resume or the question. The count is the
    // same function the Settings preview uses (R4), so the preview and this
    // gate cannot disagree; this gate stays the authority.
    if request_input_bytes(&req.system, &req.transcript) > MAX_INPUT_BYTES {
        return Err(oversize_error());
    }
    let system = req.system.joined();
    let user = req.user_message();
    Ok(json!({
        "model": MODEL, "stream": true, "think": false, "keep_alive": "10m",
        "options": { "num_ctx": 8192, "num_predict": 512, "num_thread": 4, "temperature": 0.3 },
        "messages": [{"role":"system","content":system},{"role":"user","content":user}]
    }))
}

/// A bounded byte decoder: HTTP chunks may split JSON and UTF-8 anywhere.
#[derive(Default)]
struct Ndjson {
    pending: Vec<u8>,
}
impl Ndjson {
    fn push(&mut self, bytes: &[u8]) -> AppResult<Vec<Value>> {
        let mut frames = Vec::new();
        for byte in bytes {
            if *byte == b'\n' {
                if !self.pending.iter().all(u8::is_ascii_whitespace) {
                    frames.push(serde_json::from_slice(&self.pending).map_err(|_| {
                        local_error("Ollama returned an invalid streaming response.")
                    })?);
                }
                self.pending.clear();
            } else {
                if self.pending.len() >= MAX_FRAME_BYTES {
                    return Err(local_error(
                        "Ollama returned an oversized streaming response.",
                    ));
                }
                self.pending.push(*byte);
            }
        }
        Ok(frames)
    }
    fn finish(&mut self) -> AppResult<Vec<Value>> {
        self.push(b"\n")
    }
}

/// Interpret one frame. Returns the stop reason once the `done` frame (the
/// protocol terminator) arrives, `None` while the answer is still streaming.
fn apply(frame: &Value, sink: &dyn LlmSink, answer: &mut String) -> AppResult<Option<StopReason>> {
    if frame.get("error").is_some() {
        // Avoid exposing unbounded service internals in the UI.
        return Err(local_error("Ollama could not generate an answer. Check that qwen3.5:2b is installed, then start and warm free mode in Settings."));
    }
    if let Some(content) = frame.pointer("/message/content").and_then(Value::as_str) {
        if !content.is_empty() {
            answer.push_str(content);
            sink.on_delta(content.to_owned());
        }
    }
    // /message/thinking is intentionally never emitted.
    if frame.get("done").and_then(Value::as_bool) == Some(true) {
        // `done_reason` says why generation stopped; `length` is the
        // `num_predict` cap — kept and labelled "cut short", not failed.
        let reason = frame.get("done_reason").and_then(Value::as_str);
        return Ok(Some(StopReason::from_provider(reason)));
    }
    Ok(None)
}

/// One answer's decode state: frames in, the finished answer out once the
/// `done` frame arrives. Kept apart from the HTTP loop so the completion
/// contract is testable without an Ollama server.
#[derive(Default)]
struct LocalStream {
    decoder: Ndjson,
    answer: String,
}

impl LocalStream {
    /// Feed one HTTP chunk. `Some` once the terminator arrived — the caller
    /// stops reading there, and any frame after `done` in the same chunk is
    /// never applied.
    fn push(&mut self, chunk: &[u8], sink: &dyn LlmSink) -> AppResult<Option<Answer>> {
        for frame in self.decoder.push(chunk)? {
            if let Some(stop) = apply(&frame, sink, &mut self.answer)? {
                return finished(std::mem::take(&mut self.answer), stop).map(Some);
            }
        }
        Ok(None)
    }

    /// End of stream: flush a final unterminated frame, and fail if the
    /// terminator never came.
    fn finish(&mut self, sink: &dyn LlmSink) -> AppResult<Answer> {
        for frame in self.decoder.finish()? {
            if let Some(stop) = apply(&frame, sink, &mut self.answer)? {
                return finished(std::mem::take(&mut self.answer), stop);
            }
        }
        Err(local_error(
            "Ollama stopped before finishing the answer, so it is incomplete. Try again.",
        ))
    }
}

#[async_trait]
impl LlmProvider for LocalProvider {
    async fn stream_answer(
        &self,
        req: &AnswerRequest,
        sink: Arc<dyn LlmSink>,
        cancel: CancellationToken,
    ) -> AppResult<Answer> {
        let body = request_body(req)?;
        let work = async {
            let response = client()
                .post(format!("{ORIGIN}/api/chat"))
                .json(&body)
                .send()
                .await
                .map_err(|_| {
                    local_error(
                        "Ollama is unavailable. Open Settings and start and warm free local mode.",
                    )
                })?;
            if !response.status().is_success() {
                return Err(http_failure(response.status().as_u16()));
            }
            let mut stream = response.bytes_stream();
            let mut decode = LocalStream::default();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| {
                    local_error("The local answer connection ended unexpectedly. Try again.")
                })?;
                if let Some(answer) = decode.push(&chunk, &*sink)? {
                    return Ok(answer);
                }
            }
            decode.finish(&*sink)
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(AppError::aborted()),
            result = work => result,
        }
    }

    // Loading is explicit in Settings; do not queue a second model request
    // behind an in-flight answer or waste CPU on every Record.
    fn prewarm(&self) {}
    fn kind(&self) -> LlmProviderKind {
        LlmProviderKind::Local
    }
    /// CPU inference (§3): a 2B model on a laptop can spend longer than the
    /// whole 10 s cloud first-token cap just ingesting the prompt.
    fn answer_limits(&self) -> AnswerLimits {
        AnswerLimits::LOCAL
    }
}

/// The terminator arrived: usable text is an answer, none is an explicit
/// failure (R2) — never a blank success.
fn finished(answer: String, stop_reason: StopReason) -> AppResult<Answer> {
    if answer.trim().is_empty() {
        Err(local_error(
            "The local model returned an empty answer. Try again.",
        ))
    } else {
        Ok(Answer { text: answer, stop_reason })
    }
}

pub async fn warm() -> AppResult<()> {
    let response = client()
        .post(format!("{ORIGIN}/api/chat"))
        .timeout(Duration::from_secs(120))
        .json(
            &json!({"model":MODEL,"messages":[],"stream":false,"think":false,
            "keep_alive":"10m","options":{"num_ctx":8192,"num_thread":4}}),
        )
        .send()
        .await
        .map_err(|_| {
            local_error(
                "Could not warm Ollama. Check free voice setup and available memory, then retry.",
            )
        })?;
    if !response.status().is_success() {
        return Err(http_failure(response.status().as_u16()));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| local_error("Invalid Ollama warm-up response."))?;
    if body.get("error").is_some() || body.get("done").and_then(Value::as_bool) != Some(true) {
        return Err(local_error("Ollama did not finish loading Qwen3.5 2B."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{build_system_prompt, AnswerStyle, Profile};
    use std::sync::Mutex;
    #[derive(Default)]
    struct Sink(Mutex<String>);
    impl LlmSink for Sink {
        fn on_delta(&self, s: String) {
            self.0.lock().unwrap().push_str(&s);
        }
    }
    #[test]
    fn split_utf8_and_multiple_frames_only_emit_answer_content() {
        let wire = "{\"message\":{\"thinking\":\"secret\"},\"done\":false}\n{\"message\":{\"content\":\"café\"}}\n{\"done\":true}";
        let mut d = Ndjson::default();
        let mut frames = vec![];
        for byte in wire.as_bytes() {
            frames.extend(d.push(&[*byte]).unwrap());
        }
        frames.extend(d.finish().unwrap());
        let sink = Sink::default();
        let mut answer = String::new();
        assert_eq!(apply(&frames[0], &sink, &mut answer).unwrap(), None);
        assert_eq!(apply(&frames[1], &sink, &mut answer).unwrap(), None);
        assert_eq!(apply(&frames[2], &sink, &mut answer).unwrap(), Some(StopReason::Complete));
        assert_eq!(answer, "café");
        assert_eq!(answer, *sink.0.lock().unwrap());
    }
    #[test]
    fn malformed_and_oversize_frames_fail() {
        assert!(Ndjson::default().push(b"oops\n").is_err());
        assert!(Ndjson::default()
            .push(&vec![b'x'; MAX_FRAME_BYTES + 1])
            .is_err());
    }
    #[test]
    fn request_is_local_thinking_off_and_profile_is_not_silently_cut() {
        let mut r = AnswerRequest::new(build_system_prompt(
            Profile {
                resume: "My experience",
                job_description: "My role",
                ..Default::default()
            },
            AnswerStyle::Detailed,
        ))
        .with_transcript("Question");
        let v = request_body(&r).unwrap();
        assert_eq!(v["model"], MODEL);
        assert_eq!(v["think"], false);
        assert!(v["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("My experience"));
        r.transcript = "é".repeat(MAX_INPUT_BYTES);
        let err = request_body(&r).expect_err("over the byte cap must refuse, never truncate");
        // Pinned verbatim (PROF-08): the text tells the user which fields
        // count and where to shorten them; a paraphrase that dropped a field
        // would send them trimming the wrong thing.
        assert_eq!(
            err.message,
            "Free local mode supports about 7 KB of combined instructions, profile (resume, job description, focus, extra instructions) and question. Shorten the active profile in Settings or use a cloud model."
        );
        assert_eq!(err.code, ErrorCode::LlmHttp);
    }
    #[test]
    fn local_provider_answers_on_the_local_deadlines() {
        // Ollama on the CPU needs minutes where the cloud gets seconds; on the
        // trait's cloud default every local answer would time out at 10 s.
        assert_eq!(LocalProvider.answer_limits(), AnswerLimits::LOCAL);
        assert_eq!(LocalProvider.kind(), LlmProviderKind::Local);
    }

    #[test]
    fn the_done_frame_ends_the_answer_and_nothing_after_it_is_read() {
        // R2 applied to NDJSON: `done` is the terminator; a frame after it in
        // the same chunk must not paint, and `done_reason: length` is kept as
        // "cut short" rather than failed.
        let wire = "{\"message\":{\"content\":\"Short\"}}\n{\"done\":true,\"done_reason\":\"length\"}\n{\"message\":{\"content\":\" EXTRA\"}}\n";
        let sink = Sink::default();
        let mut s = LocalStream::default();
        let answer = s.push(wire.as_bytes(), &sink).unwrap().expect("done arrived");
        assert_eq!(answer.text, "Short");
        assert_eq!(answer.stop_reason, StopReason::TokenLimit);
        assert_eq!(*sink.0.lock().unwrap(), "Short");
    }

    #[test]
    fn eof_before_done_fails_but_the_streamed_text_was_delivered() {
        let sink = Sink::default();
        let mut s = LocalStream::default();
        assert!(s.push(b"{\"message\":{\"content\":\"Half\"}}\n", &sink).unwrap().is_none());
        let err = s.finish(&sink).unwrap_err();
        assert!(err.message.contains("incomplete"), "{}", err.message);
        assert_eq!(*sink.0.lock().unwrap(), "Half");
    }

    #[test]
    fn a_done_frame_without_text_is_an_explicit_failure() {
        let sink = Sink::default();
        let mut s = LocalStream::default();
        let err = s.push(b"{\"message\":{\"content\":\"\"}}\n{\"done\":true}\n", &sink).unwrap_err();
        assert!(err.message.contains("empty answer"), "{}", err.message);
    }

    #[test]
    fn an_error_frame_after_partial_output_fails_and_keeps_the_text() {
        let sink = Sink::default();
        let mut s = LocalStream::default();
        let err = s
            .push(b"{\"message\":{\"content\":\"Half\"}}\n{\"error\":\"out of memory\"}\n", &sink)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::LlmHttp);
        assert!(!err.message.contains("out of memory"), "service internals stay out of the UI");
        assert_eq!(*sink.0.lock().unwrap(), "Half");
    }
}

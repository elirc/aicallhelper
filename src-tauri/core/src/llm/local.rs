//! Free, loopback-only Ollama answers. Never uses keys or cloud fallbacks.
use super::{AnswerRequest, LlmProvider, LlmProviderKind, LlmSink};
use crate::error::{AppError, AppResult, ErrorCode};
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

pub fn request_body(req: &AnswerRequest) -> AppResult<Value> {
    let system = req.system.joined();
    let user = req.user_message();
    // Conservatively budget by UTF-8 bytes so even non-English input fits.
    // Do not silently truncate a resume or the question.
    if system.len() + user.len() > MAX_INPUT_BYTES {
        return Err(local_error("Free local mode supports about 7 KB of combined instructions, resume, job description and question. Shorten the profile in Settings or use a cloud model."));
    }
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

fn apply(frame: &Value, sink: &dyn LlmSink, answer: &mut String) -> AppResult<bool> {
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
    Ok(frame.get("done").and_then(Value::as_bool) == Some(true))
}

#[async_trait]
impl LlmProvider for LocalProvider {
    async fn stream_answer(
        &self,
        req: &AnswerRequest,
        sink: Arc<dyn LlmSink>,
        cancel: CancellationToken,
    ) -> AppResult<String> {
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
            let mut decoder = Ndjson::default();
            let mut answer = String::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| {
                    local_error("The local answer connection ended unexpectedly. Try again.")
                })?;
                for frame in decoder.push(&chunk)? {
                    if apply(&frame, &*sink, &mut answer)? {
                        return nonempty(answer);
                    }
                }
            }
            for frame in decoder.finish()? {
                if apply(&frame, &*sink, &mut answer)? {
                    return nonempty(answer);
                }
            }
            Err(local_error(
                "Ollama stopped before finishing the answer. Try again.",
            ))
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
}

fn nonempty(answer: String) -> AppResult<String> {
    if answer.trim().is_empty() {
        Err(local_error(
            "The local model returned an empty answer. Try again.",
        ))
    } else {
        Ok(answer)
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
        assert!(!apply(&frames[0], &sink, &mut answer).unwrap());
        assert!(!apply(&frames[1], &sink, &mut answer).unwrap());
        assert!(apply(&frames[2], &sink, &mut answer).unwrap());
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
        assert!(request_body(&r).is_err());
    }
}

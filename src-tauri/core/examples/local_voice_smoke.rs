//! Opt-in real-model test. Run with scripts/test-free-voice.ps1 after setup.
use app_core::{
    audio::{self, capture::WasapiLoopbackCapture, AudioCapture, AudioSink},
    llm::{build_system_prompt, local::LocalProvider, AnswerRequest, AnswerStyle, Profile},
    session::{
        machine::SessionManager, EventSink, SessionDeps, SessionEvent, SessionId, StopOutcome,
    },
    stt::local::LocalConnector,
    AppError,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::mpsc;

struct Events(mpsc::UnboundedSender<SessionEvent>);
impl EventSink for Events {
    fn emit(&self, e: SessionEvent) {
        let _ = self.0.send(e);
    }
}
struct Capture {
    sessions: Arc<SessionManager>,
    id: SessionId,
}
impl AudioSink for Capture {
    fn on_frame(&self, pcm: Vec<i16>, rms: f32) {
        self.sessions.push_audio(self.id, &pcm, rms);
    }
    fn on_error(&self, e: AppError) {
        self.sessions.device_error(self.id, e);
    }
}
fn deps(events: Arc<dyn EventSink>, style: AnswerStyle) -> SessionDeps {
    SessionDeps {
        stt: Arc::new(LocalConnector), llm: Arc::new(LocalProvider),
        events, answer_request: AnswerRequest::new(build_system_prompt(Profile {
            resume:"Customer support specialist. I listen carefully, clarify the problem, explain available options, and follow up. I escalate issues when needed.",
            job_description:"Customer support role requiring empathy and clear communication.",
            ..Default::default()
        },style)),
    }
}
fn pcm16(path: &PathBuf) -> Result<Vec<i16>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err("Expected a WAV file".into());
    }
    let mut at = 12;
    let mut valid = false;
    let mut samples = None;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        let end = at + 8 + size;
        if end > bytes.len() {
            return Err("Truncated WAV file".into());
        }
        let data = &bytes[at + 8..end];
        match &bytes[at..at + 4] {
            b"fmt " => {
                valid = data.len() >= 16
                    && data[0..2] == 1u16.to_le_bytes()
                    && data[2..4] == 1u16.to_le_bytes()
                    && data[4..8] == 16000u32.to_le_bytes()
                    && data[14..16] == 16u16.to_le_bytes();
            }
            b"data" => {
                samples = Some(
                    data.chunks_exact(2)
                        .map(|p| i16::from_le_bytes([p[0], p[1]]))
                        .collect(),
                )
            }
            _ => {}
        }
        at = end + size % 2;
    }
    if !valid {
        return Err("Expected 16 kHz mono PCM16".into());
    }
    samples.ok_or_else(|| "No WAV audio".into())
}
async fn done(
    rx: &mut mpsc::UnboundedReceiver<SessionEvent>,
    id: SessionId,
    voice: bool,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut deltas = String::new();
    let mut partials = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(310);
    loop {
        let event = tokio::time::timeout_at(deadline, rx.recv())
            .await?
            .ok_or("Event channel closed")?;
        if event.session_id() != id {
            continue;
        }
        match event {
            SessionEvent::SttPartial { text, .. } if !text.is_empty() => partials += 1,
            SessionEvent::LlmDelta { delta, .. } => deltas.push_str(&delta),
            SessionEvent::SessionError { error, .. } => return Err(error.into()),
            SessionEvent::LlmDone {
                transcript,
                answer,
                metrics,
                ..
            } => {
                if answer.trim().is_empty() || deltas != answer {
                    return Err("Answer stream mismatch or empty answer".into());
                }
                if voice && (partials == 0 || !transcript.to_lowercase().contains("customer")) {
                    return Err(format!(
                        "Speech did not recognize the practice question: {transcript}"
                    )
                    .into());
                }
                if !voice && metrics.stt_finalize_ms != 0 {
                    return Err("Typed Ask used speech".into());
                }
                return Ok(serde_json::json!({"mode":if voice {"voice"} else {"typed"},
                    "transcript":transcript,"answer":answer,"partialEvents":partials,"metrics":metrics}));
            }
            _ => {}
        }
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let wav = PathBuf::from(args.next().ok_or("Pass a 16 kHz PCM16 WAV file")?).canonicalize()?;
    let loopback = args.any(|a| a == "--loopback");
    let samples = pcm16(&wav)?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let events: Arc<dyn EventSink> = Arc::new(Events(tx));
    let sessions = Arc::new(SessionManager::new());
    let id = sessions
        .start(deps(events.clone(), AnswerStyle::Brief))
        .await?;
    // Let the session install its stream before supplying the fixture's first frame.
    tokio::time::sleep(Duration::from_millis(200)).await;
    if loopback {
        let capture = WasapiLoopbackCapture.start(Arc::new(Capture {
            sessions: sessions.clone(),
            id,
        }))?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let play=tokio::task::spawn_blocking(move || {
            let mut command=std::process::Command::new("powershell.exe");
            // A fixed script reads the fixture path from an environment
            // variable; no shell interpolation of user-provided paths.
            command.args(["-NoProfile","-Command","$p=New-Object System.Media.SoundPlayer; $p.SoundLocation=$env:CALLHELPER_TEST_WAV; $p.PlaySync()"])
                .env("CALLHELPER_TEST_WAV",&wav);
            #[cfg(windows)] {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            command.status()
        }).await??;
        if !play.success() {
            return Err("Could not play the voice fixture".into());
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(sessions.stop(id).await, StopOutcome::Taken);
        capture.stop();
    } else {
        for chunk in samples.chunks(audio::FRAME_SAMPLES) {
            sessions.push_audio(id, chunk, audio::rms(chunk));
            tokio::time::sleep(Duration::from_secs_f64(chunk.len() as f64 / 16000.0)).await;
        }
        assert_eq!(sessions.stop(id).await, StopOutcome::Taken);
    }
    let voice = done(&mut rx, id, true).await?;
    let typed = sessions
        .ask(
            "How do you stay organized when several customers need help?",
            deps(events.clone(), AnswerStyle::Detailed),
        )
        .await?;
    let typed = done(&mut rx, typed, false).await?;
    let cancel_id = sessions
        .ask(
            "Explain customer support in detail.",
            deps(events.clone(), AnswerStyle::Detailed),
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    sessions.cancel(cancel_id);
    while rx.try_recv().is_ok() {}
    let retry = sessions
        .ask(
            "How do you ask a customer for clarification?",
            deps(events, AnswerStyle::Brief),
        )
        .await?;
    let retry = done(&mut rx, retry, false).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "capture":if loopback {"WASAPI system audio"} else {"PCM fixture"},
            "voice":voice,"typedDetailed":typed,"typedAfterCancel":retry,"cancelledSession":cancel_id
        }))?
    );
    Ok(())
}

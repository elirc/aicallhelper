//! Parsing of Deepgram's JSON text frames, and incremental transcript
//! accumulation (§6.1).
//!
//! This is pure and total: every input, including deliberately hostile JSON,
//! maps to a value rather than a panic. A crash here would take down a live
//! recording mid-call.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepgramFrame {
    /// A transcript segment. `is_final` means Deepgram has committed it and
    /// will not revise it.
    Results { transcript: String, is_final: bool },
    /// Deepgram reported an error. `detail` is whatever it told us, quoted into
    /// the user-facing message so the failure is diagnosable.
    Error { detail: String },
    /// Metadata, UtteranceEnd, SpeechStarted, unknown types, and any frame too
    /// malformed to interpret. Deliberately not an error: an unrecognised frame
    /// is not a reason to abandon a recording.
    Ignored,
}

/// Parse one text frame.
pub fn parse_frame(raw: &str) -> DeepgramFrame {
    // serde_json enforces a recursion limit internally, so pathologically
    // nested input returns Err rather than blowing the stack.
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return DeepgramFrame::Ignored;
    };
    parse_value(&value)
}

fn parse_value(value: &Value) -> DeepgramFrame {
    let Some(obj) = value.as_object() else {
        return DeepgramFrame::Ignored;
    };

    match obj.get("type").and_then(Value::as_str) {
        Some("Results") => parse_results(value),
        Some("Error") => DeepgramFrame::Error { detail: error_detail(value) },
        // Metadata / UtteranceEnd / SpeechStarted / anything new Deepgram adds.
        _ => DeepgramFrame::Ignored,
    }
}

fn parse_results(value: &Value) -> DeepgramFrame {
    // `is_final` must be *literally* the boolean true. Deepgram has shipped
    // frames carrying "true" and 1 in adjacent fields; treating a truthy
    // imposter as final permanently commits text that is still being revised,
    // so the transcript ends up with the same phrase twice.
    let is_final = matches!(value.get("is_final"), Some(Value::Bool(true)));

    // Missing or null channel, empty alternatives, or a non-string transcript:
    // nothing usable, so ignore the frame entirely rather than committing "".
    let transcript = value
        .get("channel")
        .and_then(Value::as_object)
        .and_then(|c| c.get("alternatives"))
        .and_then(Value::as_array)
        .and_then(|alts| alts.first())
        .and_then(Value::as_object)
        .and_then(|alt| alt.get("transcript"))
        .and_then(Value::as_str);

    match transcript {
        Some(t) => DeepgramFrame::Results { transcript: t.to_string(), is_final },
        None => DeepgramFrame::Ignored,
    }
}

/// Deepgram ships two error shapes: the v1 listen shape
/// `{description, message, variant}` and a newer `{code, description}`. Quote
/// whatever detail is present — a bare "STT error" sends the user hunting.
fn error_detail(value: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for key in ["code", "variant", "description", "message"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            let s = s.trim();
            if !s.is_empty() && !parts.iter().any(|p| p == s) {
                parts.push(s.to_string());
            }
        }
    }
    if parts.is_empty() {
        "Deepgram reported an error with no detail.".to_string()
    } else {
        parts.join(": ")
    }
}

/// Builds the running transcript from a stream of frames.
///
/// The full transcript is `committed + latest interim`. Committed text is
/// appended in place as each final arrives (O(1) amortised per frame) rather
/// than re-joining a growing list of segments on every message — at ~10
/// messages a second over a two-minute recording, the re-join version is doing
/// quadratic work inside the latency budget.
#[derive(Debug, Default, Clone)]
pub struct TranscriptAccumulator {
    committed: String,
    interim: String,
}

impl TranscriptAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a parsed frame. Returns true if the visible transcript changed.
    pub fn apply(&mut self, frame: &DeepgramFrame) -> bool {
        let DeepgramFrame::Results { transcript, is_final } = frame else {
            return false;
        };

        if *is_final {
            let seg = transcript.trim();
            // An empty final is a normal silence marker, not text. It still
            // clears the interim, because whatever was tentative is now gone.
            if !seg.is_empty() {
                if !self.committed.is_empty() {
                    self.committed.push(' ');
                }
                self.committed.push_str(seg);
            }
            let changed = !seg.is_empty() || !self.interim.is_empty();
            self.interim.clear();
            changed
        } else {
            let next = transcript.trim();
            if self.interim == next {
                return false;
            }
            self.interim.clear();
            self.interim.push_str(next);
            true
        }
    }

    /// The transcript as the user should see it right now.
    pub fn text(&self) -> String {
        if self.interim.is_empty() {
            return self.committed.clone();
        }
        if self.committed.is_empty() {
            return self.interim.clone();
        }
        format!("{} {}", self.committed, self.interim)
    }

    /// The committed-only transcript, used once the stream has been finalized.
    pub fn committed(&self) -> &str {
        &self.committed
    }

    /// Final answer input: prefer committed text, but if Deepgram closed while
    /// only an interim existed, that interim is still the user's question and
    /// is far better than answering nothing.
    pub fn finalized_text(&self) -> String {
        let t = self.text();
        t.trim().to_string()
    }

    pub fn is_empty(&self) -> bool {
        self.committed.trim().is_empty() && self.interim.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn results(transcript: &str, is_final: bool) -> DeepgramFrame {
        DeepgramFrame::Results { transcript: transcript.to_string(), is_final }
    }

    #[test]
    fn parses_a_normal_interim_result() {
        let raw = r#"{"type":"Results","is_final":false,"channel":{"alternatives":[{"transcript":"tell me about"}]}}"#;
        assert_eq!(parse_frame(raw), results("tell me about", false));
    }

    #[test]
    fn parses_a_normal_final_result() {
        let raw = r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":"Tell me about yourself."}]}}"#;
        assert_eq!(parse_frame(raw), results("Tell me about yourself.", true));
    }

    #[test]
    fn is_final_must_be_literally_true() {
        // Truthy imposters are interim. Committing on a string "true" would
        // append text that Deepgram then revises, duplicating the phrase.
        for imposter in ["\"true\"", "1", "\"1\"", "[]", "{}", "null"] {
            let raw = format!(
                r#"{{"type":"Results","is_final":{imposter},"channel":{{"alternatives":[{{"transcript":"x"}}]}}}}"#
            );
            assert_eq!(parse_frame(&raw), results("x", false), "imposter {imposter} leaked through");
        }
    }

    #[test]
    fn missing_is_final_is_interim() {
        let raw = r#"{"type":"Results","channel":{"alternatives":[{"transcript":"x"}]}}"#;
        assert_eq!(parse_frame(raw), results("x", false));
    }

    #[test]
    fn frames_without_a_usable_transcript_are_ignored() {
        // Each of these would otherwise commit an empty segment or panic on an
        // unwrap.
        let cases = [
            r#"{"type":"Results","is_final":true}"#,
            r#"{"type":"Results","is_final":true,"channel":null}"#,
            r#"{"type":"Results","is_final":true,"channel":{}}"#,
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":[]}}"#,
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":null}}"#,
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{}]}}"#,
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":null}]}}"#,
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":42}]}}"#,
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":"nope"}}"#,
            r#"{"type":"Results","is_final":true,"channel":[1,2,3]}"#,
        ];
        for raw in cases {
            assert_eq!(parse_frame(raw), DeepgramFrame::Ignored, "should ignore: {raw}");
        }
    }

    #[test]
    fn an_empty_string_transcript_is_a_valid_frame() {
        // Distinct from "no transcript field": Deepgram sends empty finals as
        // silence markers and the accumulator relies on seeing them.
        let raw = r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":""}]}}"#;
        assert_eq!(parse_frame(raw), results("", true));
    }

    #[test]
    fn metadata_and_unknown_types_are_ignored() {
        for raw in [
            r#"{"type":"Metadata","duration":3.2}"#,
            r#"{"type":"UtteranceEnd","last_word_end":2.1}"#,
            r#"{"type":"SpeechStarted"}"#,
            r#"{"type":"SomethingDeepgramAddsIn2027"}"#,
            r#"{"no_type":true}"#,
            r#"{"type":123}"#,
        ] {
            assert_eq!(parse_frame(raw), DeepgramFrame::Ignored, "should ignore: {raw}");
        }
    }

    #[test]
    fn malformed_json_is_ignored_never_panics() {
        for raw in ["", "   ", "not json", "{", "[", "{\"a\":", "\u{0}", "null", "true", "[1,2,3]", "\"a string\""] {
            assert_eq!(parse_frame(raw), DeepgramFrame::Ignored, "should ignore: {raw:?}");
        }
    }

    #[test]
    fn pathologically_nested_json_is_ignored_without_stack_overflow() {
        // serde_json's recursion limit turns this into an Err. Without it this
        // is a stack overflow, which aborts the process mid-call.
        let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
        assert_eq!(parse_frame(&deep), DeepgramFrame::Ignored);
    }

    #[test]
    fn parses_the_v1_listen_error_shape() {
        let raw = r#"{"type":"Error","description":"Deepgram did not receive audio","message":"NET-0001","variant":"NET-0001"}"#;
        let DeepgramFrame::Error { detail } = parse_frame(raw) else {
            panic!("expected an error frame");
        };
        assert!(detail.contains("NET-0001"), "detail was {detail:?}");
        assert!(detail.contains("Deepgram did not receive audio"), "detail was {detail:?}");
    }

    #[test]
    fn parses_the_newer_code_description_error_shape() {
        let raw = r#"{"type":"Error","code":"DATA-0000","description":"Bad audio format"}"#;
        let DeepgramFrame::Error { detail } = parse_frame(raw) else {
            panic!("expected an error frame");
        };
        assert_eq!(detail, "DATA-0000: Bad audio format");
    }

    #[test]
    fn error_frame_with_no_detail_still_says_something() {
        let DeepgramFrame::Error { detail } = parse_frame(r#"{"type":"Error"}"#) else {
            panic!("expected an error frame");
        };
        assert!(!detail.is_empty());
    }

    #[test]
    fn accumulator_appends_finals_and_replaces_interims() {
        let mut acc = TranscriptAccumulator::new();

        assert!(acc.apply(&results("tell", false)));
        assert_eq!(acc.text(), "tell");

        // A later interim replaces the earlier one rather than appending.
        assert!(acc.apply(&results("tell me about", false)));
        assert_eq!(acc.text(), "tell me about");

        assert!(acc.apply(&results("Tell me about yourself.", true)));
        assert_eq!(acc.text(), "Tell me about yourself.");
        assert_eq!(acc.committed(), "Tell me about yourself.");

        // Next utterance: interim rides on top of the committed prefix.
        assert!(acc.apply(&results("and why", false)));
        assert_eq!(acc.text(), "Tell me about yourself. and why");

        assert!(acc.apply(&results("And why this role?", true)));
        assert_eq!(acc.text(), "Tell me about yourself. And why this role?");
    }

    #[test]
    fn empty_finals_do_not_insert_stray_spaces() {
        let mut acc = TranscriptAccumulator::new();
        acc.apply(&results("", true));
        acc.apply(&results("", true));
        assert_eq!(acc.text(), "");
        acc.apply(&results("Hello.", true));
        acc.apply(&results("", true));
        assert_eq!(acc.text(), "Hello.");
        assert_eq!(acc.finalized_text(), "Hello.");
    }

    #[test]
    fn an_empty_final_clears_a_pending_interim() {
        // The interim was speculative; once Deepgram commits nothing, showing
        // the stale interim is showing text that was retracted.
        let mut acc = TranscriptAccumulator::new();
        acc.apply(&results("umm", false));
        assert_eq!(acc.text(), "umm");
        assert!(acc.apply(&results("", true)));
        assert_eq!(acc.text(), "");
    }

    #[test]
    fn repeating_an_identical_interim_reports_no_change() {
        // Deepgram re-sends unchanged interims; re-emitting an identical
        // stt:partial makes the UI repaint for nothing.
        let mut acc = TranscriptAccumulator::new();
        assert!(acc.apply(&results("same", false)));
        assert!(!acc.apply(&results("same", false)));
        assert!(!acc.apply(&results("  same  ", false)));
    }

    #[test]
    fn non_results_frames_do_not_change_the_transcript() {
        let mut acc = TranscriptAccumulator::new();
        acc.apply(&results("hi", true));
        assert!(!acc.apply(&DeepgramFrame::Ignored));
        assert!(!acc.apply(&DeepgramFrame::Error { detail: "x".into() }));
        assert_eq!(acc.text(), "hi");
    }

    #[test]
    fn is_empty_and_finalized_text_treat_whitespace_as_nothing() {
        // This feeds the no_speech decision (§5.7); whitespace must not count
        // as a question or we send the model an empty prompt.
        let mut acc = TranscriptAccumulator::new();
        assert!(acc.is_empty());
        acc.apply(&results("   ", false));
        assert!(acc.is_empty());
        assert_eq!(acc.finalized_text(), "");
        acc.apply(&results("  real words  ", true));
        assert!(!acc.is_empty());
        assert_eq!(acc.finalized_text(), "real words");
    }

    #[test]
    fn an_interim_that_never_finalizes_is_still_usable_as_the_question() {
        // If Deepgram closes while text is only tentative, answering the
        // tentative question beats answering nothing.
        let mut acc = TranscriptAccumulator::new();
        acc.apply(&results("what is your greatest weakness", false));
        assert_eq!(acc.finalized_text(), "what is your greatest weakness");
        assert!(!acc.is_empty());
    }
}

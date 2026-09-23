//! A server-sent-events decoder that is correct under hostile chunking (§6.3).
//!
//! Both providers stream SSE, so this is shared. The requirements that shape the
//! design, each of which corresponds to a way a naive implementation loses the
//! end of an answer:
//!
//! * Chunks arrive split at **any** byte boundary — mid-line, mid-JSON, and
//!   crucially *between `\r` and `\n`*. A splitter that treats a trailing `\r`
//!   as a complete line ending and then sees `\n` at the head of the next chunk
//!   emits a spurious blank line, which in SSE means "dispatch the event" — so
//!   an event gets cut in half. `pending_cr` exists for exactly this.
//! * Multi-byte UTF-8 characters get split across chunks. Solved structurally:
//!   we buffer **raw bytes** per line and only decode once a terminator is
//!   found. `\r` and `\n` can never occur inside a multi-byte UTF-8 sequence,
//!   so a line boundary is always a safe place to decode.
//! * A stream can end without a trailing newline. The last `data:` line still
//!   carries real answer text, so `finish()` flushes it. Dropping it silently
//!   truncates the answer's last words, which reads to the user as the model
//!   trailing off.
//! * `data: [DONE]` is framed like any other event. The decoder is framing
//!   only and never stops on it; deciding that `[DONE]` ends the answer is the
//!   Groq provider's protocol rule (R2), which stops reading there.
//! * Memory is bounded while reading (R2): a line longer than
//!   `MAX_LINE_BYTES`, or an event whose joined data exceeds
//!   `MAX_EVENT_BYTES`, fails the feed at the byte that crosses the cap. A
//!   server that never sends a newline must not be able to grow a buffer
//!   until the process runs out of memory.

/// One dispatched SSE event. Only the data payload matters to this app; `event:`
/// / `id:` / `retry:` fields are parsed-and-ignored so they cannot be mistaken
/// for data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub data: String,
}

/// Longest single SSE line accepted, in bytes. Real frames are a few hundred
/// bytes; 1 MiB leaves three orders of magnitude of headroom while still
/// bounding what a broken or hostile stream can make us allocate.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Longest joined `data:` payload for one event, in bytes (same reasoning).
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;

/// A line or event outgrew its cap. The decoder's state is unusable after
/// this; callers abandon the stream — but only after applying `decoded`, the
/// events that were fully framed earlier in the same feed. Dropping those
/// would make the visible partial text depend on how the network happened to
/// chunk the bytes (R1 review F3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseOverflow {
    pub decoded: Vec<SseEvent>,
}

/// Internal marker: the current line or event crossed its cap.
struct Overflowed;

impl SseEvent {
    /// The OpenAI-family end-of-stream marker. The Groq provider treats it as
    /// the protocol terminator and stops reading (R2); the decoder itself
    /// frames it like any other event.
    pub fn is_done_sentinel(&self) -> bool {
        self.data == "[DONE]"
    }
}

#[derive(Debug, Default)]
pub struct SseDecoder {
    /// Raw bytes of the line currently being accumulated.
    line: Vec<u8>,
    /// True when the previous byte was `\r` and we have not yet seen whether the
    /// next byte is the `\n` that completes a CRLF.
    pending_cr: bool,
    /// Data field values accumulated for the event currently being built.
    /// Per the SSE spec multiple `data:` lines join with `\n`.
    data: String,
    /// Whether any `data:` line has been seen for the current event. Needed to
    /// distinguish "no data" from "an empty data line", which are different.
    has_data: bool,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one chunk of bytes. Returns every event that became complete, or
    /// `SseOverflow` the moment a line or event crosses its cap.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseOverflow> {
        let mut out = Vec::new();
        for &b in chunk {
            if self.pending_cr {
                self.pending_cr = false;
                if b == b'\n' {
                    // The `\n` half of a CRLF whose `\r` already ended the line.
                    // Swallow it; emitting here would be a phantom blank line.
                    continue;
                }
                // A lone `\r` ended the previous line. `b` is ordinary content,
                // so fall through and process it normally.
            }
            match b {
                b'\r' => {
                    if self.end_line(&mut out).is_err() {
                        return Err(SseOverflow { decoded: out });
                    }
                    self.pending_cr = true;
                }
                b'\n' => {
                    if self.end_line(&mut out).is_err() {
                        return Err(SseOverflow { decoded: out });
                    }
                }
                _ => {
                    // Checked per byte, before the push: the cap holds while
                    // reading, not after a whole oversized chunk was stored.
                    if self.line.len() >= MAX_LINE_BYTES {
                        return Err(SseOverflow { decoded: out });
                    }
                    self.line.push(b);
                }
            }
        }
        Ok(out)
    }

    /// End of stream. Flushes a trailing unterminated line and then dispatches
    /// any event still being accumulated.
    pub fn finish(&mut self) -> Result<Vec<SseEvent>, SseOverflow> {
        let mut out = Vec::new();
        if !self.line.is_empty() && self.end_line(&mut out).is_err() {
            return Err(SseOverflow { decoded: out });
        }
        self.dispatch(&mut out);
        self.pending_cr = false;
        Ok(out)
    }

    fn end_line(&mut self, out: &mut Vec<SseEvent>) -> Result<(), Overflowed> {
        // A line boundary is always a valid UTF-8 boundary, but the *contents*
        // could still be invalid if the server sent broken bytes. Lossy decode:
        // a mangled character is better than dropping the answer.
        let line = String::from_utf8_lossy(&self.line).into_owned();
        self.line.clear();

        if line.is_empty() {
            // Blank line = dispatch the accumulated event.
            self.dispatch(out);
            return Ok(());
        }
        if line.starts_with(':') {
            // Comment / keep-alive heartbeat. Ignored.
            return Ok(());
        }

        let (field, value) = match line.find(':') {
            Some(i) => {
                let value = &line[i + 1..];
                // Exactly one leading space is stripped, per the SSE spec.
                let value = value.strip_prefix(' ').unwrap_or(value);
                (&line[..i], value)
            }
            // A field with no colon is a field name with an empty value.
            None => (line.as_str(), ""),
        };

        if field == "data" {
            // Many short `data:` lines join into one event, so the event needs
            // its own cap on top of the per-line one.
            if self.data.len() + value.len() + 1 > MAX_EVENT_BYTES {
                return Err(Overflowed);
            }
            if self.has_data {
                self.data.push('\n');
            }
            self.data.push_str(value);
            self.has_data = true;
        }
        // `event`, `id`, `retry` and unknown fields are deliberately dropped.
        Ok(())
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        if !self.has_data {
            return;
        }
        out.push(SseEvent { data: std::mem::take(&mut self.data) });
        self.has_data = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a whole body one byte at a time — the worst realistic chunking.
    fn decode_byte_by_byte(body: &str) -> Vec<String> {
        let mut d = SseDecoder::new();
        let mut got = Vec::new();
        for b in body.as_bytes() {
            got.extend(d.feed(&[*b]).unwrap().into_iter().map(|e| e.data));
        }
        got.extend(d.finish().unwrap().into_iter().map(|e| e.data));
        got
    }

    fn decode_whole(body: &str) -> Vec<String> {
        let mut d = SseDecoder::new();
        let mut got: Vec<String> =
            d.feed(body.as_bytes()).unwrap().into_iter().map(|e| e.data).collect();
        got.extend(d.finish().unwrap().into_iter().map(|e| e.data));
        got
    }

    /// Feed with a single split at position `at`.
    fn decode_split_at(body: &str, at: usize) -> Vec<String> {
        let bytes = body.as_bytes();
        let mut d = SseDecoder::new();
        let mut got: Vec<String> =
            d.feed(&bytes[..at]).unwrap().into_iter().map(|e| e.data).collect();
        got.extend(d.feed(&bytes[at..]).unwrap().into_iter().map(|e| e.data));
        got.extend(d.finish().unwrap().into_iter().map(|e| e.data));
        got
    }

    #[test]
    fn parses_a_simple_lf_stream() {
        assert_eq!(decode_whole("data: a\n\ndata: b\n\n"), vec!["a", "b"]);
    }

    #[test]
    fn parses_a_crlf_stream() {
        assert_eq!(decode_whole("data: a\r\n\r\ndata: b\r\n\r\n"), vec!["a", "b"]);
    }

    #[test]
    fn every_single_split_point_gives_the_same_result_as_one_shot() {
        // This is the invariant that matters: the network decides where chunks
        // break, so *no* split point may change the output. Includes the split
        // between `\r` and `\n`, which is the one that historically corrupts
        // events.
        let bodies = [
            "data: hello\n\ndata: world\n\n",
            "data: hello\r\n\r\ndata: world\r\n\r\n",
            ": keep-alive\n\ndata: x\n\n",
            "event: message\ndata: {\"a\":1}\n\ndata: [DONE]\n\n",
            "data: line1\ndata: line2\n\n",
            "data: trailing-no-newline",
            "data: a\r\rdata: b\r\r",
        ];
        for body in bodies {
            let expected = decode_whole(body);
            for at in 0..=body.len() {
                assert_eq!(
                    decode_split_at(body, at),
                    expected,
                    "split at {at} changed the result for {body:?}"
                );
            }
            assert_eq!(decode_byte_by_byte(body), expected, "byte-by-byte differed for {body:?}");
        }
    }

    #[test]
    fn crlf_split_between_cr_and_lf_does_not_dispatch_twice() {
        // The specific regression: `\r` ends the line, then a chunk boundary,
        // then `\n`. Treating that `\n` as another terminator produces a blank
        // line, which dispatches the event early and splits it in two.
        let body = "data: hello\r\n\r\n";
        let cr_index = body.find('\r').unwrap();
        assert_eq!(decode_split_at(body, cr_index + 1), vec!["hello"]);
    }

    #[test]
    fn multibyte_utf8_split_across_chunks_survives() {
        // An em dash and an emoji are 3 and 4 bytes; the network will happily
        // cut one in half. Buffering raw bytes until a line terminator makes
        // this structurally safe.
        let body = "data: café — 🎉 ok\n\n";
        let expected = vec!["café — 🎉 ok".to_string()];
        assert_eq!(decode_whole(body), expected);
        assert_eq!(decode_byte_by_byte(body), expected);
        for at in 0..=body.len() {
            assert_eq!(decode_split_at(body, at), expected, "split at {at}");
        }
    }

    #[test]
    fn done_sentinel_is_framed_like_any_event_not_swallowed() {
        // The decoder is framing only: it hands `[DONE]` and whatever follows
        // to the caller, and the Groq provider decides that `[DONE]` ends the
        // answer (R2). A decoder that swallowed or stopped at the sentinel
        // would take that protocol decision away from the one place that
        // owns it.
        let events = decode_whole("data: a\n\ndata: [DONE]\n\ndata: b\n\n");
        assert_eq!(events, vec!["a", "[DONE]", "b"]);
        assert!(SseEvent { data: "[DONE]".into() }.is_done_sentinel());
        assert!(!SseEvent { data: "a".into() }.is_done_sentinel());
    }

    #[test]
    fn unterminated_final_data_line_is_flushed_at_end_of_stream() {
        // Without the flush, the last words of the answer vanish — the failure
        // looks like the model trailed off rather than like a parser bug.
        assert_eq!(decode_whole("data: the end"), vec!["the end"]);
        assert_eq!(decode_whole("data: a\n\ndata: partial"), vec!["a", "partial"]);
    }

    #[test]
    fn event_without_trailing_blank_line_is_flushed_at_end_of_stream() {
        assert_eq!(decode_whole("data: a\n"), vec!["a"]);
    }

    #[test]
    fn comment_and_keepalive_lines_are_ignored() {
        assert_eq!(decode_whole(": ping\n\ndata: a\n\n"), vec!["a"]);
        // A bare comment must not dispatch a phantom empty event.
        assert_eq!(decode_whole(": ping\n\n"), Vec::<String>::new());
    }

    #[test]
    fn non_data_fields_are_ignored() {
        assert_eq!(
            decode_whole("event: content_block_delta\nid: 7\nretry: 100\ndata: payload\n\n"),
            vec!["payload"]
        );
    }

    #[test]
    fn multiple_data_lines_join_with_newline() {
        assert_eq!(decode_whole("data: one\ndata: two\n\n"), vec!["one\ntwo"]);
    }

    #[test]
    fn exactly_one_leading_space_is_stripped_from_the_value() {
        // "data:  x" (two spaces) must keep one — answer text can legitimately
        // begin with a space and losing it welds words together.
        assert_eq!(decode_whole("data:  x\n\n"), vec![" x"]);
        assert_eq!(decode_whole("data:x\n\n"), vec!["x"]);
    }

    #[test]
    fn empty_data_line_dispatches_an_empty_event_not_nothing() {
        assert_eq!(decode_whole("data:\n\n"), vec![""]);
    }

    #[test]
    fn field_with_no_colon_is_ignored_without_crashing() {
        assert_eq!(decode_whole("weird-line\ndata: a\n\n"), vec!["a"]);
    }

    #[test]
    fn repeated_blank_lines_do_not_emit_phantom_events() {
        assert_eq!(decode_whole("\n\n\ndata: a\n\n\n\n"), vec!["a"]);
    }

    #[test]
    fn empty_stream_yields_nothing() {
        assert_eq!(decode_whole(""), Vec::<String>::new());
        let mut d = SseDecoder::new();
        assert!(d.feed(&[]).unwrap().is_empty());
        assert!(d.finish().unwrap().is_empty());
    }

    #[test]
    fn invalid_utf8_is_replaced_rather_than_dropping_the_event() {
        // Losing a whole delta because one byte was mangled is worse than
        // rendering a replacement character.
        let mut d = SseDecoder::new();
        let mut bytes = b"data: ab".to_vec();
        bytes.push(0xFF);
        bytes.extend_from_slice(b"cd\n\n");
        let events = d.feed(&bytes).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].data.starts_with("ab"));
        assert!(events[0].data.ends_with("cd"));
    }

    #[test]
    fn lone_cr_terminates_a_line() {
        // Legal SSE line terminators are CRLF, LF, and a bare CR.
        assert_eq!(decode_whole("data: a\r\rdata: b\r\r"), vec!["a", "b"]);
    }

    #[test]
    fn json_split_mid_object_still_reassembles() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let expected = decode_whole(body);
        assert_eq!(expected.len(), 1);
        for at in 0..=body.len() {
            assert_eq!(decode_split_at(body, at), expected, "split at {at}");
        }
    }

    #[test]
    fn a_line_with_no_terminator_is_refused_at_the_cap_while_reading() {
        // R2: a server that never sends a newline must fail the feed at the
        // chunk that crosses the cap, not after the whole body was stored.
        // 64 KiB chunks after the 6-byte "data: " prefix: 15 fit under the
        // 1 MiB cap, the 16th is the one that crosses it.
        let mut d = SseDecoder::new();
        let chunk = vec![b'x'; 64 * 1024];
        assert!(d.feed(b"data: ").is_ok());
        let mut refused_at = None;
        for i in 0..32 {
            if d.feed(&chunk).is_err() {
                refused_at = Some(i);
                break;
            }
        }
        assert_eq!(refused_at, Some(15));
    }

    #[test]
    fn an_event_joined_from_many_data_lines_is_capped_too() {
        // Each line is small, so only the per-event cap can stop this one.
        let mut d = SseDecoder::new();
        let line = format!("data: {}\n", "y".repeat(1000));
        let mut result = Ok(Vec::new());
        for _ in 0..(MAX_EVENT_BYTES / 1000 + 2) {
            result = d.feed(line.as_bytes());
            if result.is_err() {
                break;
            }
        }
        assert!(result.is_err());
    }

    #[test]
    fn frames_at_the_line_cap_still_decode() {
        // The caps must not clip a large-but-legal frame: a line of exactly
        // MAX_LINE_BYTES is accepted.
        let payload = "z".repeat(MAX_LINE_BYTES - "data: ".len());
        let body = format!("data: {payload}\n\n");
        let events = decode_whole(&body);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].len(), payload.len());
    }

    #[test]
    fn events_framed_before_an_overflow_in_the_same_chunk_are_still_delivered() {
        // R1 review F3: "a" is complete before the oversized line starts, so
        // it must reach the caller whether the bytes arrive in one chunk or
        // two — otherwise the partial text shown depends on network chunking.
        let mut d = SseDecoder::new();
        let mut chunk = b"data: a\n\n".to_vec();
        chunk.extend(vec![b'x'; MAX_LINE_BYTES + 1]);
        let err = d.feed(&chunk).unwrap_err();
        assert_eq!(err.decoded, vec![SseEvent { data: "a".into() }]);
    }
}

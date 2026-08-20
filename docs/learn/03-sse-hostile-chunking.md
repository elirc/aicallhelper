# 03 — Streaming protocol parsing under hostile chunking

**Concept.** TCP delivers a byte stream, not messages. Every framing decision
your protocol makes — "lines end in `\r\n`", "events end at a blank line" —
can be cut by the network at *any byte*, including between the two bytes of
`\r\n` and between the four bytes of an emoji. A parser that is correct on
whole messages and wrong on fragments will pass every hand-written test and
fail in production at a rate proportional to your traffic. The senior insight
is that this is not a bug class you fix; it is a bug class you make
**unrepresentable** — with two moves:

1. **State machines over lookahead.** Never peek at "the next byte" — it may
   not exist yet. Carry a state bit across the chunk boundary instead.
2. **An exhaustive cut-point test.** For every interesting body, assert that
   *every possible split* produces byte-identical output to a one-shot parse.
   That test doesn't check the split you thought of; it checks all of them.

**Where this repo stakes its life on it.** Both LLM providers stream SSE, and
the decoder (`src-tauri/core/src/llm/sse.rs`) sits directly on the product
promise: a mis-framed event is a corrupted or truncated answer painted in
front of the user mid-interview. The module header (sse.rs:1-21) enumerates
each requirement as "a way a naive implementation loses the end of an answer."

## The split-point invariant, and the byte that breaks naive parsers

The whole decoder is a per-byte state machine over four fields
(sse.rs:39-52): the raw bytes of the current line, a `pending_cr` flag, the
accumulated `data`, and `has_data`. The interesting one:

```rust
// sse.rs:43-45
/// True when the previous byte was `\r` and we have not yet seen whether the
/// next byte is the `\n` that completes a CRLF.
pending_cr: bool,
```

SSE allows three line terminators: `\n`, `\r\n`, and lone `\r`. So when you
see `\r`, the line is over — but you don't yet know whether the *next* byte is
the second half of a CRLF (to be swallowed) or the first byte of the next line
(to be processed). If the chunk ends right there, the answer arrives in the
future. `pending_cr` carries the question across the boundary:

```rust
// sse.rs:60-82 (abridged)
pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
    let mut out = Vec::new();
    for &b in chunk {
        if self.pending_cr {
            self.pending_cr = false;
            if b == b'\n' {
                // The `\n` half of a CRLF whose `\r` already ended the line.
                // Swallow it; emitting here would be a phantom blank line.
                continue;
            }
            // A lone `\r` ended the previous line. `b` is ordinary content...
        }
        match b {
            b'\r' => { self.end_line(&mut out); self.pending_cr = true; }
            b'\n' => self.end_line(&mut out),
            _ => self.line.push(b),
        }
    }
    out
}
```

Why the phantom blank line matters: in SSE, a blank line means *dispatch the
accumulated event*. A decoder that treats a chunk-final `\r` as a complete
ending and then sees `\n` at the head of the next chunk emits a spurious blank
line — and dispatches the event early, cutting it in two (sse.rs:7-11). That
is the historical regression pinned by
`crlf_split_between_cr_and_lf_does_not_dispatch_twice` (sse.rs:214-220).

**UTF-8 is solved structurally, not cleverly** (sse.rs:12-15): the decoder
buffers *raw bytes* per line and only decodes at a terminator, because `\r`
and `\n` can never occur inside a multi-byte UTF-8 sequence — a line boundary
is always a safe decode point. No partial-codepoint bookkeeping exists because
none is needed. (Content that is genuinely invalid UTF-8 gets a lossy decode
at sse.rs:98-101: "a mangled character is better than dropping the answer.")

## The two end-of-stream traps

**`[DONE]` is a sentinel, not a terminator** (sse.rs:31-37). Groq can pack
`data: [DONE]` and further bytes into one chunk; a read loop that `break`s on
the sentinel drops whatever followed. So the decoder never stops — the
*caller* skips sentinel events. Pinned by
`done_sentinel_is_skipped_not_treated_as_a_terminator`.

**A stream can end without a trailing newline.** The last `data:` line of a
truncated response still carries real answer text:

```rust
// sse.rs:86-95
/// End of stream. Flushes a trailing unterminated line and then dispatches
/// any event still being accumulated.
pub fn finish(&mut self) -> Vec<SseEvent> {
    let mut out = Vec::new();
    if !self.line.is_empty() {
        self.end_line(&mut out);
    }
    self.dispatch(&mut out);
    self.pending_cr = false;
    out
}
```

Dropping it "silently truncates the answer's last words, which reads to the
user as the model trailing off" (sse.rs:16-19).

## The test that makes it unbreakable

```rust
// sse.rs:187-212 (abridged)
let bodies = [ /* seven bodies covering CRLF, comments, [DONE],
                  multi-line data, no trailing newline, lone CR */ ];
for body in bodies {
    let expected = decode_whole(body);
    for at in 0..=body.len() {
        assert_eq!(decode_split_at(body, at), expected,
            "split at {at} changed the result for {body:?}");
    }
    assert_eq!(decode_byte_by_byte(body), expected, ...);
}
```

This is a property test with an enumerable domain: for a body of n bytes,
all n+1 single splits plus fully byte-by-byte feeding must equal the one-shot
decode. You do not have to *predict* which split is dangerous — the CR|LF
split, the mid-UTF-8 split, the split inside `[DONE]` — they are all in
`0..=body.len()`. When you add a feature to the decoder, you add a body to the
array and the test re-derives every dangerous split for you. The remaining 18
tests in the file (see `docs/TESTING.md`, sse section) pin *semantic* choices
the invariant can't express: one leading space stripped, empty `data:` ≠ no
data, comment lines invisible, `event:`/`id:`/`retry:` ignored.

## Exercises

**Reading 1.** `has_data` exists so the decoder can "distinguish 'no data'
from 'an empty data line', which are different" (sse.rs:50-52). Trace what
each produces and why the difference could matter downstream.

<details><summary>Answer</summary>

A blank line with `has_data == false` dispatches nothing
(`dispatch` returns early, sse.rs:135-138); `data:` followed by a blank line
dispatches an event whose payload is `""`. If the decoder collapsed the two,
either repeated blank lines would fabricate phantom empty events (pinned by
`repeated_blank_lines_do_not_emit_phantom_events` — blank-line runs happen at
chunk boundaries), or genuinely empty events would vanish (pinned by
`empty_data_line_dispatches_an_empty_event_not_nothing`). Downstream, the
providers parse each event's JSON; an invented empty event is a parse failure
path exercised for no reason, and a swallowed real one desynchronizes any
protocol that counts events.
</details>

**Reading 2.** `finish()` resets `pending_cr` but `feed` never times out a
pending CR — if the stream ends with a final `\r` and `finish` is called, what
happens to that last line, and why is this correct rather than lucky?

<details><summary>Answer</summary>

The `\r` already called `end_line` *before* setting `pending_cr`
(sse.rs:74-77) — ending the line is never deferred; only the
swallow-or-process decision about the *next* byte is. So at `finish`, the line
buffer is empty, the event data (if any) is dispatched by the unconditional
`dispatch`, and `pending_cr` is cleared only as hygiene for decoder reuse.
This is the state-machine discipline in miniature: do everything you can with
the bytes you have, and make the pending flag encode only the genuinely
unknowable — never buffer work that doesn't need the future.
</details>

**Break it.** In `finish()` (sse.rs:87-95), delete the line flush:

```rust
pub fn finish(&mut self) -> Vec<SseEvent> {
    let mut out = Vec::new();
    self.dispatch(&mut out);
    self.pending_cr = false;
    out
}
```

Run `cargo test -p app-core unterminated_final_data_line_is_flushed_at_end_of_stream`.

It fails: the body ends with `data: ...` and no newline, so the final line is
still sitting in `self.line` as raw bytes — never parsed, so `has_data` was
never set, so `dispatch` emits nothing and the event evaporates. Note which
test does *not* fail: `every_single_split_point_gives_the_same_result_as_one_shot`
computes its expected value with the same broken decoder, so it is
consistently wrong and stays green. That is the sharpest lesson in this file:
**an invariant test proves self-consistency, not correctness** — you still
need at least one test that pins the absolute expected output, the way
`unterminated_final_data_line_is_flushed_at_end_of_stream` pins
`vec!["the end"]`, or your property test will happily bless a decoder that
loses the last words of every truncated answer.

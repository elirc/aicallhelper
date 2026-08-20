# 04 — The transcript accumulator: O(1) incremental state and literal-true

**Concept.** Two habits separate code that survives streaming input from code
that doesn't. First: when a value grows over time, maintain it
**incrementally** — append to running state — instead of recomputing it from
the full history on every update; the recompute version is quadratic and the
cost hides until the input gets long. Second: when the input is a wire
protocol you don't control, **type-check it literally** — `true` means the
JSON boolean `true`, not "truthy" — because the sender has shipped adjacent
fields as `"true"` and `1` before, and your leniency becomes their bug's blast
radius.

**Where this repo stakes its life on it.** `src-tauri/core/src/stt/frame.rs`
parses Deepgram's JSON frames and maintains the live transcript. The header
sets the stakes: "This is pure and total: every input, including deliberately
hostile JSON, maps to a value rather than a panic. A crash here would take
down a live recording mid-call" (frame.rs:3-6).

## Deepgram's contract, and the accumulator that mirrors it

Deepgram streams two kinds of `Results`: **interims** (speculative, revised
freely) and **finals** (committed, never revised). The visible transcript at
any moment is *committed prefix + latest interim*. The accumulator stores
exactly those two strings:

```rust
// frame.rs:92-103
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
```

Do the arithmetic the comment implies: a 120 s recording at ~10 frames/s is
~1200 frames. A `Vec<String>` of segments re-joined per frame walks all
previous text every time — sum over frames of O(total-so-far), quadratic in
the recording length, and it runs *inside the latency budget* on the same
runtime that is racing to finalize. The accumulator's `apply`
(frame.rs:111-138) does one `push_str` per final instead. Same output,
different complexity class — and the difference only shows up on long
recordings, which is why it must be a design rule, not a profiling discovery.

`apply` returns whether the *visible* transcript changed, and that boolean is
load-bearing: Deepgram re-sends identical interims, and the Deepgram driver
only emits `stt:partial` when `changed` is true (deepgram.rs:402-413), so the
UI isn't repainted for nothing. Pinned by
`repeating_an_identical_interim_reports_no_change`.

Note also the empty final (frame.rs:117-127): "An empty final is a normal
silence marker, not text. It still clears the interim, because whatever was
tentative is now gone." Silence markers must neither add stray spaces
(`empty_finals_do_not_insert_stray_spaces`) nor leave a retracted interim on
screen (`an_empty_final_clears_a_pending_interim`).

## Literal true

```rust
// frame.rs:48-52
// `is_final` must be *literally* the boolean true. Deepgram has shipped
// frames carrying "true" and 1 in adjacent fields; treating a truthy
// imposter as final permanently commits text that is still being revised,
// so the transcript ends up with the same phrase twice.
let is_final = matches!(value.get("is_final"), Some(Value::Bool(true)));
```

Work through the failure so it sticks. Deepgram sends interim `"tell me"`,
then interim `"tell me about"`, then final `"tell me about yourself"`. If some
frame's `is_final` arrives as the *string* `"true"` and you treat it as final,
you commit `"tell me about"` into the immutable prefix — and when the real
final lands, the transcript reads `"tell me about tell me about yourself"`.
The duplicated phrase then goes to the LLM as the question. A one-character
leniency (`is_some()` instead of `matches!`) becomes a garbled answer in a
live interview. Pinned by `is_final_must_be_literally_true` (frame.rs:190-199),
which feeds `"true"`, `1`, `"1"`, `[]`, `{}`, and `null` and requires all of
them to read as interim.

The transcript extraction has the same posture — a five-link `and_then` chain
(frame.rs:56-64) where *any* missing/null/wrong-typed link yields `Ignored`
rather than committing `""` or panicking on an unwrap. And parsing is capped
structurally: serde_json's internal recursion limit turns 5000-deep nested
JSON into an `Err` instead of a stack overflow that aborts the process
mid-call (`pathologically_nested_json_is_ignored_without_stack_overflow`,
frame.rs:26-28).

## Honest degradation at the edges

`finalized_text` (frame.rs:156-162) prefers committed text but will hand back
a lone interim: "if Deepgram closed while only an interim existed, that
interim is still the user's question and is far better than answering
nothing" (`an_interim_that_never_finalizes_is_still_usable_as_the_question`).
And `is_empty` treats whitespace as nothing (frame.rs:164-166) — it feeds the
§5.7 `no_speech` decision, where whitespace must not count as a question.

## Exercises

**Reading 1.** `parse_frame` distinguishes three outcomes — `Results`,
`Error`, `Ignored` — and `Ignored` covers both "frame types we know and don't
care about" and "garbage". Why is collapsing malformed input into `Ignored`
(rather than a fourth `Malformed` variant surfaced as an error) the right
product call *here*, when lesson 05's driver treats an `Error` frame as fatal?

<details><summary>Answer</summary>

Because the two mean different things about the *stream's* health. An `Error`
frame is Deepgram affirmatively saying the stream is broken — continuing means
silently transcribing nothing (§5.5 makes that fatal). A frame we can't parse
says nothing about the stream: it is "not a reason to abandon a recording"
(frame.rs:19-21) — most plausibly a new frame type
(`SomethingDeepgramAddsIn2027` is literally in the test corpus) or one mangled
message among hundreds. Killing a live mid-interview recording over a frame
we merely didn't understand converts a nothing-event into the worst outcome.
Forward compatibility is a *policy* (ignore the unknown), and the tests pin
it as policy: `metadata_and_unknown_types_are_ignored`,
`malformed_json_is_ignored_never_panics`.
</details>

**Reading 2.** `error_detail` (frame.rs:75-90) iterates
`["code", "variant", "description", "message"]`, dedupes, joins with `": "`,
and falls back to a canned sentence. Why this much ceremony for an error
string?

<details><summary>Answer</summary>

Because two wire shapes exist in the field — v1 listen sends
`{description, message, variant}`, newer errors send `{code, description}`
(§6.1) — and the detail is "the only diagnostic the user sees"
(`parses_the_v1_listen_error_shape`, `parses_the_newer_code_description_error_shape`).
The dedupe guards shapes where `description` and `message` carry the same
text; the fallback guards a bare `{"type":"Error"}`, because "an empty error
message is unactionable" (`error_frame_with_no_detail_still_says_something`).
It is the repo-wide error voice applied at the parser: quote what the wire
gave you, never less, never a hunt.
</details>

**Break it.** In `parse_results` (frame.rs:52), loosen the literal check:

```rust
let is_final = value.get("is_final").is_some();
```

Run `cargo test -p app-core is_final_must_be_literally_true`.

It fails on the first imposter: `"true"` now parses as
`Results { .., is_final: true }` where the test demands interim — the
duplicated-phrase bug from above, one keystroke away. Then run the full file
and count the collateral: `parses_a_normal_interim_result` fails too, because
a normal interim carries `is_final: false`, and `Some(Bool(false))` is still
`is_some()` — the "lenient" edit didn't just admit imposters, it inverted the
meaning of every legitimate interim in the protocol. That is the general
lesson about leniency toward wire input: its blast radius is always wider
than the case you were being lenient for, and only a test that enumerates the
imposters (`"true"`, `1`, `"1"`, `[]`, `{}`, `null`) makes the strict check
survive the next well-meaning refactor.

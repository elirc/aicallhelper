# Testing

Every test in this repo runs with **no network, no audio device, and no live provider**. The rule is enforced by construction:

- **Rust core (`app-core`)** — every external dependency sits behind a trait (`SttConnector`/`SttStream`, `LlmProvider`/`LlmSink`, `AudioCapture`/`AudioSink`, `EventSink`). The session state machine is tested against scripted fakes with a paused tokio clock; the Anthropic/Groq/Deepgram clients are tested against scripted local servers on `127.0.0.1` (raw TCP for HTTP/SSE, tokio-tungstenite for the Deepgram WebSocket) so the tests control the exact bytes on the wire, including malformed ones.
- **Tauri shell (`src-tauri/src`)** — unit tests cover the pure seams (envelope shapes, event wire mapping, debounce/limiter/URL-guard logic, timestamp formatting). These compile with the shell crate, not `app-core`.
- **Frontend** — the Tauri IPC surface is a single `Bridge` object; tests install a fake via `setBridge()` (and `vi.mock` the `@tauri-apps/api` modules in `bridge.test.ts`) and emit core events exactly as the Rust side would.

Commands:

- Rust core: `cargo test -p app-core` (run from `src-tauri/`) — **242 tests**.
- Tauri shell: `cargo test` from `src-tauri/` also compiles the shell crate's **35 tests** (commands/events/window/logging).
- Frontend: `npm test` (`vitest run`) — **293 test cases** (248 `it(...)` declarations; parameterized `it.each` blocks expand to the larger runtime count, noted per file below).

Files with **no** tests (so nothing is missing, just absent by design): `core/src/lib.rs`, `core/src/llm/mod.rs`, `core/src/stt/mod.rs`, `core/src/store/mod.rs`, `src/main.rs`, `src/state.rs`.

---

## Rust core — errors

### `core/src/error.rs` (2 tests)

- `error_codes_serialize_to_the_documented_wire_strings` — every `ErrorCode` serializes to its exact snake_case wire literal; **why**: the frontend switches on these strings, so a rename is a silent behavior change across the IPC boundary.
- `aborted_is_recognisable_so_the_ui_can_stay_silent` — `is_aborted()` is true only for the aborted error; **why**: "aborted" must be detectable so the UI never renders an error for a user-initiated cancel.

## Rust core — LLM

### `core/src/llm/prompt.rs` (14 tests)

- `role_instructions_are_verbatim` — pins `ROLE_INSTRUCTIONS` byte-for-byte; **why**: the prompt text is product behavior — "improving" the wording is a product decision and must break a test.
- `empty_profile_yields_role_instructions_only_and_no_grounding_note` — no resume/JD means no headers and no grounding note; **why**: telling the model to ground in an absent resume produces hedging about nothing.
- `whitespace_only_profile_counts_as_absent` — whitespace-only resume/JD is treated as empty; **why**: a blank-looking profile must not smuggle in headers and the grounding note.
- `resume_only_still_gets_the_grounding_note` — resume without JD still appends the note; **why**: grounding applies whenever either section exists.
- `jd_only_still_gets_the_grounding_note` — JD without resume still appends the note; **why**: same rule from the other side.
- `sections_appear_in_resume_then_jd_order` — resume header precedes the JD header; **why**: a stable section order keeps the cached prefix byte-stable.
- `interior_resume_formatting_survives_verbatim` — only the edges are trimmed; **why**: a resume's internal blank lines and indentation carry meaning the model reads.
- `style_lives_outside_the_cached_prefix` — all three styles share one identical `cached_prefix`; **why**: a style flip must not invalidate the Anthropic prompt cache (the whole point of the split, §3).
- `style_suffixes_are_verbatim` — pins all three style strings byte-for-byte; **why**: same "strings are product behavior" rule as the role instructions.
- `prompt_is_byte_stable_across_repeated_builds` — 50 rebuilds produce identical output; **why**: cache hits are a byte-prefix match — any nondeterminism silently costs a cache write per call.
- `unknown_style_falls_back_to_balanced` — corrupt/unknown style strings parse as Balanced; **why**: the settings file is user-writable, so garbage is a real input path (§7).
- `user_message_wrapper_is_verbatim` — pins the triple-quote transcript wrapper; **why**: the wrapper distinguishes speech from instruction and lives outside the cached prefix.
- `transcript_is_not_escaped_or_trimmed_by_the_wrapper` — quotes and punctuation survive; **why**: the transcript is data, not markup — mangling it changes the question being answered.
- `joined_prompt_places_style_after_the_prefix` — `joined()` is prefix + blank line + suffix; **why**: Groq gets one system string and it must read like the two Anthropic blocks.

### `core/src/llm/sse.rs` (19 tests)

- `parses_a_simple_lf_stream` — baseline LF-terminated events decode; **why**: the trivial case every other guarantee builds on.
- `parses_a_crlf_stream` — CRLF-terminated events decode; **why**: real providers send CRLF.
- `every_single_split_point_gives_the_same_result_as_one_shot` — for seven bodies, every possible chunk split (and byte-by-byte) equals a one-shot decode; **why**: the network decides where chunks break, so no split point may change the output.
- `crlf_split_between_cr_and_lf_does_not_dispatch_twice` — the split between `\r` and `\n` yields one event; **why**: the historical regression where the trailing `\n` reads as a blank line and dispatches the event early, cutting it in two.
- `multibyte_utf8_split_across_chunks_survives` — em dash/emoji cut mid-character reassemble; **why**: the network will cut a UTF-8 sequence in half; buffering raw bytes must make this safe.
- `done_sentinel_is_skipped_not_treated_as_a_terminator` — `[DONE]` passes through without stopping the decoder; **why**: Groq can pack `[DONE]` and further bytes into one chunk — stopping at the sentinel drops what follows.
- `unterminated_final_data_line_is_flushed_at_end_of_stream` — a trailing unterminated `data:` line is emitted at `finish()`; **why**: otherwise the last words of an answer vanish, looking like the model trailed off.
- `event_without_trailing_blank_line_is_flushed_at_end_of_stream` — a final event missing its blank line still flushes; **why**: same end-of-stream loss mode.
- `comment_and_keepalive_lines_are_ignored` — `:`-comment lines produce nothing, not phantom empty events; **why**: keep-alives are routine and must be invisible.
- `non_data_fields_are_ignored` — `event:`/`id:`/`retry:` lines are skipped; **why**: only `data:` carries payload in this protocol use.
- `multiple_data_lines_join_with_newline` — consecutive `data:` lines join with `\n`; **why**: SSE spec behavior a hand-rolled decoder can easily get wrong.
- `exactly_one_leading_space_is_stripped_from_the_value` — `data:  x` keeps one space, `data:x` keeps none; **why**: answer text can legitimately start with a space; losing it welds words together.
- `empty_data_line_dispatches_an_empty_event_not_nothing` — `data:` alone yields an empty-string event; **why**: empty and absent are different events downstream.
- `field_with_no_colon_is_ignored_without_crashing` — a colonless line is skipped; **why**: resilience to malformed frames without panicking mid-answer.
- `repeated_blank_lines_do_not_emit_phantom_events` — extra blank lines produce no events; **why**: blank-line runs happen at chunk boundaries.
- `empty_stream_yields_nothing` — empty feed and empty finish produce nothing; **why**: degenerate input must not fabricate events.
- `invalid_utf8_is_replaced_rather_than_dropping_the_event` — a bad byte becomes U+FFFD, the event survives; **why**: losing a whole delta over one mangled byte is worse than a replacement character.
- `lone_cr_terminates_a_line` — bare-CR line endings decode; **why**: CR is a legal SSE terminator alongside LF and CRLF.
- `json_split_mid_object_still_reassembles` — a JSON payload split at every byte reassembles to one event; **why**: the split-invariance guarantee applied to the exact payload shape providers send.

### `core/src/llm/retry.rs` (8 tests)

- `connection_failure_before_any_delta_is_retried_exactly_once` — attempt indices are exactly `[0, 1]`; **why**: pre-response connection failures get one retry, never more.
- `http_status_error_is_not_retried` — an HTTP error runs one attempt; **why**: the server answered — repeating the question changes nothing and burns first-token budget.
- `connection_failure_after_a_delta_is_not_retried` — a retryable failure after `mark_delta_emitted()` is vetoed; **why**: THE rule preventing two answers being concatenated in the appending UI.
- `aborted_error_is_not_retried` — abort vetoes even a predicate that allows everything; **why**: cancelled work must never be re-run.
- `cancelled_token_short_circuits_before_any_attempt` — a pre-cancelled token means zero attempts; **why**: no request may be spent on work the user already cancelled.
- `cancellation_during_the_first_attempt_yields_aborted_not_the_http_error` — cancel mid-attempt surfaces `aborted()`; **why**: the UI never renders aborted — an HTTP-flavoured error for abandoned work would show a spurious banner.
- `failure_on_the_second_attempt_propagates_the_second_error` — the second error wins; **why**: the second error describes current reality; surfacing the first sends the user debugging a connection already replaced.
- `both_attempts_receive_the_identical_body` — same allocation, same bytes across both attempts; **why**: Anthropic's cache keys on the exact byte prefix — even a semantically-equal rebuild can regress it.

### `core/src/llm/warm.rs` (5 tests)

- `throttle_blocks_within_two_seconds_then_reopens` — a warm fires, denies for <2s, fires at 2s; **why**: pre-warm must not hammer the provider on rapid Record presses.
- `denied_warms_do_not_extend_the_window` — denials don't refresh the timestamp; **why**: otherwise rapid-fire presses would starve warming forever — the opposite of the throttle's purpose.
- `origins_are_throttled_independently` — two origins each get their own window; **why**: warming Anthropic must not suppress warming Groq when the user switches provider.
- `a_fired_warm_resets_its_own_window` — each fired warm becomes the new reference point; **why**: the window is measured from the last real warm, not from first use.
- `prewarm_never_panics_without_a_runtime` — `prewarm()` outside tokio is a silent no-op; **why**: a best-effort optimization must never take the caller down.

### `core/src/llm/http.rs` (2 tests)

- `shared_client_is_the_same_instance_on_every_call` — pointer identity across calls; **why**: pre-warm only works if the warm GET and the answer POST share one connection pool, which the client instance owns.
- `shared_client_is_the_same_instance_across_threads` — pointer identity across spawned threads; **why**: the warm task and the answer request run on different tokio workers.

### `core/src/llm/anthropic.rs` (15 tests)

- `happy_path_streams_every_delta_and_returns_their_exact_concatenation` — deltas stream in order, the returned answer is byte-identical to their concatenation, and `cache_read_input_tokens` is exposed; **why**: the invariant that stops text changing after the user has read it.
- `text_deltas_across_multiple_content_blocks_join_with_nothing_between` — two content blocks join with no separator; **why**: any injected joiner corrupts the visible answer.
- `request_pins_model_streaming_max_tokens_and_the_two_system_blocks` — captured request carries the key/version headers, pinned model, `stream:true`, `max_tokens`, and two system blocks with `cache_control` on the FIRST only; **why**: a breakpoint on the style block would invalidate the profile cache on every style flip.
- `status_401_maps_to_llm_auth_and_names_the_status` — 401 → `LlmAuth`, message names 401 and points at Settings; **why**: auth failures must route the user to the fix, not a generic error.
- `status_403_maps_to_llm_auth_and_names_the_status` — 403 → `LlmAuth` naming 403; **why**: same class, correct status in the message.
- `status_429_maps_to_llm_rate_limit_and_names_the_status` — 429 → `LlmRateLimit`; **why**: rate limits have their own UI treatment.
- `status_529_maps_to_llm_http_overloaded_and_names_the_status` — 529 → `LlmHttp` mentioning "overloaded"; **why**: Anthropic's overload status deserves its own explanation, not a generic 5xx.
- `other_status_maps_to_llm_http_with_status_and_body_snippet` — 500 keeps the status and body detail; **why**: the body snippet is the only clue the user gets.
- `long_error_bodies_are_truncated_in_the_message` — a 2000-char body yields a <400-char message; **why**: never dump a whole error page into the UI.
- `error_event_mid_stream_surfaces_as_llm_http_quoting_the_detail` — an SSE `error` event after a delta becomes `LlmHttp` with the detail; **why**: mid-stream provider errors must surface, not silently truncate.
- `connection_failure_maps_to_llm_http_with_actionable_message` — dial to a dead port yields "Could not reach Anthropic"; **why**: connection failures need a message the user can act on.
- `empty_200_body_is_llm_http_not_a_silent_empty_answer` — 200 with no events is an error; **why**: a blank answer panel with no explanation is the worst outcome.
- `mid_stream_connection_drop_maps_to_llm_http` — cut mid-body → `LlmHttp` "dropped while the answer was streaming", earlier deltas still delivered; **why**: partial answers must be kept and the drop named.
- `cancellation_mid_stream_returns_aborted_never_an_http_error` — cancel on first delta against a stalled server returns `aborted`; **why**: cancellation must not depend on the server sending more bytes, and must never surface as HTTP.
- `empty_api_key_returns_no_llm_key_without_connecting` — empty key fails with `NoLlmKey` against a dead port; **why**: the missing-key check must fire before any socket is dialed.

### `core/src/llm/groq.rs` (15 tests)

- `happy_path_streams_every_delta_and_returns_their_exact_concatenation` — OpenAI-shape chunks stream and concatenate byte-identically; **why**: same read-stability invariant as Anthropic.
- `done_sentinel_followed_by_more_data_in_the_same_chunk_does_not_truncate` — `[DONE]` mid-chunk does not drop trailing deltas; **why**: a decoder/loop treating `[DONE]` as a terminator loses the tail of the answer.
- `request_pins_model_streaming_reasoning_knobs_and_omits_reasoning_format` — captured request carries Bearer auth, pinned model, `stream`, `temperature`, `max_completion_tokens`, `reasoning_effort:"low"`, `include_reasoning:false`, no `reasoning_format`, and the system prompt as ONE joined string; **why**: `reasoning_format` is a Qwen-family knob — sent to gpt-oss it is at best ignored, at worst a future request error.
- `status_401_maps_to_llm_auth_and_names_the_status` — 401 → `LlmAuth`; **why**: route to the key fix.
- `status_403_maps_to_llm_auth_and_reports_403_not_401` — 403 message contains 403 and not 401; **why**: a 403 labelled 401 sends the user debugging the wrong thing.
- `status_429_maps_to_llm_rate_limit_and_names_the_status` — 429 → `LlmRateLimit`; **why**: distinct rate-limit handling.
- `status_404_maps_to_llm_http_and_points_at_the_pinned_model_constant` — 404 message says "retired" and "pinned model constant"; **why**: Groq retires models; the fix is editing the constant and the error must say so.
- `status_500_maps_to_llm_http_groq_unavailable` — 500 → "unavailable"; **why**: server-side outage wording, not a user-fault message.
- `status_503_also_maps_to_llm_http_groq_unavailable` — 503 same mapping; **why**: both 5xx flavors of "Groq is down".
- `connection_failure_maps_to_llm_http_with_actionable_message` — dead port → "Could not reach Groq"; **why**: actionable connection-failure wording.
- `empty_200_body_is_llm_http_not_a_crash_and_not_a_silent_empty_answer` — 200 with no events errors; **why**: blank answers must be loud.
- `mid_stream_connection_drop_maps_to_llm_http_with_dropped_wording` — cut mid-body → "connection dropped", delivered deltas kept; **why**: keep the partial, name the drop.
- `stream_drop_before_any_delta_is_retried_once_with_an_identical_body` — a 200 whose stream dies before ANY delta is silently retried once with a byte-identical body, and the second attempt's answer streams normally; **why**: a pre-delta drop is a connection-level failure with nothing at risk of duplication — the one case where a silent retry beats surfacing an error one second into the latency window (the delta-emitted veto, proven in `retry.rs` and the mid-stream-drop test, is what makes it safe).
- `cancellation_mid_stream_returns_aborted_never_an_http_error` — cancel against a stalled server → `aborted`; **why**: cancellation is self-contained and never HTTP-flavoured.
- `empty_api_key_returns_no_llm_key_without_connecting` — empty key → `NoLlmKey`, no dial; **why**: fail at the gate, not at the provider.

## Rust core — STT

### `core/src/stt/frame.rs` (19 tests)

- `parses_a_normal_interim_result` — a standard interim Results frame parses; **why**: baseline for the parser.
- `parses_a_normal_final_result` — a standard final frame parses with `is_final:true`; **why**: finals drive the commit path.
- `is_final_must_be_literally_true` — `"true"`, `1`, `[]`, `{}`, `null` all read as interim; **why**: committing on a truthy imposter appends text Deepgram then revises, duplicating the phrase.
- `missing_is_final_is_interim` — absent `is_final` means interim; **why**: absence must not commit.
- `frames_without_a_usable_transcript_are_ignored` — ten missing/null/wrong-type transcript shapes → `Ignored`; **why**: each would otherwise commit an empty segment or panic on an unwrap.
- `an_empty_string_transcript_is_a_valid_frame` — `""` transcript is a real frame, not `Ignored`; **why**: Deepgram sends empty finals as silence markers and the accumulator relies on seeing them.
- `metadata_and_unknown_types_are_ignored` — Metadata/UtteranceEnd/SpeechStarted/future types → `Ignored`; **why**: forward compatibility with frame types Deepgram adds later.
- `malformed_json_is_ignored_never_panics` — garbage, truncated JSON, and non-objects → `Ignored`; **why**: a parser panic mid-call kills the session.
- `pathologically_nested_json_is_ignored_without_stack_overflow` — 5000-deep nesting → `Ignored`; **why**: without serde_json's recursion limit this is a stack overflow that aborts the process mid-call.
- `parses_the_v1_listen_error_shape` — legacy `description`/`message`/`variant` error keeps both code and text; **why**: the error detail is the only diagnostic the user sees.
- `parses_the_newer_code_description_error_shape` — `code`/`description` formats as `CODE: text`; **why**: both wire shapes exist in the field.
- `error_frame_with_no_detail_still_says_something` — bare `{"type":"Error"}` yields a non-empty detail; **why**: an empty error message is unactionable.
- `accumulator_appends_finals_and_replaces_interims` — finals append to committed text, interims replace each other on top; **why**: the core accumulation contract — misordering here duplicates or drops spoken words.
- `empty_finals_do_not_insert_stray_spaces` — empty finals never add separators; **why**: silence markers must not punctuate the transcript.
- `an_empty_final_clears_a_pending_interim` — an empty final wipes the speculative interim; **why**: showing a stale interim is showing text Deepgram retracted.
- `repeating_an_identical_interim_reports_no_change` — re-sent identical interims report unchanged; **why**: Deepgram re-sends interims; re-emitting identical `stt:partial` events repaints the UI for nothing.
- `non_results_frames_do_not_change_the_transcript` — `Ignored`/`Error` frames leave text untouched; **why**: only Results frames carry transcript.
- `is_empty_and_finalized_text_treat_whitespace_as_nothing` — whitespace-only text is empty and trims in `finalized_text()`; **why**: feeds the `no_speech` decision (§5.7) — whitespace must not count as a question.
- `an_interim_that_never_finalizes_is_still_usable_as_the_question` — a lone interim survives into `finalized_text()`; **why**: if Deepgram closes with only tentative text, answering the tentative question beats answering nothing.

### `core/src/stt/deepgram.rs` (18 tests)

- `the_default_connector_targets_the_documented_url` — pins `DEEPGRAM_URL` params (nova-3, linear16, 16000, mono, interim_results, smart_format) and pins the deliberate ABSENCE of `endpointing`/`no_delay`; **why**: the omissions are design decisions (§6.1) — a well-meaning tweak must argue with this test first.
- `an_empty_api_key_fails_fast_without_a_socket` — empty/whitespace key → `NoSttKey`, and the listener proves no dial was ever attempted; **why**: fail at the gate, not with a confusing handshake error.
- `the_subprotocol_header_carries_the_token` — `Sec-WebSocket-Protocol: token, <key>` reaches the server; **why**: this is how Deepgram auth actually travels — asserted on the wire, not on intent.
- `happy_path_accumulates_and_finalize_returns_the_tail` — interim + final stream out, `finalize()` sends `CloseStream` and still captures a transcript the server releases only after it; **why**: the smart_format hold-back — text released at close must make the returned transcript.
- `audio_is_sent_as_binary_little_endian_i16` — PCM frames arrive as binary LE i16 bytes; **why**: a text frame or wrong endianness is garbage audio Deepgram transcribes as nothing.
- `audio_sent_before_open_is_flushed_in_order_at_open` — frames sent pre-handshake buffer and flush in order; **why**: the user starts talking before the socket opens; those words must not be lost or reordered.
- `pre_open_buffer_drops_the_oldest_beyond_fifteen_seconds` — over the 240,000-sample cap the oldest frames go, order preserved; **why**: losing the newest audio would lose the very speech the user is asking about.
- `keepalives_flow_on_an_idle_open_socket` — two KeepAlive texts arrive within virtual minutes on an idle socket (paused clock); **why**: Deepgram closes idle sockets; keepalives are what hold the line open while nobody speaks.
- `no_keepalive_is_sent_once_close_is_requested` — after `CloseStream`, the virtual window where a leaked keepalive would tick stays silent; **why**: a keepalive after CloseStream reopens Deepgram's silence timer and delays the final transcript.
- `finalize_is_idempotent_and_sends_one_close_stream` — two concurrent finalizes return the same text and the server sees exactly one `CloseStream`; **why**: a double-tapped Stop once raced two closes for the same socket.
- `a_stop_during_the_dial_still_delivers_the_buffered_question` — frames queued before any handshake, then a finalize that lands mid-dial: the server still receives the buffered binaries, then `CloseStream`, and finalize returns the real transcript with no error; **why**: the pre-open buffer holds the user's ENTIRE captured question — the old dial loop abandoned it on stop, turning a slow TLS handshake into a false `no_speech`.
- `a_dial_that_fails_after_a_stop_surfaces_stt_connect` — a dial the server kills while a stop is pending surfaces exactly one `SttConnect` error and finalize still resolves; **why**: the recording is unusable BECAUSE the connect failed — reporting that beats mapping the empty transcript to a misleading "make sure call audio is playing".
- `an_abort_during_the_dial_abandons_it_silently` — `abort()` during a dial that never accepts exits fast (< 2 s) with no error event; **why**: abort is the caller tearing down on purpose (§5.1/§5.10) — only a *stop* now waits the dial out, and conflating the two would slow supersession.
- `close_1008_before_open_is_a_connect_failure_and_finalize_is_instant` — Deepgram's reject-by-close (1008 + DATA reason, no Error frame) surfaces `SttConnect` with code and reason, and finalize on the dead stream is instant; **why**: Deepgram rejects bad keys this way — and finalize must not wait for a tail that cannot arrive.
- `a_mid_stream_error_frame_surfaces_stt_error_with_the_detail` — an Error frame after establishment surfaces `SttError` carrying the code and description; **why**: mid-call failure must reach the UI with its diagnostic intact.
- `abort_reports_no_error` — after `abort()`, the teardown the server observes produces zero error events; **why**: the abort caused the socket death — reporting it would blame the network for our own teardown.
- `an_error_frame_during_the_drain_surfaces_stt_error` — an Error frame answering `CloseStream` (followed by a clean Close) surfaces `SttError` naming the finalizing phase plus the code and description, while `finalize()` still returns the text heard; **why**: error reports are gated on abort only, not on `close_requested` — swallowing this and treating the Close as a clean finalize hands back a truncated transcript, or an empty one that sends the user debugging their call audio for a server-side flush failure.
- `audio_queued_at_stop_is_flushed_before_close_stream` — all 50 frames queued at the instant of finalize reach the wire as binary before `CloseStream` goes out; **why**: frames captured between the pump's last poll and the stop are the final words of the question — the old drain dropped them at the loop break.

## Rust core — session

### `core/src/session/mod.rs` (5 tests)

- `non_streaming_answer_reports_total_not_zero_for_first_token` — `Metrics::finish` with no first-token sample uses total; **why**: "0.0s to first word" would be a lie about the headline metric.
- `streaming_answer_keeps_its_measured_first_token` — a measured first-token value survives; **why**: the real measurement must not be overwritten by the fallback.
- `typed_questions_report_zero_stt_time` — `stt_finalize_ms` can be 0; **why**: typed questions have no STT stage; 0 is the only honest value.
- `every_event_carries_its_session_id_and_wire_name` — all five `SessionEvent` variants expose the right id and event name; **why**: routing and staleness filtering key on exactly these.
- `metrics_serialize_as_camel_case_for_the_frontend` — `sttFinalizeMs`/`firstTokenMs`/`totalMs` on the wire; **why**: the frontend destructures these exact keys.

### `core/src/session/machine.rs` (41 tests, all `#[tokio::test(start_paused = true)]` against scripted STT/LLM fakes)

- `recording_happy_path_streams_partials_deltas_then_done` — partial, partial, delta, delta, done in order with the right payloads; **why**: the baseline every other test degrades from — if the plain flow misorders events, nothing else matters.
- `start_supersedes_live_recording_and_its_late_events_are_dropped` — a new start aborts the old stream, and its late transcript AND the socket death our abort caused are both dropped; **why**: v2 painted a superseded session's transcript over the new one and reported its own teardown as an error.
- `superseded_sessions_done_is_never_emitted` — a superseded session's in-flight answer never emits `done`, silently; **why**: a late done repaints an answer the user already abandoned — the single most confusing failure in v2.
- `double_record_while_first_is_connecting_latest_start_wins` — the second press wins even though the first connect resolves later; the loser is aborted, gets no audio, emits nothing; **why**: the loser once installed itself over the winner and swallowed its audio.
- `stop_unknown_id_is_not_taken` — stop against nothing returns `NotTaken`, zero events; **why**: emitting anything would animate a session that does not exist.
- `stop_while_connecting_is_not_taken_and_session_survives` — stop before connect is `NotTaken` and the session still stops normally afterwards; **why**: accepting the stop would strand the UI in "Finalizing…" with no event ever coming.
- `second_stop_during_finalize_is_not_taken_and_finalize_runs_once` — double-tap Stop: one finalize call; **why**: a second finalize once raced the first for the same socket.
- `stop_after_done_is_not_taken` — stop on a finished session is `NotTaken` with no new events; **why**: "already ended" is a silent NotTaken case — the UI resets on the return value, not a ghost event.
- `stop_after_error_teardown_is_not_taken` — stop after a fatal error is `NotTaken`; **why**: a stop that pretended to take would promise a finalize that can never happen.
- `audio_routes_to_live_session_and_emits_level` — a pushed frame reaches the stream and emits its `AudioLevel`; **why**: `push_audio` is the level meter's only source — a frame without its level leaves the meter dead.
- `audio_after_stop_requested_is_dropped` — post-stop frames reach neither the stream nor the meter; **why**: a late frame races the CloseStream flush and smears this question's tail into the next transcript.
- `audio_for_stale_or_connecting_ids_is_dropped` — frames while connecting or for a wrong id vanish; **why**: audio must never misroute into another session.
- `device_error_while_recording_fails_the_session_once` — a device error mid-recording surfaces exactly one error, tears down the STT stream, and releases the slot (a stop afterwards is `NotTaken`); **why**: an unplugged headset mid-recording must be one honest device error, not a dead level meter followed by a misleading `no_speech` at stop.
- `device_error_after_stop_does_not_kill_the_streaming_answer` — a device death reported after stop (the capture stays physically open through finalize and answer) emits no error and the answer still completes; **why**: once stop was requested the audio stream's job is done — a Bluetooth dropout in that window must not destroy an answer already on its way (the §5.5 late-socket rule, for audio).
- `device_error_for_a_stale_id_is_ignored` — a device error carrying a stale id emits nothing anywhere and the live session still stops normally; **why**: a superseded session's capture dying late must not touch the winner.
- `is_active_tracks_slot_ownership_across_supersession_and_completion` — `is_active` is true only for the current slot owner: flips from loser to winner on supersession and goes false once the answer completes; **why**: the shell installs the audio capture only for the session that owns the slot — a wrong answer here steals the winner's capture or strands the live session without audio (§5.2).
- `mid_recording_stream_death_surfaces_one_stt_error_and_tears_down` — a double-reported socket death yields exactly one `SttError`, teardown, and no LLM call; **why**: a silently truncated transcript answers the wrong question — but the death must surface exactly once.
- `mid_finalize_stream_death_surfaces_stt_error` — a death during the flush window surfaces `SttError`, no done; **why**: swallowing it leaves the UI in "Finalizing…" forever — the flush is where sockets actually die.
- `late_socket_death_after_finalize_does_not_kill_streaming_answer` — an STT error after the transcript finalized leaves the streaming answer intact; **why**: v2 let a late close tear down an answer already painting on screen.
- `error_before_handler_registration_is_buffered_and_delivered` — an error pushed inside `connect()` (before anything listens) is still delivered; **why**: an early death lost here means the UI records into a socket that no longer exists (§5.6).
- `no_error_after_cancel` — an error arriving after `cancel()` produces zero events; **why**: "never after abort" — the abort caused the death; reporting it blames the user's network for our teardown.
- `whitespace_transcript_surfaces_no_speech_verbatim_and_never_calls_llm` — whitespace transcript → one `NoSpeech` with the canonical message, no LLM call; **why**: an LLM call on an empty prompt answers a question nobody asked and bills for it; the message is the one error the user can fix themselves (§5.7).
- `ask_emits_final_partial_then_deltas_then_done_with_zero_stt_ms` — typed ask replays one trimmed final partial, then deltas/done with `stt_finalize_ms == 0`; **why**: typed and spoken questions share one event shape, and billing STT time that never happened lies about the metric (§5.8).
- `ask_validates_input_before_superseding_the_live_session` — empty and over-limit asks are rejected and the live recording survives untouched; **why**: a fat-fingered empty Ask once killed a live recording.
- `ask_accepts_exactly_max_chars` — a question of exactly `MAX_ASK_CHARS` is accepted; **why**: an off-by-one at the limit rejects the longest legal question.
- `ask_over_recording_supersedes_it_silently` — a valid ask aborts the recording with zero error events; **why**: a typed question is a deliberate pivot; the replaced recording must die without a sound.
- `connect_failure_surfaces_its_error_and_releases_the_slot` — a failed connect reports `SttConnect` and the next start proceeds; **why**: v2 left a phantom "connecting" session that blocked Record.
- `connect_timeout_surfaces_stt_connect` — a hung connect fails at `limits::STT_CONNECT`; **why**: a hung connect with no cap is a Record button that never answers and never fails.
- `finalize_timeout_reports_stt_timeout` — a hung finalize fails at `limits::STT_FINALIZE` with `SttTimeout` and aborts the stream; **why**: a hung flush must fail with its own code or the user debugs the wrong stage.
- `first_token_timeout_fires_and_late_deltas_are_suppressed` — no first delta by `limits::LLM_FIRST_TOKEN` → one `LlmFirstTokenTimeout`; a detached straggler delta after it paints nothing; **why**: after the error is on screen, a late delta would show text under an error banner (§5.9).
- `first_delta_disarms_the_first_token_timeout` — a delta just inside the cap disarms it; later deltas past the mark finish normally with correct metrics; **why**: once tokens flow only the total cap applies — v2 truncated healthy answers at the 10 s mark.
- `total_timeout_reports_llm_timeout` — a trickling stream fails at the 60 s total with `LlmTimeout` and no further deltas; **why**: an answer that trickles forever is worse than a clean failure — the total cap runs regardless of progress.
- `max_recording_auto_stops_and_answers_normally` — the 120 s cap auto-stops, prewarms exactly like a user stop, a later manual stop is `NotTaken`, and the answer completes with metrics from the auto-stop instant; **why**: the cap is a stop, not a failure.
- `cancel_is_silent_and_releases_the_slot` — cancel aborts the stream, emits nothing, and the slot is free; **why**: cancel means "pretend this never happened" — any event animates a dismissed session (§5.10).
- `cancel_during_finalize_is_silent` — cancel with a finalize in flight muzzles both its completion AND its timeout; **why**: cancel-during-stop is the razor's edge — a late `stt_timeout` would resurrect a dismissed session.
- `slot_release_after_done_leaves_no_ghost` — a start after done does not "supersede" the finished session; **why**: a lingering slot would make the next start abort a finished stream and mute nobody (§5.11).
- `slot_release_after_error_leaves_no_ghost` — after an error the slot is free and no second error leaks from teardown; **why**: the next Record press must not fight a corpse.
- `metrics_are_measured_from_the_stop_instant` — 3 s of recording contributes nothing; finalize/first-token/total measure from stop; **why**: recording time is the user talking, not us working — counting it makes a long question look like a slow answer.
- `non_streaming_answer_reports_total_as_first_token` — a one-shot answer reports first-token = total, no deltas; **why**: 0 would render "instant" for a provider that streamed nothing.
- `prewarm_fires_on_start_stop_and_ask` — prewarm counts 1/2/3 at start, at stop (asserted BEFORE the finalize resolves), and at ask; **why**: the stop-time warm overlapping the STT flush is what buys the ~1 s stop-to-first-word; dropping any of the three is a cold TLS handshake on the critical path.
- `llm_provider_error_passes_through_and_releases_the_slot` — a provider `LlmHttp` surfaces unwrapped and the next ask works; **why**: provider errors carry their own closed-set codes; re-wrapping breaks the UI's error routing.

## Rust core — store

### `core/src/store/bounds.rs` (14 tests)

- `no_saved_bounds_falls_back_to_defaults_centred` — `None` → default size, no position; **why**: first run must open centred at the default size.
- `a_normal_on_screen_window_is_restored_exactly` — a valid saved geometry round-trips; **why**: the common case must be untouched.
- `size_is_clamped_up_to_the_window_minimum` — sub-minimum sizes clamp to `MIN_WIDTH`/`MIN_HEIGHT`; **why**: a saved tiny window would be unusable.
- `fractional_values_are_rounded_to_integers` — fractional pixels round; **why**: DPI-scaled saves carry fractions the window system can't take.
- `negative_coordinates_are_valid_for_a_monitor_left_of_primary` — negative x restores on a left-arranged monitor; **why**: the classic false positive — treating x < 0 as corrupt strands left-of-primary users.
- `a_window_on_a_monitor_that_no_longer_exists_loses_its_position` — off-screen position drops, size survives; **why**: the unplug-monitor scenario — restoring there makes the app unreachable.
- `visibility_needs_forty_pixels_on_both_axes` — exactly 40 px overlap keeps the position, 39 drops it, both axes; **why**: pins the usable-overlap threshold at its exact boundary.
- `both_axes_must_pass_on_the_same_display` — L-shaped arrangements can't satisfy one axis per monitor; **why**: two monitors each passing one axis would restore a window visible on neither.
- `visibility_is_judged_at_the_clamped_size` — a tiny saved size is clamped BEFORE the overlap test; **why**: judging at the stored size needlessly recentres a window that is perfectly usable once clamped.
- `corrupt_numbers_drop_the_whole_geometry_not_one_field` — NaN/∞/1e300 anywhere → full fallback; **why**: a defaulted height welded onto a saved width is a shape the user never chose.
- `zero_or_negative_size_falls_back` — 0 or negative dimensions → fallback; **why**: degenerate sizes are corruption, not preferences.
- `an_empty_display_list_never_trusts_a_position` — no monitors enumerated → no position, size kept; **why**: if visibility cannot be proven, guessing wrong makes the app unreachable.
- `a_window_spanning_two_monitors_is_kept` — a straddling window restores; **why**: spanning is a legitimate arrangement, not corruption.
- `a_taskbar_only_overlap_is_not_visible` — overlap only with the taskbar strip drops the position; **why**: work area excludes the taskbar — a window there has no usable pixels.

### `core/src/store/settings.rs` (27 tests)

- `first_run_loads_defaults` — empty dir → `Settings::default()`, default hotkey, always-on-top true; **why**: first run must be fully defined, not partially null.
- `a_fully_valid_file_loads_every_field` — a complete file loads all ten fields; **why**: baseline the corruption tests degrade from.
- `patch_round_trips_through_disk` — a patch plus saved bounds reload identically from a fresh store; **why**: that is what persistence means here.
- `unparseable_json_loads_as_defaults` — syntactically broken file → defaults; **why**: a broken file must not crash the app at startup.
- `non_object_json_loads_as_defaults` — arrays/strings/numbers/bools/null at top level → defaults; **why**: same, for the shape being wrong rather than the syntax.
- `each_corrupt_field_falls_back_alone` — for 8 fields, corrupting one defaults exactly that field (whole-struct equality); **why**: the core promise of per-field validation — one bad value must not cost the rest.
- `corrupt_answer_style_never_costs_the_resume_or_the_keys` — a bad enum string keeps resume and all three keys; **why**: the named disaster from the spec — one bad enum must not read as "corrupt file" and wipe everything.
- `over_length_resume_is_truncated_on_load_and_on_patch` — the `MAX_PROFILE_CHARS` cap applies on both paths; **why**: an unbounded resume bloats every prompt and the settings file.
- `truncation_counts_characters_not_bytes` — multibyte text truncates on a char boundary and round-trips; **why**: a byte-boundary cut poisons the file for every later load.
- `resume_is_stored_verbatim_never_trimmed` — leading/trailing whitespace survives the round trip; **why**: profile formatting belongs to the user.
- `empty_hotkey_means_disabled_and_never_reverts_to_default` — `""` persists as `""` across reload; **why**: an absent-vs-empty confusion would resurrect the shortcut the user turned off.
- `whitespace_only_hotkey_becomes_empty` — whitespace hotkey normalizes to disabled; **why**: an invisible "shortcut" is disabled in intent.
- `hotkey_is_trimmed_and_capped` — padding is trimmed and length capped at `MAX_HOTKEY_CHARS`; **why**: pasted garbage must not become an unregisterable accelerator.
- `missing_hotkey_field_gets_the_default` — an absent field yields `DEFAULT_HOTKEY`; **why**: missing means "never configured", not "disabled".
- `omitted_key_field_leaves_the_stored_key_untouched` — a patch without the key field keeps the stored key; **why**: the typical patch edits the resume, not the key — omission is not deletion.
- `empty_key_patch_clears_the_stored_key` — `""` in a patch deletes the key everywhere (view, memory, disk); **why**: the explicit clear path must actually clear.
- `key_patch_values_are_trimmed` — pasted keys lose clipboard whitespace; **why**: an untrimmed key fails at the provider with a confusing mid-call auth error.
- `view_never_exposes_key_material` — the serialized `SettingsView` carries `has*Key` booleans and zero key bytes; **why**: the view crosses the IPC boundary — key material must never reach the webview.
- `keys_are_never_written_to_disk_in_plaintext` — the raw file contains no plaintext key and only `enc:`/`plain:` encodings; **why**: the settings file is greppable by anything on the machine.
- `plain_prefixed_key_on_disk_is_read_back` — a `plain:`-stored key decodes; **why**: decode dispatches on the stored prefix, not on whether DPAPI is available right now.
- `undecryptable_key_reads_as_unset_without_losing_other_fields` — a foreign-machine `enc:` blob reads as no key; resume and other keys survive; **why**: DPAPI is per-user — a copied file must not surface ciphertext-as-key or wipe the rest.
- `successful_save_leaves_no_tmp_file` — after save, only the real file exists; **why**: the temp-then-rename dance must clean up after itself.
- `a_stale_tmp_file_does_not_corrupt_a_load_or_the_next_save` — garbage tmp beside a good file: load is clean, next save overwrites the tmp; **why**: the crashed-mid-write aftermath must be survivable.
- `window_bounds_round_trip` — saved bounds reload from memory and disk; **why**: window position persistence is a first-class field.
- `corrupt_bounds_are_dropped_without_losing_other_fields` — four malformed bounds shapes drop only the bounds; **why**: per-field validation again, for the nested object.
- `save_window_bounds_swallows_write_failures` — a forced rename failure is silent and the cache does not claim unsaved bounds; **why**: this runs during shutdown — the only acceptable outcomes are "saved" or "silently didn't", and memory must match disk.
- `failed_patch_reports_an_error_and_leaves_memory_matching_disk` — a failed write returns `Internal` and the cache is not updated; **why**: the UI must not show settings that will silently vanish on restart.

### `core/src/store/secrets.rs` (9 tests; `on_windows_protect_uses_dpapi_not_the_fallback` is `#[cfg(windows)]`)

- `protect_output_is_always_prefixed` — output starts with `enc:` or `plain:`; **why**: the prefix is the decode dispatch — an unprefixed value would read as unset forever.
- `protect_unprotect_round_trips` — several secrets (incl. unicode) round-trip; **why**: the storage transform must be lossless.
- `stored_form_never_contains_the_plaintext` — the stored string does not contain the raw key; **why**: keys must not be greppable out of the settings file.
- `on_windows_protect_uses_dpapi_not_the_fallback` — on Windows, `protect` emits `enc:`; **why**: if this fails, keys land on disk merely encoded on the one platform where real encryption exists.
- `plain_prefix_decodes_by_stored_prefix_not_keystore_state` — a `plain:` value decodes even where `protect` would choose `enc:`; **why**: files written on a DPAPI-less machine (or pre-OS-repair) must stay readable.
- `garbage_and_unknown_prefixes_read_as_unset` — raw pasted keys, empty strings, unknown/case-wrong prefixes → `None`; **why**: handing a raw pasted key back "works" until the prefix logic changes — uniform rejection is safer.
- `invalid_base64_reads_as_unset` — bad base64 under either prefix → `None`; **why**: corrupt encodings must fail closed.
- `undecryptable_enc_blob_reads_as_unset` — valid base64 that is not a DPAPI blob → `None`; **why**: the settings-file-copied-from-another-machine case — never surface ciphertext.
- `plain_value_with_invalid_utf8_reads_as_unset` — non-UTF-8 plaintext bytes → `None`; **why**: a key that isn't a string is not a key.

## Rust core — audio

### `core/src/audio/mod.rs` (4 tests)

- `rms_of_silence_is_zero` — zeros and empty input → 0.0; **why**: a silent call must show a dead meter, not noise.
- `rms_of_full_scale_square_wave_is_one` — a full-scale square wave ≈ 1.0; **why**: pins the normalization so the meter tops out at exactly full scale.
- `rms_is_bounded_even_at_the_negative_rail` — all-`i16::MIN` stays within 0.0..=1.0; **why**: `i16::MIN` has larger magnitude than `MAX` — without the clamp the meter renders past 100% and overflows its track.
- `rms_rises_with_amplitude` — louder input gives a larger value; **why**: monotonicity is the property the meter actually communicates.

### `core/src/audio/resample.rs` (12 tests)

- `stereo_48k_yields_one_third_the_frames_and_averages_channels` — constant L/R average exactly, count ≈ 1/3; **why**: any non-average sample proves a dropped channel or broken downmix.
- `non_integer_ratio_44100_does_not_drift_across_chunk_boundaries` — 10 s chunked equals one-shot exactly; **why**: any per-buffer rounding shows up as drift that desyncs the transcript over a call.
- `many_tiny_buffers_match_one_big_buffer` — 100 small chunks equal one push; **why**: WASAPI delivers arbitrary buffer sizes — output must not depend on them.
- `one_khz_sine_survives_with_roughly_the_right_amplitude` — peak within 5% after 48k→16k; **why**: resampling must not attenuate speech-band content.
- `opposite_stereo_channels_average_to_silence` — L=+1/R=-1 cancels to zero; **why**: +1.0 out would mean channel 0 was taken instead of downmixed.
- `surround_downmix_uses_the_speech_channels_not_all_of_them` — 5.1 center-channel dialog survives at exactly 1/3 (FL+FR+FC average) and pure LFE/surround content mixes to zero; **why**: an equal average over all six channels divides the voice by 6 — quiet enough to cost transcription accuracy — while the rear/LFE channels carry rumble Deepgram should never hear.
- `i16_conversion_is_centred_and_round_trips` — 0→0.0, MIN→-1.0, symmetric conversion; **why**: an off-centre conversion injects a DC offset into everything downstream.
- `u16_conversion_is_centred` — 32768→0.0 exactly; **why**: 32768 is unsigned silence — a wrong midpoint gives the whole stream a DC offset.
- `samples_past_full_scale_clamp_instead_of_wrapping` — ±1.5 clamps to `i16::MAX`/`MIN` on both fast and generic paths; **why**: overshoot that wraps becomes loud garbage audio.
- `sixteen_khz_mono_passthrough_is_bit_exact` — 16 kHz mono in = identical i16 out; **why**: the passthrough fast path must not resample at all.
- `upsampling_from_8k_roughly_doubles_and_preserves_level` — constant input stays constant at ~2x count; **why**: interpolation between equal endpoints must not ripple.
- `empty_input_is_a_no_op` — empty pushes produce nothing and leave interleaved-stream output unchanged; **why**: empty device callbacks happen and must not perturb resampler state.

### `core/src/audio/capture.rs` (13 tests; no test opens a real device — the device-watch and drop-gate decisions, framing logic, forwarder thread + `NullCapture` only)

- `device_watch_reports_only_a_real_move` — `default_device_moved` is true only when a CURRENT default exists and differs; the same name, or no default at all, reports nothing; **why**: the watcher's one job is telling "the default moved" apart from "the default died" — the stream's own error callback owns the death report, and double-reporting reads as a crash loop.
- `emits_only_complete_frames_and_retains_the_remainder` — a partial tail is held and completed by the next push; **why**: Deepgram frames are fixed-size; dropped remainders lose audio.
- `emits_exactly_n_frames_for_n_times_frame_samples` — ragged chunk sizes still yield exactly N frames of `FRAME_SAMPLES`; **why**: frame boundaries never align with device buffers in practice.
- `never_emits_a_short_frame` — under-full input emits nothing; **why**: a short frame would corrupt the fixed-size stream contract.
- `frame_content_is_passed_through_in_order` — concatenated frames equal the input exactly; **why**: accumulation must not reorder or duplicate samples.
- `null_capture_starts_emits_nothing_and_stop_is_idempotent` — `NullCapture` starts, double-stop and drop are safe, zero frames/errors; **why**: the no-device fallback must be inert and safely stoppable.
- `a_brief_hiccup_stays_silent` — up to `DROP_REPORT_FRAMES - 1` lost frames report nothing; **why**: a 128 ms gap does not change what the transcript asks, and killing a live session over it costs the user their question — strictly worse than the gap.
- `crossing_the_threshold_reports_exactly_once` — the report fires at the threshold and never again; **why**: a consumer that fell behind keeps dropping frames — the user needs one honest error, not a toast storm that reads as a crash loop.
- `scattered_single_frame_losses_accumulate_into_one_report` — thirty scattered 1-frame takes still produce exactly one report at the threshold; **why**: the counter is drained frame by frame, so a second of loss almost never arrives as one big number — if small takes did not add up, an arbitrarily long hole could stay invisible forever.
- `a_report_names_the_size_of_the_gap_and_what_to_do` — the message carries the gap length in seconds and says to press Record again; **why**: "audio was lost" is not actionable; the number tells the user whether their question survived.
- `the_forwarder_reports_a_big_loss_once_and_still_delivers_its_frames` — a pre-loaded counter over the threshold yields one `on_error` and every queued frame still reaches the sink; **why**: the frames that DID make it are still audio the transcript needs.
- `the_forwarder_stays_silent_under_the_threshold` — sub-threshold pre-loaded losses deliver frames and no error; **why**: the gate's silence contract holds through the real forwarder loop, not just the pure `DropGate`.
- `a_full_queue_counts_every_dropped_frame` — three frames into a capacity-1 channel land one and count exactly two; **why**: the `try_send`-failure increment is the single line that turns a silent transcript hole into a countable one (§5.5) — a refactor of the hand-off that forgot the counter would pass every other test while losing audio invisibly again.

## Tauri shell (compiled with the shell crate, not `app-core`)

### `src-tauri/src/commands.rs` (10 tests)

- `ok_envelope_serializes_to_the_exact_wire_shape` — `{ ok: true, value }`; **why**: the frontend destructures these exact keys.
- `ok_envelope_with_unit_value_still_carries_the_value_key` — unit values serialize as `value: null`; **why**: `res.ok` / `res.value` must behave uniformly for stop/cancel.
- `err_envelope_serializes_code_and_message` — `{ ok: false, error: { code, message } }`; **why**: the error branch's exact shape is the UI's routing input.
- `err_envelope_never_carries_a_value_key_and_ok_never_an_error_key` — the two branches are disjoint; **why**: a stray key would make `in` checks lie.
- `stop_not_taken_is_an_error_envelope` — NotTaken maps to an `internal` error envelope; **why**: the UI unsticks "Finalizing…" from the return value, not an event.
- `ask_text_is_trimmed` — surrounding whitespace is stripped; **why**: the trimmed text is what the LLM should answer.
- `empty_or_whitespace_ask_is_rejected` — blank asks fail validation; **why**: an empty prompt must be stopped at the gate.
- `ask_at_the_limit_passes_and_one_over_fails` — exact `MAX_ASK_CHARS` boundary; **why**: an off-by-one rejects the longest legal question.
- `ask_limit_counts_characters_not_bytes` — 8000 four-byte chars pass; **why**: a byte-counted check wrongly rejects multibyte questions well under the limit.
- `present_treats_blank_keys_as_missing` — `None`/`""`/whitespace read as no key, values are trimmed; **why**: a key of spaces passes `is_some()` and then fails at the provider with a confusing auth error.

### `src-tauri/src/events.rs` (6 tests)

- `stt_partial_maps_to_its_name_and_bare_payload` — `stt:partial` + camelCase payload; **why**: the wire name/shape is the frontend contract.
- `llm_delta_maps_to_its_name_and_bare_payload` — `llm:delta` + `{ sessionId, delta }`; **why**: same contract per event.
- `llm_done_carries_camel_case_metrics` — `llm:done` with nested camelCase metrics; **why**: metrics keys are destructured verbatim by the UI.
- `session_error_carries_code_and_message` — `session:error` with the error object; **why**: error routing depends on the code reaching the UI intact.
- `audio_level_carries_rms` — `audio:level` + `{ sessionId, rms }`; **why**: the level meter's only input.
- `no_payload_ever_leaks_the_kind_tag` — the serde `kind` tag is stripped from every variant and `sessionId` is always present; **why**: the event NAME already carries the kind — leaking the tag silently changes every payload shape the UI destructures.

### `src-tauri/src/window.rs` (14 tests)

- `latest_touch_owns_the_save_and_stale_tokens_get_nothing` — a superseded debounce token gets `None`, the newest gets the newest bounds, one-shot; **why**: saving the stale geometry would persist a position the user already moved past.
- `a_stale_wakeup_does_not_consume_the_pending_bounds` — a stale claim leaves the value for the rightful owner; **why**: a consuming stale wakeup would drop the save entirely.
- `close_flush_takes_pending_regardless_of_generation` — `take_pending` grabs the latest, once; **why**: the close-time flush must not be defeated by the debounce still counting down.
- `first_reload_is_allowed_immediately` — the first crash-reload passes; **why**: recovery must not wait out a gap that hasn't started.
- `reloads_inside_the_gap_are_denied` — reloads within the window are refused; **why**: a crash loop must not become a reload storm.
- `a_reload_after_the_gap_is_allowed_again` — the gap boundary reopens; **why**: recovery must eventually retry.
- `denied_attempts_do_not_push_the_window` — a storm of denials doesn't starve the retry at gap-end; **why**: sliding the window on denials would postpone recovery forever.
- `https_urls_are_allowed` — https (case-insensitive scheme) passes the external-open guard; **why**: RFC 3986 schemes are case-insensitive; a case-sensitive check breaks legitimate links.
- `non_https_schemes_are_rejected` — http/file/javascript/mailto/empty are refused; **why**: the opener must never launch a non-web scheme from model-influenced content.
- `urls_that_could_split_an_argument_are_rejected` — spaces, quotes, newlines, control chars are refused; **why**: characters that split a shell argument turn "open URL" into "run something else".
- `multibyte_prefixes_do_not_panic_the_guard` — non-ASCII near byte 8 doesn't panic; **why**: a naive `url[..8]` slice panics on a char boundary.
- `the_production_csp_keeps_styles_locked_down` — parses `tauri.conf.json` at compile time and pins `style-src 'self'` with no `unsafe-inline`; **why**: index.html's dev meta keeps `unsafe-inline` for Vite, and the effective policy is the intersection — deleting or "simplifying" the conf entry would silently reopen inline styles in the shipped app, and nothing else pins that file.
- `app_origins_are_internal` — tauri scheme, `tauri.localhost`, and the dev-server host:port classify as internal; **why**: internal navigations stay in the webview; misclassifying them breaks the app's own pages.
- `everything_else_is_external` — example.com, localhost on a non-dev port, plain localhost, and file are external; **why**: the dev-server host without the dev port is NOT a free pass — a page on another local service is still another page.

### `src-tauri/src/logging.rs` (5 tests)

- `epoch_formats_as_the_epoch` — 0 → `1970-01-01T00:00:00Z`; **why**: fixed point for the hand-rolled formatter (no chrono).
- `last_second_of_a_day_stays_in_that_day` — 86,399 stays on Jan 1; **why**: a day-boundary off-by-one misdates every crash near midnight.
- `known_timestamp_round_trips` — the billennium formats correctly; **why**: a well-known fixed point exercising the whole civil_from_days algorithm.
- `leap_day_is_computed_correctly` — 2000-02-29 exists; **why**: 2000 is the divisible-by-400 edge a naive century rule skips.
- `pre_epoch_times_do_not_wrap` — -1 → `1969-12-31T23:59:59Z`; **why**: a clock set before 1970 must not produce garbage via unsigned wrap.

---

## Frontend — markdown

### `src/markdown/Markdown.test.tsx` (53 tests)

**headings (5)**

- `demotes every model level by two, capped at h6` — `#`→`h3` … `######`→`h6`; **why**: the page owns h1/h2 — a model heading must never outrank app chrome.
- `requires a space after the hashes so #hashtags stay prose` — `#nospace` is a paragraph; **why**: model text contains hashtags.
- `treats seven hashes as prose, not a heading` — `#######` is a paragraph; **why**: CommonMark boundary; over-eager matching mangles prose.
- `strips a closing hash run` — `## title ##` → `h4` "title"; **why**: trailing hashes are decoration, not content.
- `parses inline markdown inside a heading` — bold inside a heading renders; **why**: headings carry inline content too.

**lists (18)**

- `preserves the start number of a numbered list` — `7.` → `<ol start="7">`; **why**: answers regularly continue numbering; resetting to 1 misnumbers steps.
- `emits no start attribute when the list starts at 1` — no redundant `start`; **why**: keeps the emitted-attribute audit surface minimal.
- `supports the paren marker form` — `3)` works; **why**: models emit both marker styles.
- `accepts -, * and + bullets` — all three bullet chars; **why**: model output varies by bullet.
- `renders tight items without paragraph wrappers` — no `<p>` in tight lists; **why**: the DOM shape is the styling contract.
- `renders loose items with paragraph wrappers` — blank-separated items get `<p>`; **why**: tight/loose distinction per CommonMark.
- `does NOT end a list at a blank line when another item follows` — one `<ul>`, three `<li>`; **why**: streaming models emit blank-separated items — the renderer must not shatter them into single-item lists.
- `ends the list at a blank line followed by non-item text` — list closes before prose; **why**: the closing rule's other half.
- `lazily continues a wrapped item onto the next line` — lazy continuation joins into the item; **why**: soft-wrapped items are common in model output.
- `keeps an indented second paragraph inside its item and goes loose` — indented continuation stays in the `<li>`; **why**: multi-paragraph items must not escape the list.
- `starts a new list when the family changes` — `ol` then `ul`; **why**: changing marker family is a new list, not a mixed one.
- `lets a list interrupt a paragraph without a blank line` — "Steps:" + items; **why**: models constantly glue a lead-in straight onto item one.
- `` lets a `1.` item interrupt a paragraph without a blank line `` — "Steps:" + `1.`/`2.` splits into a paragraph and an `<ol>`; **why**: CommonMark allows exactly `1.`/`1)` (plus bullets) to interrupt — numbered steps get glued onto the lead-in just as often as bullets.
- `keeps "1997." at a wrapped line start inside the paragraph` — a year+dot (and a `2019)` variant) at a soft-wrapped line start stays prose; **why**: the case CommonMark's interruption restriction exists for — a naive "any N. interrupts" turned a mid-sentence figure into an `<ol start="1997">` mid-answer.
- `still starts an <ol> at any number after a blank line` — `7. item` after a blank line still opens `<ol start="7">`; **why**: only the mid-paragraph interruption path is restricted to `1.` — §10's start-number requirement at block starts must survive the fix.
- `does not let an empty marker interrupt a paragraph` — a lone `-` or `1.` at a line break stays prose; **why**: CommonMark lets only non-empty items interrupt — an empty surprise `<li>` mid-answer helps nobody.
- `lets an indented fence after a blank line interrupt the list` — `- Example:` + blank line + indented ```` ```python ```` yields one real `pre>code` block, the item keeps only its own text, and trailing prose stays outside any fence; **why**: the audit regression — the pending-blank branch swallowed the indented opener as item text, so the CLOSING ``` opened a brand-new top-level fence that ate the entire rest of the answer.
- `does not mistake decimals or flags for list markers` — `3.14` and `-rf` stay prose; **why**: false-positive list detection shreds ordinary sentences.

**fenced code blocks (6)**

- `drops the info string instead of turning model text into a class` — no attributes on `<code>`; **why**: the info string is model-controlled — it must never become an attribute.
- `supports tilde fences` — `~~~` works; **why**: both fence styles occur.
- `keeps an unterminated fence open to EOF` — open fence renders as a code block; **why**: mid-stream, the closer simply hasn't arrived yet.
- `requires the closer to be at least as long as the opener` — a shorter run is content; **why**: nested fence examples must survive.
- `leaves markdown inside a fence completely inert` — headings/bold/lists inside a fence stay text; **why**: code content must never be parsed as markup.
- `renders an empty fence as an empty block` — ` ```\n``` ` → empty `pre>code`; **why**: degenerate input must not crash or vanish.

**inline code (7)**

- `renders a simple span` — backticks → `<code>`; **why**: baseline.
- `lets a double-backtick span contain a single backtick` — `` ``has ` tick`` `` works; **why**: the standard escape for literal backticks.
- `requires the closer run to match the opener length exactly` — mismatched runs stay literal; **why**: a sloppy match swallows text into a phantom code span.
- `strips one space of padding from each end` — `` ` x ` `` → `x`; **why**: CommonMark's padding rule for spans starting/ending with backticks.
- `does not strip an all-space span to nothing` — `` ` ` `` keeps its space; **why**: the padding rule's exception.
- `leaves an unclosed backtick literal` — `` `unclosed `` stays text; **why**: mid-stream half-arrived delimiters must not eat the tail.
- `protects emphasis characters inside a span` — `*` inside code stays literal; **why**: code content is inert.

**emphasis (7)**

- `renders bold, italic, and both` — `**`/`*`/`***` and `__`/`_` forms; **why**: baseline emphasis grammar.
- `never italicizes snake_case identifiers` — `snake_case`, `SCREAMING_SNAKE_CASE` stay flat; **why**: THE bug that bites markdown renderers on model output — naive `_` handling shreds identifiers.
- `allows intraword asterisk emphasis but not intraword underscore` — `in*tra*word` yes, `in_tra_word` no; **why**: the CommonMark asymmetry that protects identifiers.
- `supports nesting in both directions` — bold-in-em and em-in-bold; **why**: nested emphasis is everywhere in answers.
- `applies the multiple-of-3 rule so runs do not cross-pair` — `*foo**bar*` pairs correctly; **why**: cross-paired delimiters corrupt everything after them.
- `leaves a partly-consumed opener as literal text` — `**foo*` → `*<em>foo</em>`; **why**: leftovers must degrade to visible text, not vanish.
- `leaves space-surrounded and unclosed delimiters literal` — `a * b * c` and `*not closed` stay text; **why**: multiplication signs and stray stars are prose.

**backslash escapes (4)**

- `disarms emphasis and code delimiters` — `\*`, `` \` ``, `\_` render literally; **why**: the model's own escape mechanism must work.
- `keeps the backslash before non-punctuation` — `C:\Users\Owner` intact; **why**: Windows paths are constant in this app's answers.
- `escapes a backslash itself` — `\\` → `\`; **why**: completes the escape grammar.
- `a leading escaped hash is not a heading` — `\#` is prose; **why**: escapes must defeat block-level detection too.

**thematic breaks (2)**

- `accepts the three characters and spaced forms` — `---`/`***`/`___` and spaced variants → `<hr>`, never `<li>`; **why**: `***` must not be parsed as a list or emphasis.
- `rejects two-character runs` — `--` stays prose; **why**: em-dash-ish typography is not a rule.

**paragraphs and links (4)**

- `keeps soft-wrapped lines in one paragraph and splits on blanks` — newline joins, blank line splits; **why**: paragraph boundaries are the core block grammar.
- `renders [text](url) as literal visible text with no anchor` — no `<a>` ever; **why**: links are deliberately unsupported — with no href there is no URL scheme to sanitize.
- `renders the empty string as an empty container` — no children, no text; **why**: the pre-stream state.
- `normalizes CRLF input` — `\r\n` behaves like `\n`; **why**: Windows line endings arrive from every direction here.

### `src/markdown/streaming.test.tsx` (5 `it(...)` → 21 runtime cases)

- `every prefix renders and the stream converges: ${name}` — parameterized over a 17-document corpus (kitchenSink, nestedEmphasis, numberedFromSeven, looseList, lazyContinuation, unterminatedFence, fenceContainingMarkdown, headingsEveryLevel, snakeCaseIdentifiers, inlineCodeDoubleBacktick, escapes, thematicBreaks, mixedParagraphs, plus four audit-regression docs: fenceInListItem walks every prefix of the indented-fence-in-list state that once inverted fence parity, yearAtLineStart walks the wrapped "1997." line that once became an `<ol start="1997">`, and emphasisStorm/deadClosers walk the two delimiter shapes whose pairing the openers-bottom rewrite could most plausibly change): every cut point of each document renders without throwing and the fully-streamed DOM is byte-identical to a one-shot render; **why**: §10 requirements (a)/(b) — fence/emphasis/list bugs live at the exact byte where a delimiter is half-arrived.
- `completed blocks keep their DOM nodes as the stream grows` — heading/paragraph/text-node identity survives appended blocks; **why**: broken identities destroy the user's text selection on every streamed token and make the answer flicker.
- `keeps earlier list items stable while the last item is still streaming` — the first `<li>` keeps its node while the last grows; **why**: same stability rule inside a list.
- `an unchanged source is a no-op` — `parseMarkdown` returns the referentially same result and DOM nodes persist; **why**: referential equality proves the parse was skipped, not repeated per render.
- `parses delimiter-dense answers well inside a frame budget` — the two worst delimiter shapes at max-answer size parse in far under a deliberately loose 1000 ms bound (the quadratic version took ~1.6 s); **why**: the parser re-runs over the whole answer every streamed frame, so a return to quadratic emphasis pairing would burn the stop-to-first-word window — this asserts a complexity class, not a benchmark.

### `src/markdown/xss.test.tsx` (5 `it(...)` → 12 runtime cases)

- `it(name)` over 8 payloads (script tag, img onerror, fence breakout, javascript: link, attribute injection via quotes, iframe, html comment, comment plus handler element) — no script/iframe/img/a elements exist, the whole tree passes the tag/attribute audit, and the payload is VISIBLE as literal text; **why**: model output is untrusted — the guarantee is structural (every string becomes a text node), and swallowing the payload would hide what the model actually said.
- `a fence-breakout payload stays inside the code element` — `</pre><script>` inside a fence is text inside `pre>code`; **why**: the classic close-the-pre-from-inside attack must stay inert content.
- `no comment nodes are ever created` — `<!-- hidden -->` produces zero comment nodes; **why**: a real comment node would be invisible — the user must see the bytes.
- `emits zero attributes on ordinary rich output` — the audit holds for benign input too; **why**: otherwise the "safe set" rots into an allowlist of accidents.
- `event-handler attributes are absent from every element` — no `on*` attribute anywhere; **why**: redundant with the audit, but it is the assertion a security review greps for, so it exists by name.

## Frontend — state

### `src/state/reducer.test.ts` (46 tests; ignored actions asserted by object identity)

**record/start (4)**

- `idle -> starting pushes a live entry, jumps the view, resets per-attempt fields` — error/rms/elapsed/cap all reset, view jumps to the new entry; **why**: stale per-attempt state bleeding into a new recording misreports it.
- `answering -> starting supersedes: a captured streaming entry is retired, not lost` — the half-streamed answer stays in history; **why**: superseding must not discard work the user can still read.
- `answering -> starting discards a previous live entry that captured nothing` — an empty live entry is dropped; **why**: empty husks would pollute history and the n/m counter.
- `is refused in starting, recording, and finalizing` — same state object returned; **why**: a mid-pipeline start would orphan the in-flight session.

**record/started — adoption (3)**

- `starting -> recording adopts the id onto state and the live entry` — id lands on both; **why**: event routing keys on the adopted id.
- `ignores a resolution for a superseded attempt key` — stale key → same state; **why**: the losing start's resolution must not install itself.
- `ignores adoption when no longer starting (user already aborted)` — same state; **why**: adoption after abort would resurrect a dismissed attempt.

**record/startFailed (3)**

- `discards the empty live entry, returns to idle, surfaces the error` — cleanup plus error; **why**: a failed start must fully unwind.
- `never surfaces aborted` — aborted code → no error shown; **why**: the user cancelled; a banner would blame them for it.
- `ignores a stale key` — same state; **why**: a superseded attempt's failure is not this attempt's failure.

**record/abortStarting (2)**

- `silently tears down: idle, no error, empty attempt discarded` — clean silent unwind; **why**: abort-while-starting is a user cancel, not an error.
- `is a no-op outside starting` — same state; **why**: the action only means something in one phase.

**record/stop + stopRejected (6)**

- `recording -> finalizing` — transition plus rms reset; **why**: the meter must drop the instant recording ends.
- `stop is ignored outside recording` — same state in all four other phases; **why**: stray stops must not corrupt the machine.
- `stopRejected unsticks finalizing: retires a captured transcript and surfaces the error` — back to idle, transcript kept; **why**: NotTaken must not leave the UI hanging in "Finalizing…".
- `stopRejected discards an attempt that captured nothing` — empty attempt dropped; **why**: no husk entries.
- `stopRejected with aborted is silent` — no error surfaced; **why**: aborted is never rendered.
- `stopRejected ignores a stale key or wrong phase` — same state; **why**: only the live attempt's stop result may act.

**ask lifecycle (5)**

- `idle -> answering pushes the question and jumps the view` — entry with the typed question; **why**: typed questions share the history pipeline.
- `ask over a streaming answer supersedes it and retires the partial` — old partial kept, new entry live; **why**: same retire-don't-lose rule as record.
- `ask/start is refused mid-recording pipeline` — same state in starting/recording/finalizing; **why**: an ask must not kill an in-flight recording at the reducer level.
- `ask/accepted adopts the id; stale key ignored` — id lands, stale resolution is identity; **why**: same adoption discipline as record.
- `ask/failed retires the entry (it holds the typed question) and surfaces the error` — question preserved for retry; **why**: losing the typed question on failure forces the user to retype it.

**tick + recording cap (4)**

- `accumulates elapsed only while recording` — ticks add up, idle tick is identity; **why**: the timer must freeze outside recording.
- `crossing MAX_RECORDING_SECONDS caps locally: finalizing, flag latched, session KEPT` — the cap tick clamps elapsed, latches the flag, flips recording → finalizing, drops rms, and keeps `activeId`/`liveKey`; **why**: the core capped first (its timer is armed before ours, §3) and will stream this same session's answer — nulling the ids here is what used to drop it.
- `the cap transition fires exactly once — a follow-up tick is a no-op` — a second tick on the capped state returns the same object; **why**: ticks keep arriving after the cap and must not re-run the transition.
- `the flag resets on the next start, not before` — survives stop/done, clears on restart; **why**: the explanation must stay visible through the answer it explains.

**core auto-stop recovery — answer events while still "recording" (4)**

- `llm:delta for the current session transitions recording -> answering and appends` — deltas arriving while the ui still says recording flip to answering, drop rms, and append (the cap flag stays tick-driven); **why**: the core auto-stops at 120 s without emitting an event, so its answer can arrive while the frontend clock still says recording — dropping those events lost every capped recording's answer.
- `llm:done for the current session completes the entry straight from recording` — done from recording finalizes the entry and returns to idle, no error; **why**: the same race when the entire answer lands inside one throttled-timer gap.
- `still drops stale ids while recording` — wrong-id delta/done are identity; **why**: the recovery path must not weaken the §9 staleness rule.
- `still drops pre-adoption events (starting, or ask not yet accepted)` — delta/done before any id was adopted are identity; **why**: recovery applies only to the adopted session — never to events taken on trust.

**stale events (2)**

- `drops events whose sessionId is not the tracked session` — five event types with a wrong id are identity; **why**: §9 hard rule — a superseded session must not write into the new one.
- `drops events arriving before the id was adopted (activeId still null)` — pre-adoption events are identity; **why**: no buffering — events racing the command promise must not be applied on trust.

**event application (7)**

- `audio:level drives rms while recording` — rms lands; **why**: the meter's data path.
- `stt:partial replaces (not appends) the live question in recording and finalizing` — later partial replaces; **why**: appending interims duplicates the phrase as Deepgram revises.
- `llm:delta moves finalizing -> answering and appends` — first delta flips the phase, deltas append; **why**: the first token is what ends "Finalizing…".
- `llm:done finalizes the entry and returns to idle` — transcript/answer/metrics land, ids clear; **why**: the terminal transition of the happy path.
- `session:error mid-stream keeps the partial answer, surfaces the error, returns to idle` — partial preserved; **why**: text the user already read must not vanish under the banner.
- `session:error aborted is silent; an empty attempt is discarded` — no error, husk dropped; **why**: aborted is invisible by contract.
- `session:error aborted retires an attempt that captured a question` — captured words kept; **why**: silent teardown still must not lose captured work.

**history limit (3)**

- `pushing onto a full history trims the oldest and jumps the view to the new entry` — FIFO at `HISTORY_LIMIT`; **why**: unbounded history leaks memory in a long call.
- `the in-flight entry survives a trim even from a pathological oversized history` — the live entry is never trimmed; **why**: guards a future bug that lets history exceed the limit and then trims the live entry.
- `repeated pushes never exceed the limit and always keep the newest live` — invariant over 10 cycles; **why**: the limit holds under sustained use, not just one push.

**view + clear (2)**

- `viewPrev/viewNext clamp at the ends` — no out-of-range viewIndex, empty history stays 0; **why**: an out-of-range index renders nothing.
- `clearHistory wipes everything, but only when idle` — busy phases return identity; **why**: clearing mid-pipeline would orphan the live entry.

**error/set (1)**

- `sets and clears the surfaced error` — set then null; **why**: the dismiss path for banners.

### `src/state/useSession.test.ts` (27 tests; fake bridge wiring)

- `idle -> starting -> recording -> finalizing -> answering -> idle` — the full round trip: bridge calls, event application, viewed entry with metrics; **why**: proves the WIRING between hook, bridge, and reducer — the moments calls are issued, not just the pure rules.
- `the hotkey event toggles exactly like the button` — `hotkey:toggle` drives the same transitions; **why**: two entry points, one behavior.
- `drops events arriving before the start call resolved (no buffering)` — pre-resolution events ignored, post-resolution ones apply; **why**: adopting early events would let a superseded session write into a new one.
- `drops events carrying a different sessionId` — wrong-id partial/level/error do nothing; **why**: the staleness filter at the hook layer.
- `returns to idle silently and cancels the session if it starts anyway` — abort-while-starting, then `cancelSession` on the orphan the core opened; **why**: the session that resolves after the abort must be cancelled, not leaked.
- `a failed start discards the empty attempt and surfaces the error` — idle + error + empty history; **why**: failed starts fully unwind through the hook too.
- `returns to idle immediately instead of hanging in finalizing; transcript retired` — stop NotTaken unsticks the UI, transcript kept; **why**: the "Finalizing… forever" failure mode.
- `discards the attempt when nothing was captured` — NotTaken with nothing captured leaves no husk; **why**: empty entries pollute history.
- `a session:error with code aborted tears down silently, keeping captured work` — no banner, words kept; **why**: aborted is never surfaced, but captured work is never lost.
- `refuses while starting, recording, and finalizing without touching the live session` — `submitAsk` resolves false in all three phases, `ask` never called, live transcript intact; **why**: a typed ask must not kill an in-flight recording.
- `refuses empty and whitespace-only text` — resolves false, no bridge call; **why**: garbage input stops at the hook.
- `resolves true only when the core accepted it` — accepted ask returns true and enters answering; **why**: the caller (the form) clears its input off this boolean.
- `a rejected ask resolves false, retires the typed question, surfaces the error` — question kept for retry; **why**: the user must not retype a rejected question.
- `asking during a streaming answer supersedes it: old session cancelled, its events dropped` — `cancelSession(1)`, partial retired, zombie delta ignored; **why**: supersession end-to-end through the hook.
- `toggleRecord during a streaming answer also supersedes it` — record over answering cancels and starts; **why**: same rule from the record side.
- `elapsedMs tracks wall-clock time between fires` — the counter reads a mocked monotonic `performance.now`, not a count of timer fires; **why**: charging a fixed 250 ms per fire undercounts whenever WebView2 throttles the interval — the visible timer must track the wall clock.
- `caps locally at 120 s: NO stop issued, the core auto-stop answer lands intact` — crossing the cap flips to finalizing without ever calling `stopSession`, the core's answer streams through delta/done with no error, and the flag survives through the answer then clears on the next start; **why**: the core's cap timer is armed before ours and emits no event when it fires (§3) — a frontend `stop_session` here is refused "not taken", and that double-stop teardown dropped every capped recording's answer behind a spurious internal error.
- `caps at true wall-clock 120 s even when timer fires are throttled and lumpy` — three rare, late fires (45 s + 45 s + 31 s) still clamp elapsed at exactly 120 s with the flag latched; **why**: WebView2 background throttling makes fires rare — counted as fixed 250 ms deltas those three fires read as 0.75 s and the cap never fires at all.
- `adopts an answer delta arriving while still "recording" (core capped first)` — a delta/done for the current session before any tick crossed the cap still transitions to answering and completes cleanly, with no stop issued; **why**: answer events are the authoritative "recording ended" signal in the cap race — belt and braces below any tick timing.
- `keeps the last 6, trims the oldest, and the in-flight entry survives the trim` — HISTORY_LIMIT through real completions; **why**: the limit holds with the real hook plumbing, and the live entry survives.
- `regenerate re-asks the viewed question as a new entry` — re-ask with the same text, new entry, view jumps; **why**: regenerate is "ask this again", not "overwrite the old answer".
- `regenerate is a no-op when the viewed entry has no question` — no bridge call; **why**: nothing to re-ask.
- `clearHistory wipes when idle and is refused otherwise` — refused during answering, works after done; **why**: same guard as the reducer, proven through the hook.
- `viewPrev/viewNext clamp at the ends` — clamped navigation over three entries; **why**: navigation must not run off either end.
- `keeps the partial answer, surfaces the error, returns to idle` — mid-stream `session:error` preserves the partial; **why**: read text must survive the banner.
- `setError clears a surfaced error` — set then null; **why**: the banner dismiss path.
- `unmount unsubscribes every bridge listener` — 6 subscriptions before, 0 after; **why**: leaked listeners apply events into an unmounted tree.

### `src/state/format.test.ts` (10 tests; the spec pins these strings verbatim)

- `renders mm:ss with zero padding` — floors seconds, pads both fields; **why**: `999ms` rounding up would tick the timer a second early.
- `clamps negative input to zero` (formatDuration) — `-500` → `00:00`; **why**: clock skew must not render garbage.
- `renders seconds with exactly one decimal` — `1200` → `1.2s` etc.; **why**: the latency chip's exact format is pinned.
- `clamps negative input to zero` (formatLatency) — `-100` → `0.0s`; **why**: same skew guard.
- `passes an already-displayable accelerator through` — `Ctrl+Shift+Space` unchanged; **why**: no gratuitous rewriting.
- `normalizes the cross-platform Ctrl spellings (this app is Windows-only)` — `CommandOrControl`/`CmdOrCtrl`/`Control` → `Ctrl`; **why**: Tauri's cross-platform spellings would read as noise on Windows.
- `capitalizes lowercase tokens and single keys` — `ctrl+a` → `Ctrl+A`; **why**: consistent display of hand-typed accelerators.
- `handles empty and whitespace-padded input without inventing separators` — `""` stays `""`, padding collapses; **why**: an empty (disabled) hotkey must not render a stray `+`.
- `emits the pinned wording, order, and "·" separators exactly` — the full latency tooltip string, with the total pinned as `X.X s` (space before the unit); **why**: §9 pins it verbatim, and the space deliberately diverges from the chip's spaceless `X.Xs` (`formatLatency`, above) — the two formats drifting toward each other is a UI regression, not a cleanup.
- `reports 0 ms transcript finalization for typed questions (no STT stage)` — tooltip with 0 ms; **why**: honest metrics for the ask path.

### `src/state/bridge.test.ts` (9 tests; `@tauri-apps/api` mocked)

- `converts a rejected invoke into an internal-code error envelope` — a thrown Error becomes `{ ok:false, error:{ code:'internal' } }`; **why**: the bridge's whole job is one error path — the UI never catches rejections.
- `stringifies non-Error throwables so the message is never "[object Object]" surprise-free` — a string rejection carries its text; **why**: unreadable error text is unactionable.
- `passes a resolved envelope through untouched` — ok envelopes pass verbatim; **why**: the bridge must not rewrap success.
- `never rejects: every command method resolves even when invoke throws` — all six commands resolve with `ok:false`; **why**: one leaked rejection crashes the calling flow.
- `uses the snake_case command names and argument shapes the core registered` — every command's exact name and args, including `stop_session` taking `sessionId` (not `id`); **why**: Tauri camelCases the Rust `session_id` parameter — the wrong key fails every stop with a missing-key error.
- `cancelSession swallows a rejection (cancel races teardown by design)` — no unhandled rejection; **why**: cancel legitimately races session teardown; a leaked rejection would fail the run.
- `delivers listened payloads to the handler and unlistens on unsubscribe` — payload delivery plus real unlisten; **why**: the event plumbing both directions.
- `unsubscribing before listen resolves still detaches` — off() before the async registration completes still unlistens; **why**: the unmount-during-registration race leaks a listener otherwise.
- `setBridge replaces what getBridge returns` — the test seam works; **why**: every other frontend test depends on this swap.

## Frontend — views

### `src/views/App.test.tsx` (10 tests; real `useSession` + real views over a `FakeBridge`)

- `walks the full pipeline against real state` — Record → partial → Stop → deltas → done, with status texts, live tag, and the latency chip carrying the metrics tooltip; **why**: the one test where every real layer (views, hook, reducer, bridge seam) runs together.
- `copies the markdown source and confirms` — Copy writes the SOURCE (`- first\n- second`), not rendered text, and shows "Copied ✓"; **why**: pasting into a doc must reproduce the markdown, not flattened list text.
- `surfaces a session error, and aborted renders nothing` — aborted → no alert; a real code → alert with the message; **why**: the aborted-is-silent contract at the full-app level.
- `sends the text, clears the input, and streams the answer` — accepted ask clears the field and stays enabled during answering; **why**: only starting/recording/finalizing lock the ask box.
- `keeps the input and shows the error when the ask is rejected` — rejected ask preserves the typed text; **why**: no retyping after a failure.
- `is disabled while recording` — the ask input is disabled mid-recording; **why**: an ask cannot supersede a recording from the form.
- `toggles recording like the Record button` — `hotkey:toggle` starts and stops; **why**: the global shortcut is a first-class entry point.
- `is ignored while Settings is open, and works again after close` — hotkey inert with Settings open, focus returns to the gear, hotkey works after; **why**: the user may be typing the accelerator itself into the hotkey field.
- `lights the chip the SAVE returned, not the one clicked` — core coerces Brief→detailed and the UI follows the response; **why**: the persisted value is truth; the click is only a request.
- `sends only the touched key field and re-checks the hotkey` — the patch carries the typed key, omits untouched keys, sends the rest of the form whole, and `hotkeyStatus` is re-fetched after save; **why**: saving may re-register the shortcut, and untouched keys must never be re-sent.

### `src/views/MainView.test.tsx` (45 `it(...)` → 56 runtime cases; hand-built `SessionApi`, assertions on rendered DOM)

**status line (9 source / 12 cases)**

- `idle without a registered hotkey` — the ready line with `role="status"`; **why**: the status line is the app's narrator and must be announced.
- `idle appends the formatted hotkey when registered` — ready line + formatted accelerator; **why**: the shortcut is only advertised when it actually works.
- `%s` (it.each: starting/recording/finalizing/answering) — each state shows its exact status text; **why**: the four pinned in-flight strings.
- `explains the auto-stop when the recording cap was hit` — the 120s-limit wording; **why**: an unexplained auto-stop looks like a bug.
- `shows the first-run nudge when the Deepgram key is missing` — first-run text; **why**: the empty-state path to Settings.
- `first-run keys off the SELECTED provider` — Groq selected + missing Groq key → nudge; **why**: the nudge must track the provider in use.
- `a missing key for the UNSELECTED provider is not first-run` — ready line despite a missing unused key; **why**: nagging about an unused provider's key is noise.
- `shows Done after an answer completes cleanly` — answering → idle shows Done; **why**: closure for the finished flow.
- `does not claim Done when the session ended in an error` — error-idle shows no Done; **why**: "Done" over an error banner is a contradiction.

**record button (5 source / 9 cases)**

- `labels %s as %s` (it.each: idle/starting/recording/finalizing/answering) — the five exact labels; **why**: the button is the primary control; its label is the state.
- `is disabled only while finalizing` — disabled in finalizing, enabled in starting; **why**: finalizing is the one phase with nothing valid to do.
- `shows the formatted hotkey chip when registered` — chip text present; **why**: discoverability of the shortcut.
- `drops the chip when the hotkey is not registered` — accessible name is exactly "Record"; **why**: advertising a dead shortcut misleads.
- `click toggles the session` — `toggleRecord` called once; **why**: the button's one job.

**hotkey taken notice (3 source / 4 cases)**

- `uses the exact wording` — the full "already taken by another app" sentence; **why**: pinned wording that explains the failure and both remedies.
- `is absent while the hotkey is registered` — no notice; **why**: the notice only belongs to the failure state.
- `renders no notice, no chip, no status suffix for a disabled (%s) hotkey` (it.each: empty/whitespace-only) — an empty accelerator with `registered:false` produces no "taken" notice, no chip inside the Record button, and no "or <hotkey>" status suffix; **why**: §8 — an empty accelerator means the user turned the shortcut OFF, but the bridge still reports `registered:false`, which used to trip the "taken by another app" notice with a blank key name — a false accusation for a deliberate setting.

**recording row (2)**

- `shows the level meter and mm:ss timer while recording` — meter `aria-valuenow` and formatted timer; **why**: live feedback that audio is being heard.
- `is absent when idle` — no meter; **why**: the row exists only during recording.

**question heard panel (6)**

- `shows the placeholder when nothing was ever recorded` — placeholder text; **why**: the empty state must explain itself.
- `shows Listening… and the live tag while recording with nothing heard` — Listening… + live; **why**: silence during recording needs distinct feedback from idle.
- `shows the live transcript text while recording` — the partial text renders; **why**: the transcript is the user's confirmation the right question was heard.
- `drops the live tag once recording ends` — no live tag when idle; **why**: "live" on a finished transcript is a lie.
- `hides the live tag when viewing an OLDER entry mid-recording` — an older entry viewed during recording shows its text with no live tag; **why**: the tag belongs to the entry the STT is feeding — a pulsing "live" on finished text claims the visible words are still moving while the real transcript updates out of view.
- `keeps the live tag when viewing the LIVE entry mid-recording` — viewing the newest entry mid-recording keeps the tag; **why**: the positive half of the scoping rule — the genuinely live entry must still say so.

**ask form (4 source / 7 cases)**

- `is disabled while %s` (it.each: starting/recording/finalizing) — input disabled; **why**: an ask mid-pipeline is refused, so the control must look refused.
- `stays enabled while %s` (it.each: idle/answering) — input enabled; **why**: asking during answering is legal (it supersedes).
- `clears the input only when the ask is accepted` — accepted → cleared; **why**: clearing on a refusal would eat the question.
- `keeps the input when the ask is refused so the user can retry` — refused → text intact; **why**: same guarantee, negative case.

**style chips (2)**

- `pressed state mirrors the persisted style, not the clicked chip` — clicking Brief while settings still say detailed keeps Detailed pressed; **why**: after a failed or coerced save, lighting the clicked chip shows a style that is not in effect.
- `surfaces a failed style save in the error box` — `setError` called with the failure; **why**: a silent failed save leaves the user thinking the style changed.

**error box (2)**

- `renders real errors with role alert` — alert contains the message; **why**: errors must be announced.
- `renders nothing for aborted` — no alert; **why**: aborted is invisible by contract.

**history bar (6)**

- `is hidden with fewer than two entries` — no navigation; **why**: navigation over one entry is noise.
- `appears at two entries with an n/m label and nav arrows` — 2/2, Next disabled at the end, Prev fires; **why**: the bar's core affordances.
- `disables prev at the oldest entry` — Prev disabled, Next enabled; **why**: end clamping made visible.
- `disables Clear unless idle` — Clear disabled during answering; **why**: clearing mid-pipeline is refused, so the button must show it.
- `Clear clears, announces, and moves focus to Record` — clearHistory fires, "History cleared" announced, focus lands on Record; **why**: post-clear focus must not be lost on a removed element.
- `announces History cleared again on a second Clear` — the announcement appears, is wiped, and appears again on the next Clear; **why**: a second identical string is a state update React bails out of — no DOM change reaches the live region and the screen reader hears nothing (§9 pins the announcement for every Clear).

**header (2)**

- `opens settings from the gear` — `onOpenSettings` fires; **why**: the settings entry point.
- `keeps the gear disabled until settings load` — disabled with `settings: null`; **why**: opening a form over unhydrated settings shows blanks that then jump.

**regenerate visibility (4)**

- `is shown when the viewed entry has a question and state is idle` — button present; **why**: regeneration is offered exactly when it can act.
- `is shown during answering` — present mid-stream; **why**: re-asking during a stream is legal supersession.
- `is hidden while recording` — absent; **why**: regenerating over a live recording would kill it.
- `is hidden when the viewed entry has no question` — absent; **why**: nothing to re-ask.

### `src/views/SettingsView.test.tsx` (12 tests)

- `moves focus into the heading on open` — the Settings heading has focus; **why**: keyboard/screen-reader users must land inside the dialog.
- `Escape closes` — `onBack` fires; **why**: the standard dismissal path.
- `Back closes` — `onBack` fires from the button; **why**: the visible dismissal path.
- `are always empty, with a replace placeholder only where a key exists` — key inputs render empty; "saved — type to replace" only where a key is stored; **why**: stored key material never round-trips into the DOM, and promising "saved" where nothing is saved is a lie.
- `are password inputs so keys never show on screen shares` — all three key fields are `type="password"`; **why**: this app is used during screen-shared calls.
- `omits untouched key fields but always sends the rest of the form` — patch has no key fields but the full non-key form; **why**: sending an empty key field would wipe a stored key on every unrelated save.
- `sends a typed key, and only that key` — one key in the patch; **why**: minimal-diff key updates.
- `a typed-then-cleared key sends "" to wipe the stored key` — explicit `""` travels; **why**: type-then-clear is the deliberate delete gesture and must be distinguishable from untouched.
- `a successful save resets key fields to untouched` — a second save omits the key again; **why**: a second save must not silently re-send (and re-store) the same key.
- `sends changed provider, style, hotkey, and always-on-top values` — all four changed values land in the patch; **why**: the non-key form travels whole and current.
- `shows Saved ✓ briefly after a successful save` — appears then disappears; **why**: confirmation that also gets out of the way.
- `reports a failed save in a settings-local error box` — alert with the message, no Saved ✓; **why**: a failed save that looks like success loses the user's edits.

## Frontend — components

### `src/components/AnswerPanel.test.tsx` (17 tests)

- `shows the placeholder when no entry exists` — the stream-here placeholder; **why**: the empty state explains what the panel is for.
- `marks streaming with the generating tag and aria-busy` — generating… tag, `aria-busy="true"`, `aria-live="polite"`; **why**: assistive tech must know the region is mid-update.
- `drops aria-busy when the stream settles` — `aria-busy="false"`, no tag; **why**: a permanently-busy region is never announced as done.
- `shows the latency chip with the metrics tooltip once metrics exist` — chip text + `title` tooltip; **why**: the headline metric and its breakdown.
- `hides the latency chip while metrics are missing` — no chip mid-stream; **why**: a chip with no data would show a placeholder lie.
- `renders and fires only when allowed` — Regenerate fires when allowed, disappears when not; **why**: the visibility rule enforced at the component level.
- `copies the markdown source and shows Copied ✓ briefly` — writes the SOURCE, confirms visually AND via the screen-reader announcement, then clears; **why**: the visual swap alone is silent to assistive tech.
- `announces every copy: text lands, clears, and lands again` — the `role="status"` region pre-exists the copy (empty), fills with the announcement, is wiped, and fills again on a second copy; **why**: several SR/browser combos ignore a live region inserted together with its text, and a second identical write is a DOM no-op React skips — only a permanently-mounted region wiped between copies makes every copy audible.
- `routes a clipboard failure to the error box callback` — `onCopyError` with an `internal` code, no Copied ✓; **why**: clipboard permission failures must surface, not silently no-op.
- `is hidden while there is no answer yet` — no Copy button on an empty answer; **why**: copying nothing is a broken affordance.
- `renders at most one update per animation frame and lands on the latest text` — two deltas inside one frame paint once, with the final text; **why**: painting per delta janks the stream; coalescing must still land on the newest text.
- `renders immediately once the stream is over` — completion paints without waiting a frame; **why**: the finished answer must not lag a frame behind.
- `sticks to the bottom when the reader is within the threshold` — near-bottom scroll follows new tokens; **why**: a reader following along must not have to chase the stream.
- `leaves a reader who scrolled up alone` — scrolled-up position survives new tokens; **why**: yanking a re-reading user to the bottom is hostile.
- `resets to the top when switching history entries` — a different entry starts at the top; **why**: opening an old answer mid-scroll disorients.
- `resets to top on entry switch even when the previous entry fit entirely` — switching from a short (fitting) entry to a long one lands at scrollTop 0, measured against geometry that tracks the COMMITTED content; **why**: the audit fix — deriving nearBottom from the OLD entry's DOM counted "it fits" as "following" and bottom-stuck the next entry to 400 instead of §9's pinned top.
- `re-engages stick-to-bottom for a new stream after viewing a long answer` — a reader parked at the top of a long answer still gets stick-to-bottom when a NEW session's tokens stream in; **why**: nearBottom measured against the old long DOM stayed false forever, so no later answer ever followed again — it must be re-derived against the fresh entry.

### `src/components/ErrorBox.test.tsx` (3 `it(...)` → 14 runtime cases)

- `renders %s as an alert carrying the message` (it.each over the 12 rendered codes: `no_stt_key`, `no_llm_key`, `stt_connect`, `stt_error`, `stt_timeout`, `no_speech`, `llm_auth`, `llm_http`, `llm_rate_limit`, `llm_first_token_timeout`, `llm_timeout`, `internal`) — each renders a `role="alert"` with the raw message plus a human title; **why**: every code in the closed set must have a rendering — an unmapped code would show nothing for a real failure.
- `renders nothing for aborted — the user did that on purpose` — empty DOM; **why**: the aborted-is-silent contract at the component that would otherwise paint it.
- `renders nothing when there is no error` — empty DOM; **why**: the null state must be truly empty, not an empty frame.

### `src/components/HistoryBar.test.tsx` (6 tests)

- `hides below two entries` — no navigation for one entry; **why**: a one-entry navigator is noise.
- `shows the n/m position label` — `2/2`; **why**: orientation within history.
- `disables the arrows at the ends and fires them in between` — mid-list arrows both fire; **why**: the callbacks are the component's contract.
- `disables prev at the oldest and next at the newest` — end clamping in the DOM; **why**: disabled ends prevent no-op clicks and signal position.
- `permits Clear only while idle` — disabled when not idle; **why**: mirrors the reducer's clear guard visually.
- `fires onClear` — the callback fires; **why**: the destructive action's wiring.

---

## Known test-environment notes

- **Two paused-clock Deepgram tests** (`keepalives_flow_on_an_idle_open_socket`, `no_keepalive_is_sent_once_close_is_requested` in `core/src/stt/deepgram.rs`) interact with tokio's paused-clock auto-advance while using real loopback sockets. With the clock paused, an idle runtime auto-advances to the nearest timer, which can fire the 5 s connect timeout before real loopback bytes ever move, or let a drain cap race real socket readiness. They were made deterministic by (a) pausing only after the handshake / proving establishment (a transcript round-trip) before stopping, and (b) ending the drain via the server sending its own WebSocket Close rather than relying on the drain cap timing out.
- **jsdom boot on this machine is slow**, so `vite.config.ts` sets vitest's `testTimeout` (and `hookTimeout`) to 20 s — a cold jsdom start must not read as a test failure.
- **jsdom under vitest 3 runs a real rAF clock** (`pretendToBeVisual`): `requestAnimationFrame` fires on its own ~16 ms frame clock, NOT the timer queue, so flushing the answer panel's frame-coalesced paint with a `setTimeout(0)` only caught a pending frame when the machine happened to be slow — a latent flake. The `frame()` helpers in `App.test.tsx` and `AnswerPanel.test.tsx` therefore await a `requestAnimationFrame` of their own: rAF callbacks run in scheduling order, so our frame firing guarantees any frame the panel queued earlier has already fired. (`vitest.setup.ts` keeps a macrotask rAF polyfill only for environments with no rAF at all.)

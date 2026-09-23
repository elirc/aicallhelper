# Testing

Every test in this repo runs with **no network, no audio device, and no live provider**. The rule is enforced by construction:

- **Rust core (`app-core`)** — every external dependency sits behind a trait (`SttConnector`/`SttStream`, `LlmProvider`/`LlmSink`, `AudioCapture`/`AudioSink`, `EventSink`). The session state machine is tested against scripted fakes with a paused tokio clock; the Anthropic/Groq/Deepgram clients are tested against scripted local servers on `127.0.0.1` (raw TCP for HTTP/SSE, tokio-tungstenite for the Deepgram WebSocket) so the tests control the exact bytes on the wire, including malformed ones. The free local mode is tested the same way: the Ollama NDJSON decoder and request builder are pure functions, and the local speech client runs against a scripted tokio-tungstenite server on a loopback port — no Ollama, no Python service.
- **Tauri shell (`src-tauri/src`)** — unit tests cover the pure seams (envelope shapes, event wire mapping, debounce/limiter/URL-guard logic, timestamp formatting, the provider key gates, the active-profile prompt, the local budget pre-checks, and the local-voice parsers and launch plan; the settings effect reconciler is tested in the core, where its OS calls sit behind the `OsEffects` trait). These compile with the shell crate, not `app-core`.
- **Frontend** — the Tauri IPC surface is a single `Bridge` object; tests install a `FakeBridge` via `setBridge()` (and `vi.mock` the `@tauri-apps/api` modules in `bridge.test.ts`) and emit core events exactly as the Rust side would. The local-voice panel goes through the same fake (`dockToCamera`, `localVoiceStatus`, `prepareLocalVoice`), never a `vi.spyOn`.

Commands:

- Rust core: `cargo test -p app-core` (run from `src-tauri/`) — **374 unit tests + 1 integration test** (`core/tests/local_settings.rs`); cargo reports them as `374 passed` then `1 passed`, plus 0 doc-tests.
- Tauri shell: `cargo test -p aicallhelper` from `src-tauri/` also compiles the shell crate's **58 tests** (commands/events/window/logging/local_voice). If the disk is too full for the shell test binary, `cargo check -p aicallhelper --tests` at least type-checks them.
- Frontend: `npm test` (`vitest run`) — **426 test cases** across 16 files (380 `it(...)`/`test(...)` declarations; parameterized `it.each` blocks and the two corpus loops expand to the larger runtime count, noted per file below). The full run takes ~70 s on a warm machine and up to ~6 minutes on a cold one.

Files with **no** tests (so nothing is missing, just absent by design): `core/src/lib.rs` and `core/src/stt/mod.rs` (re-exports only); in the shell `src/main.rs`, `src/lib.rs` (Tauri setup — pure I/O, exercised by running the app), `src/state.rs` (a struct), and `src/hotkey.rs` (a thin wrapper over the global-shortcut plugin: unregister-all then register, with the plugin owning every observable outcome — its wire shape is pinned indirectly by `bridge.test.ts` and the `hotkey_status` consumers in `MainView.test.tsx`). `src/local_voice.rs`, which used to be all I/O, now has a tested pure core (see its section).

---

## Rust core — errors

### `core/src/error.rs` (2 tests)

- `error_codes_serialize_to_the_documented_wire_strings` — every `ErrorCode` (fourteen, `settings_conflict` included since ADR 016) serializes to its exact snake_case wire literal; **why**: the frontend switches on these strings, so a rename is a silent behavior change across the IPC boundary.
- `aborted_is_recognisable_so_the_ui_can_stay_silent` — `is_aborted()` is true only for the aborted error; **why**: "aborted" must be detectable so the UI never renders an error for a user-initiated cancel.

## Rust core — LLM

### `core/src/llm/prompt.rs` (31 tests; the last 9 pin the R4 local budget against the real `local::request_body` gate)

- `role_instructions_are_verbatim` — pins `ROLE_INSTRUCTIONS` byte-for-byte; **why**: the prompt text is product behavior — "improving" the wording is a product decision and must break a test.
- `empty_profile_yields_role_instructions_only_and_no_grounding_note` — no resume/JD means no headers and no grounding note; **why**: telling the model to ground in an absent resume produces hedging about nothing.
- `whitespace_only_profile_counts_as_absent` — whitespace-only resume/JD is treated as empty; **why**: a blank-looking profile must not smuggle in headers and the grounding note.
- `resume_only_still_gets_the_grounding_note` — resume without JD still appends the note; **why**: grounding applies whenever either section exists.
- `jd_only_still_gets_the_grounding_note` — JD without resume still appends the note; **why**: same rule from the other side.
- `sections_appear_in_resume_then_jd_order` — resume header precedes the JD header; **why**: a stable section order keeps the cached prefix byte-stable.
- `interior_resume_formatting_survives_verbatim` — only the edges are trimmed; **why**: a resume's internal blank lines and indentation carry meaning the model reads.
- `style_lives_outside_the_cached_prefix` — all three styles share one identical `cached_prefix`, checked for the v3 interview shape AND a full Sales profile (call line, background, context, focus, extra); **why**: a style flip must not invalidate the Anthropic prompt cache (the whole point of the split, §3), and a new section must never be accidentally routed after the breakpoint.
- `style_suffixes_are_verbatim` — pins all three style strings byte-for-byte; **why**: same "strings are product behavior" rule as the role instructions.
- `prompt_is_byte_stable_across_repeated_builds` — 50 rebuilds produce identical output for both the interview shape and the full Sales profile; **why**: cache hits are a byte-prefix match — any nondeterminism silently costs a cache write per call, and the Sales profile exercises every branch of the builder.
- `unknown_style_falls_back_to_balanced` — corrupt/unknown style strings parse as Balanced; **why**: the settings file is user-writable, so garbage is a real input path (§7).
- `user_message_wrapper_is_verbatim` — pins the triple-quote transcript wrapper; **why**: the wrapper distinguishes speech from instruction and lives outside the cached prefix.
- `transcript_is_not_escaped_or_trimmed_by_the_wrapper` — quotes and punctuation survive; **why**: the transcript is data, not markup — mangling it changes the question being answered.
- `joined_prompt_places_style_after_the_prefix` — `joined()` is prefix + blank line + suffix; **why**: Groq gets one system string and it must read like the two Anthropic blocks.
- `migrated_v3_profile_yields_a_byte_identical_prefix` — an interview profile with empty focus/extra builds exactly `ROLE_INSTRUCTIONS · RESUME_HEADER · R · JD_HEADER · J · GROUNDING_NOTE`, spelled out from the pinned constants, and `Profile::default()` equals the explicit `CallType::Interview` literal; **why**: the upgrade must not change one byte of what existing users' prompts say, or every one of them pays a cache write on first launch (ADR 007) — a new section slipping into the interview path breaks this.
- `call_type_lines_and_new_headers_are_verbatim` — pins all nine new constants (`CALL_TYPE_SALES/SUPPORT/MEETING/OTHER`, `BACKGROUND_HEADER`, `CONTEXT_HEADER`, `GROUNDING_NOTE_CALL`, `FOCUS_HEADER`, `EXTRA_INSTRUCTIONS_HEADER`) AND re-pins the three v3 header strings; **why**: these sentences are what a sales or support user hears the app say — product behavior, like `ROLE_INSTRUCTIONS`.
- `interview_emits_no_call_type_line` — an interview profile contains none of the four call-type lines and the resume header follows the role text immediately; **why**: the interview path is the v3 path — a call line there would break byte-identity for every migrated user.
- `non_interview_uses_background_context_headers_and_call_grounding_note` — for each of Sales/Support/Meeting/Other the prefix is exactly `ROLE · call line · BACKGROUND_HEADER R · CONTEXT_HEADER J · GROUNDING_NOTE_CALL`, with no `RESUME_HEADER`/`JD_HEADER`/`GROUNDING_NOTE`; **why**: "the job they are interviewing for" must never reach a sales, support, meeting or general-call profile — the whole header set swaps, not one line.
- `sections_appear_in_call_resume_jd_grounding_focus_extra_order` — a full Sales profile's sections appear in the §3.1 order, pinned end to end as one exact string (with the resume's interior newlines intact); **why**: a stable section order keeps the cached prefix byte-stable; pinning the whole string means the order can never drift by one section.
- `whitespace_only_focus_and_extra_count_as_absent` — `" \n\t"` focus and `"\n\n"` extra emit neither header; **why**: same edge rule as the resume — a stray newline in an optional field must not emit an empty section header the model then puzzles over.
- `focus_alone_does_not_trigger_the_grounding_note` — focus (and focus + extra) with no resume/JD yields no grounding note, for interview and Sales alike; **why**: a focus line is a steer, not something to ground in — "ground every answer in the background above" with no background is the same lie as the empty-profile case.
- `unknown_call_type_falls_back_to_interview` — the five lowercase names parse, and `""`/`"SALES"`/`"persona"` read as Interview (the `Default`); `as_str()` and the serde form round-trip through `parse_or_default`; **why**: the settings file is hand-editable — a corrupt value must give v3's framing, never a guess at a different call type, and a value that survives save/load must be the same value.
- `budget_fixed_overhead_for_interview_balanced_resume_only_is_771` — an interview profile with only a 6 500-byte resume, Balanced, `"Hi?"`: fixed 771, profile 6 500, question 3, used 7 274, remaining −274, `over`, and the gate refuses it; a resume of `7000 − 771 − 1` bytes leaves exactly 1 byte (`tight`); **why**: the maintainer's hand-count is verified rather than trusted, and the review's reproduction (a 6 500-byte profile the old warning let through, 274 bytes over) is pinned.
- `budget_equals_the_gate_for_every_call_type_style_and_field_combination` — 5 call types × 3 styles × all 16 combinations of the optional fields: `usedBytes` equals the bytes of the body `local::request_body` builds, and `used = fixed + profile + question`; **why**: the preview must count exactly what the gate counts whatever sections the builder adds (call line, headers, grounding note) — a second, hand-written calculation is how the 6 500 heuristic went wrong.
- `budget_counts_edge_trimmed_fields_and_whitespace_only_as_absent` — edge-padded fields budget the same as unpadded ones, and whitespace-only fields the same as empty ones (no header); **why**: the builder trims edges, and the old UI counter summed raw fields, over-counting padding.
- `budget_counts_utf8_bytes_not_characters` — 3 emoji + 2 CJK + `é` count 20 profile bytes, `¿Qué?` 7 question bytes, and the total equals the gate's; **why**: the cap is UTF-8 bytes; a character count under-reports non-Latin text by up to 4×.
- `budget_boundaries_match_the_gate_at_limit_minus_one_limit_and_plus_one` — for every call type and style, questions sized to land at limit − 1, limit and limit + 1: the first two fit (the gate builds a body; at the limit `remaining 0`, `tight`), the third is `over` and the gate refuses with `local::oversize_error()`; **why**: an off-by-one between preview and gate would let the UI promise a question the backend then refuses, or block one it would accept.
- `an_empty_question_is_over_only_when_not_even_one_byte_fits` — with no question, remaining 1 is `tight`, remaining 0 and −1 are `over`; **why**: the pre-record check uses the empty question, and no real question is zero bytes.
- `the_reserve_separates_ok_from_tight_and_is_not_a_limit` — 200 bytes left is `ok`, 199 is `tight`, and a question leaving 1 byte still gets a body from the gate; **why**: the 200-byte reserve is a usability warning (FINAL-REVIEW §4), never a second, arbitrary hard limit.
- `switching_profile_or_style_changes_the_budget_by_exactly_the_text_and_suffix` — a longer resume changes `used` by exactly its extra bytes with the fixed overhead unchanged; Balanced → Detailed changes it by exactly the style-suffix difference; **why**: the Settings preview recomputes on profile and style switches, and each must move the figure by what actually changed.
- `budget_serializes_the_wire_shape` — the JSON keys are exactly `fixedBytes, limitBytes, profileBytes, questionBytes, remainingBytes, reserveBytes, status, usedBytes` and `status` is `ok`/`tight`/`over`; **why**: pinned against `LocalPromptBudget` in `src/types.ts`.

### `core/src/llm/sse.rs` (23 tests)

- `parses_a_simple_lf_stream` — baseline LF-terminated events decode; **why**: the trivial case every other guarantee builds on.
- `parses_a_crlf_stream` — CRLF-terminated events decode; **why**: real providers send CRLF.
- `every_single_split_point_gives_the_same_result_as_one_shot` — for seven bodies, every possible chunk split (and byte-by-byte) equals a one-shot decode; **why**: the network decides where chunks break, so no split point may change the output.
- `crlf_split_between_cr_and_lf_does_not_dispatch_twice` — the split between `\r` and `\n` yields one event; **why**: the historical regression where the trailing `\n` reads as a blank line and dispatches the event early, cutting it in two.
- `multibyte_utf8_split_across_chunks_survives` — em dash/emoji cut mid-character reassemble; **why**: the network will cut a UTF-8 sequence in half; buffering raw bytes must make this safe.
- `done_sentinel_is_framed_like_any_event_not_swallowed` — `[DONE]` and the event after it are both handed to the caller (renamed in R2 from `done_sentinel_is_skipped_not_treated_as_a_terminator`); **why**: the decoder is framing only — whether `[DONE]` ends the answer is the Groq provider's protocol decision (it does, since R2), and a decoder that swallowed or stopped at it would take that decision away from the one place that owns it.
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
- `a_line_with_no_terminator_is_refused_at_the_cap_while_reading` — 64 KiB chunks with no newline: 15 fit under `MAX_LINE_BYTES` (1 MiB), the 16th fails `feed` with `SseOverflow`; **why** (R2): a server that never sends a newline must fail at the chunk that crosses the cap, not grow a buffer until the process runs out of memory.
- `an_event_joined_from_many_data_lines_is_capped_too` — many 1 000-byte `data:` lines of one event overflow `MAX_EVENT_BYTES`; **why**: each line is under the line cap, so only the per-event cap stops this shape.
- `frames_at_the_line_cap_still_decode` — a line of exactly `MAX_LINE_BYTES` decodes to one event; **why**: the caps must not clip a large-but-legal frame.
- `events_framed_before_an_overflow_in_the_same_chunk_are_still_delivered` — `data: a` followed in the SAME chunk by an over-cap line: the overflow carries `decoded == [a]`, and the providers apply it before failing; **why** (review F3): the kept partial text must not depend on how the network chunked the bytes.

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

### `core/src/llm/http.rs` (5 tests)

- `shared_client_is_the_same_instance_on_every_call` — pointer identity across calls; **why**: pre-warm only works if the warm GET and the answer POST share one connection pool, which the client instance owns.
- `shared_client_is_the_same_instance_across_threads` — pointer identity across spawned threads; **why**: the warm task and the answer request run on different tokio workers.
- `client_bounds_the_connect_but_never_the_whole_request` — the builder's Debug output carries `connect_timeout: 3s` and no whole-request `timeout`; **why**: a black-holed SYN otherwise waits on the OS (~21 s on Windows, past the first-token cap), while a whole-request timeout would cut a long streamed answer off mid-sentence (RS-6).
- `keepalive_probes_before_a_gateway_can_forget_the_warm_socket` — pins `TCP_KEEPALIVE_IDLE` = 20 s, `TCP_KEEPALIVE_INTERVAL` = 1 s, interval < idle < `POOL_IDLE_TIMEOUT` (still 120 s); **why**: keepalive is not observable through the built client, so the numbers are pinned here — the first probe must land inside the pool idle window or the pool reaps the socket before keepalive ever mattered, and the interval must be explicit because an unset one reaches Windows as 0.
- `error_bodies_are_read_only_up_to_the_cap_even_if_the_server_never_ends` — a loopback server promises 10 MB, sends 64 KiB and stalls; `read_error_body` returns exactly `MAX_ERROR_BODY_BYTES` (16 KiB) within 20 s; **why** (R2): `response.text()` would allocate the whole body and, against a server that never ends it, never return — the cap has to hold while reading.

### `core/src/llm/local.rs` (8 tests; the Ollama client — NDJSON decoder, `LocalStream` completion logic and request builder are pure, no server involved)

- `split_utf8_and_multiple_frames_only_emit_answer_content` — a three-frame NDJSON stream fed one byte at a time decodes to three frames; `apply` skips the `thinking` frame, emits `café` (its UTF-8 was split mid-character), and returns a stop reason only on `done`; **why**: the sink must see exactly the answer text — a thinking frame painted into the answer, or a mid-character split turned into U+FFFD, corrupts what the user reads.
- `malformed_and_oversize_frames_fail` — a non-JSON line and a frame over `MAX_FRAME_BYTES` (256 KiB) are errors, not silently skipped; **why**: an unbounded line buffer against a misbehaving local server is a memory leak mid-call, and garbage on the wire is a real failure to surface.
- `request_is_local_thinking_off_and_profile_is_not_silently_cut` — the request body pins `MODEL`, `think: false`, and carries the profile text; a transcript that pushes the total past `MAX_INPUT_BYTES` (7 000) is REFUSED with `LlmHttp` and the exact PROF-08 message ("Free local mode supports about 7 KB of combined instructions, profile (resume, job description, focus, extra instructions) and question. Shorten the active profile in Settings or use a cloud model."); **why**: a 2 B model quietly truncating the prompt would answer without the resume and nobody would know; the message is pinned verbatim because it tells the user which fields count — a paraphrase that dropped a field would send them trimming the wrong thing.
- `local_provider_answers_on_the_local_deadlines` — `LocalProvider.answer_limits()` is `AnswerLimits::LOCAL` and its kind is `Local`; **why**: Ollama on the CPU needs minutes where the cloud gets seconds — on the trait's cloud default every local answer would time out at 10 s.
- `the_done_frame_ends_the_answer_and_nothing_after_it_is_read` — a chunk carrying content, `{"done":true,"done_reason":"length"}` and a further content frame yields `"Short"` with `StopReason::TokenLimit`, and the later frame never reaches the sink; **why** (R2): `done` is the NDJSON terminator — text after it is not part of the answer — and a capped answer is "cut short", not a failure.
- `eof_before_done_fails_but_the_streamed_text_was_delivered` — content then end of stream → error mentioning "incomplete", sink holds the text; **why**: an early end must be labelled incomplete while the user keeps what they read.
- `a_done_frame_without_text_is_an_explicit_failure` — an empty-content frame then `done` → "empty answer" error; **why**: a terminator without text must never be a blank success.
- `an_error_frame_after_partial_output_fails_and_keeps_the_text` — content then `{"error":"out of memory"}` → `LlmHttp` that does not quote the service text, sink holds the partial; **why**: the error frame wins over the partial, and unbounded service internals stay out of the UI.

### `core/src/llm/mod.rs` (4 tests)

- `cloud_kinds_need_keys_and_deepgram_but_local_needs_neither` — Anthropic and Groq report `needs_cloud_keys()` and `uses_deepgram()`; Local reports neither; **why**: local mode must start with no keys stored at all (§6.5), and the cloud pair must never start without theirs, or the first request fails with a confusing 401 instead of the "No … API key set" nudge — the shell's gates key off these capabilities, not `== Local` checks.
- `answer_limits_default_to_cloud_pacing` — a bare provider that overrides nothing gets `AnswerLimits::CLOUD` (10 s / 60 s); **why**: defaulting to the local pair instead would let a wedged cloud request sit for 90 s before the UI heard anything.
- `stop_reason_distinguishes_only_the_token_limit_spellings` — `max_tokens` and `length` map to `TokenLimit`; `end_turn`, `stop`, `refusal`, an unknown reason and none map to `Complete`; wire strings `"token_limit"`/`"complete"`; **why** (R2): mapping an unknown future reason to "cut short" would mislabel whole answers, and the UI switches on these literals.
- `a_terminal_event_without_usable_text_is_an_error_not_a_blank_answer` — `Answer::from_terminal` refuses whitespace-only text with `LlmHttp` "…without any answer text" and keeps text + stop reason otherwise; **why**: a ping-only or role-only stream used to resolve `Ok("")`, rendered as the model silently saying nothing.

### `core/src/llm/anthropic.rs` (24 tests)

- `happy_path_streams_every_delta_and_returns_their_exact_concatenation` — deltas stream in order, the returned answer is byte-identical to their concatenation, and `cache_read_input_tokens` is exposed; **why**: the invariant that stops text changing after the user has read it.
- `text_deltas_across_multiple_content_blocks_join_with_nothing_between` — two content blocks join with no separator; **why**: any injected joiner corrupts the visible answer.
- `request_pins_model_streaming_max_tokens_and_the_two_system_blocks` — (fixture now ends with `message_stop`, since R2 an unterminated stream is incomplete) captured request carries the key/version headers, pinned model, `stream:true`, `max_tokens`, and two system blocks with `cache_control` on the FIRST only; **why**: a breakpoint on the style block would invalidate the profile cache on every style flip.
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
- `max_tokens_stop_completes_with_a_token_limit_reason` — `stop_reason: max_tokens` + `message_stop` → `Ok` with `StopReason::TokenLimit`, deltas equal the text; `end_turn` → `Complete`; **why** (R2): a capped answer is kept and labelled "cut short", never failed and never mistaken for a normal end.
- `a_stop_reason_without_message_stop_is_still_incomplete` — deltas + `message_delta` (`end_turn`) and clean EOF → `LlmHttp` "…before the answer was finished…", delta delivered; **why**: the refinement over the first plan — `stop_reason` is metadata, not proof the final protocol event arrived.
- `clean_eof_before_message_stop_fails_but_keeps_the_streamed_text` — two deltas then EOF → "incomplete" error, both deltas delivered; **why**: partial text used to be labelled a complete answer.
- `metadata_only_stream_is_an_explicit_failure_not_a_blank_answer` — `message_start` + `ping` + `message_delta` + `message_stop`, no text → "…without any answer text"; **why**: a ping-only 200 used to resolve `Ok("")`.
- `nothing_after_message_stop_is_consumed` — a delta after `message_stop` in the same body never reaches the sink; **why**: bytes after the terminator are not part of the answer.
- `malformed_known_frames_fail_instead_of_silently_dropping_text` — an unparseable `content_block_delta` and a `text_delta` with no `text` each fail "malformed" with the earlier delta kept; **why**: either could have been answer text — skipping it and reporting "done" is the silent truncation R2 exists to prevent.
- `unknown_event_types_and_non_text_deltas_are_harmless` — an unknown event type and a `thinking_delta` are ignored and the answer completes; **why**: a protocol addition must not break answers.
- `an_oversized_frame_is_refused_while_reading` — a 1.1 MiB line on a stalled socket fails "oversized" within 20 s, the earlier delta kept; **why**: only a cap enforced while reading returns here instead of buffering forever.
- `an_endless_error_body_is_read_only_up_to_the_cap` — a 500 promising 10 MB that sends 64 KiB and stalls returns a <400-char "HTTP 500" message within 20 s; **why**: `response.text()` waited for the rest of the body forever.

### `core/src/llm/groq.rs` (23 tests)

- `happy_path_streams_every_delta_and_returns_their_exact_concatenation` — OpenAI-shape chunks stream and concatenate byte-identically; **why**: same read-stability invariant as Anthropic.
- `nothing_after_the_done_sentinel_is_consumed` — REVERSED in R2 (was `done_sentinel_followed_by_more_data_in_the_same_chunk_does_not_truncate`, which expected " world" after `[DONE]` to be appended): `Hello`, `[DONE]`, ` world` in one write yields `"Hello"` and only that delta paints; **why**: Groq documents `data: [DONE]` as the stream terminator, so it is the only proof the answer finished and nothing after it belongs to the answer — painting text after the terminator is what a garbled or concatenated stream looks like.
- `request_pins_model_streaming_reasoning_knobs_and_omits_reasoning_format` — (fixture now ends with `[DONE]`) captured request carries Bearer auth, pinned model, `stream`, `temperature`, `max_completion_tokens`, `reasoning_effort:"low"`, `include_reasoning:false`, no `reasoning_format`, and the system prompt as ONE joined string; **why**: `reasoning_format` is a Qwen-family knob — sent to gpt-oss it is at best ignored, at worst a future request error.
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
- `length_finish_completes_with_a_token_limit_reason` — `finish_reason: length` + `[DONE]` → `Ok` with `TokenLimit`; `stop` → `Complete`; **why** (R2): a capped answer is "cut short", not failed.
- `a_finish_reason_without_done_is_incomplete_and_keeps_the_text` — delta + `finish_reason: stop`, then EOF → "…before the answer was finished…", delta kept; **why**: `finish_reason` is metadata, not the terminator.
- `role_only_stream_that_finishes_is_an_explicit_failure` — role-priming chunk + finish + `[DONE]`, no text → "…without any answer text"; **why**: used to resolve `Ok("")`, a blank success.
- `a_structured_error_frame_after_partial_output_fails_with_its_detail` — delta then `{"error":{"message":"Model overloaded"}}` → `LlmHttp` quoting it, delta kept; **why**: the OpenAI-style error object used to be ignored, letting half an answer end "successfully".
- `a_malformed_frame_fails_instead_of_silently_dropping_text` — a truncated JSON chunk fails "malformed", earlier delta kept; **why**: the unparseable chunk may have carried answer text.
- `usage_only_and_unknown_frames_are_harmless` — a `choices: []` usage frame and an `x_groq` frame are ignored and the answer completes; **why**: trailing metadata frames must not break answers.
- `an_oversized_frame_is_refused_while_reading` — a 1.1 MiB line on a stalled socket fails "oversized" within 20 s; **why**: the cap holds while reading.
- `an_endless_error_body_is_read_only_up_to_the_cap` — a 400 promising 10 MB that sends 64 KiB and stalls returns a <400-char "HTTP 400" message within 20 s; **why**: the snippet path used to read the whole body first.

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

### `core/src/stt/local.rs` (3 tests, `#[tokio::test]` against a scripted tokio-tungstenite server on `127.0.0.1`; the free-mode speech client that dials `ws://127.0.0.1:8765/transcribe`)

- `early_stop_flushes_audio_and_concurrent_finalize_joins_once` — audio sent before the server's `ready` frame reaches the wire as binary LE i16, then `{"type":"finish"}` follows exactly once even though two `finalize()` calls run concurrently; both return the `done` text ("Hello world"), the interim `transcript` reached the sink, and no error was reported; **why**: the same three guarantees Deepgram has — pre-open audio is the user's question, a double-tapped Stop must not race two finishes for one socket, and the text released at close must be the returned transcript.
- `disconnect_does_not_succeed_with_partial_text` — a server that closes the socket without a `done` frame makes `finalize()` fail and reports exactly one error; **why**: handing back whatever was heard as if it were the final transcript answers a truncated question — the user must learn the local service dropped the line.
- `abort_ends_pending_connection_without_error_event` — `abort()` before the handshake completes makes `finalize()` return `aborted` with zero error events; **why**: abort is the caller tearing down on purpose (§5.1/§5.10) — reporting the death we caused would blame the local service.

## Rust core — session

### `core/src/session/mod.rs` (7 tests)

- `non_streaming_answer_reports_total_not_zero_for_first_token` — `Metrics::finish` with no first-token sample uses total; **why**: "0.0s to first word" would be a lie about the headline metric.
- `streaming_answer_keeps_its_measured_first_token` — a measured first-token value survives; **why**: the real measurement must not be overwritten by the fallback.
- `typed_questions_report_zero_stt_time` — `stt_finalize_ms` can be 0; **why**: typed questions have no STT stage; 0 is the only honest value.
- `every_event_carries_its_session_id_and_wire_name` — all five `SessionEvent` variants (`LlmDone` now with `stop_reason`) expose the right id and event name; **why**: routing and staleness filtering key on exactly these.
- `metrics_serialize_as_camel_case_for_the_frontend` — `sttFinalizeMs`/`firstTokenMs`/`totalMs` on the wire; **why**: the frontend destructures these exact keys.
- `answer_limits_presets_mirror_the_pinned_constants` — `AnswerLimits::CLOUD` equals `limits::LLM_FIRST_TOKEN`/`LLM_TOTAL`, `AnswerLimits::LOCAL` equals `LOCAL_FIRST_TOKEN`/`LOCAL_TOTAL`, and LOCAL is strictly longer on both axes; **why**: the presets are the only route the deadlines take into the machine — if one drifted from `limits::*`, the numbers SPEC §3 promises and the numbers enforced would diverge without any test noticing, and a "longer" local pair that is shorter is a copy-paste error.
- `session_outcomes_have_the_wire_shapes_the_frontend_switches_on` — `SessionStart` is `{ sessionId, outcome: { status: "active" } }`; `completed` carries `transcript/answer/metrics/stopReason`, `failed` carries `error/transcript/partial`, `cancelled`/`unknown` are bare; `is_terminal` is true exactly for completed/failed/cancelled; **why** (R1): `src/types.ts` mirrors these by hand — a renamed tag or field would make every adoption-time reconciliation fall through to "unknown".

### `core/src/session/machine.rs` (57 tests, all `#[tokio::test(start_paused = true)]` against scripted STT/LLM fakes; the fake stream counts aborts, and R1 added scripts for stop reasons, fail-after-deltas, panics and a spurious `aborted`)

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
- `local_limits_keep_a_slow_first_token_alive_past_the_cloud_cap` — with the fake reporting `AnswerLimits::LOCAL`, a first delta at 60 s passes the 10 s cloud mark with no error and lands with `first_token_ms == 60_000`; **why**: the local model can spend a minute ingesting the prompt on the CPU — on the cloud deadlines every local answer died at 10 s with "did not start answering in time".
- `local_limits_still_cap_the_first_token_at_ninety_seconds` — on the local limits a stream with nothing by 89 s is still fine, 90 s yields one `LlmFirstTokenTimeout`, and a detached delta at 100 s paints nothing; **why**: "longer" is not "forever" — a wedged Ollama must surface as a structured timeout on the local number, and late deltas stay suppressed exactly as on the cloud path (§5.9).
- `local_limits_extend_the_total_deadline_to_five_minutes` — a stream that starts at 1 s and finishes at 201 s survives the 60 s cloud total and completes with `total_ms == 201_000`; **why**: a CPU stream trickles — cutting it at the cloud's 60 s truncated real local answers mid-sentence.
- `cloud_limits_stay_the_default_when_a_provider_says_nothing` — a fake that never called `use_limits` (like a provider that does not override `answer_limits`) hits `LlmFirstTokenTimeout` at exactly `limits::LLM_FIRST_TOKEN`; **why**: the machine must read the deadlines off the provider — a default that silently inherited the local pair would let a wedged cloud request hang the UI for 90 s.
- `metrics_are_measured_from_the_stop_instant` — 3 s of recording contributes nothing; finalize/first-token/total measure from stop; **why**: recording time is the user talking, not us working — counting it makes a long question look like a slow answer.
- `non_streaming_answer_reports_total_as_first_token` — a one-shot answer reports first-token = total, no deltas; **why**: 0 would render "instant" for a provider that streamed nothing.
- `prewarm_fires_on_start_and_stop_not_ask` (renamed from `prewarm_fires_on_start_stop_and_ask`) — prewarm counts 1 at start and 2 at stop (asserted BEFORE the finalize resolves), and STAYS at 2 after a typed ask; **why**: the stop-time warm overlapping the STT flush is what buys the ~1 s stop-to-first-word — dropping either is a cold TLS handshake on the critical path; ask is different (RS-1): its answer request fires microseconds after submission, so a warm there has nothing to overlap and only races the real request.
- `llm_provider_error_passes_through_and_releases_the_slot` — a provider `LlmHttp` surfaces unwrapped and the next ask works; **why**: provider errors carry their own closed-set codes; re-wrapping breaks the UI's error routing.
- `an_immediate_connect_failure_is_recorded_before_anyone_adopts_the_id` — a failed connect leaves `is_active == false` and `outcome == Failed { stt_connect, "", "" }`, with still exactly one error event; **why** (R1): the shell used to return `Ok(id)` for a session that had already failed during the device open, and the UI dropped the only error event.
- `an_instant_answer_is_recorded_as_completed_never_as_an_error` — an instant typed answer's outcome equals the `done` event's transcript/answer/metrics with `Complete`; **why**: a stored error alone cannot recover an early SUCCESS.
- `a_failed_answer_records_exactly_the_text_the_ui_was_shown` — deltas then a provider error → `Failed { partial: "Half an answer", transcript: "Question?" }`; **why**: an adoption that missed the stream must recover the same partial text, marked incomplete.
- `superseded_and_cancelled_sessions_are_recorded_as_cancelled_and_stay_silent` — a superseded id reads `Cancelled`, the winner `Active`, a cancelled id `Cancelled`, an unissued id `Unknown`; each old stream was aborted exactly once; no errors; **why**: an older lookup must never read as a failure, and teardown plus the driver's cancel arm both decide to abort — the socket must die once.
- `outcomes_are_kept_per_id_and_retired_oldest_first` — 17 completed asks: the first reads `Unknown`, the other 16 `Completed`; **why**: a single global "last outcome" could be overwritten before an older adoption read it; the log is keyed by id and bounded.
- `a_terminal_outcome_is_never_rewritten_by_a_later_path` — a late `device_error` and `cancel` for a completed id leave its outcome and emit nothing; **why**: settle-once — a delivered answer must not turn into a failure on the next lookup.
- `token_limit_stop_reaches_done_and_the_recorded_outcome` — a `TokenLimit` answer reaches `llm:done` and the outcome with that reason; **why** (R2): "cut short" must survive both the live and the lookup path.
- `a_panicking_provider_settles_the_session_once_and_frees_the_slot` — a provider that streams "Hal" then panics yields exactly one `internal` `MSG_DRIVER_STOPPED` error, `Failed { partial: "Hal" }`, a free slot for the next start, and no extra abort of the already-finalized stream; **why** (R1): a panic used to leave `Active` in the slot — a hang the user could only escape by pressing Record again.
- `a_panic_while_the_stream_is_open_aborts_it_exactly_once` — a connector whose finalize panics has its stream aborted once and the session fails `internal`; **why**: the guard owns the live socket until finalize closes it.
- `a_provider_returning_aborted_on_its_own_cannot_hang_the_session` — a spurious `aborted` from the provider is settled by the guard as `MSG_DRIVER_STOPPED`; **why**: `aborted` is silent because it normally means WE cancelled; returned spontaneously it used to exit the driver with the slot still owned.
- `a_sink_panic_mid_answer_settles_the_session_without_aborting_the_process` — a sink that panics on the first delta: the session still gets exactly one `internal` `MSG_DRIVER_STOPPED` error (emitted from a fresh task), is released, and records `Failed` with an EMPTY partial because that delta never reached the UI; **why** (review F1): the guard runs mid-unwind, and an inline emit through a sink that panics again is a panic in a destructor during unwinding, which aborts the process.
- `a_sink_that_panics_on_every_emit_cannot_abort_the_process` — every emit panics, including the error; the test binary survives and the outcome is `Failed`; **why**: with the old inline emit this variant aborted the whole binary — reaching the assertions is the proof.

## Rust core — store

### `core/src/store/bounds.rs` (22 tests: 16 sanitizer + 6 pure dock-math)

- `no_saved_bounds_falls_back_to_defaults_centred` — `None` → the default size and no position; **why**: first run must open at the default size with nothing to restore — the shell then docks it under the camera (§8), centring only if docking fails.
- `fallback_is_not_from_saved` — the fallback, `None`, a NaN coordinate and a zero size all yield `from_saved == false`; **why**: the fallback size is the builder's LOGICAL 460×700 — flagging it as saved would make the shell re-apply it as physical pixels and open a shrunken window on every hi-DPI display (RS-7).
- `saved_bounds_are_from_saved` — a valid save, a save whose position was unprovable, and a clamped-up tiny save all report `from_saved == true`; **why**: whenever the size came from the file it is physical and safe to apply — "the position was dropped" must not be confused with "there was no file".
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
- `dock_centres_horizontally_and_hugs_the_top_margin` — a 600 px window on a 1920 px work area docks at `(660, 8)`; **why**: the eye-level rule in one number each — horizontal centre, `DOCK_TOP_MARGIN` from the top.
- `odd_widths_and_odd_work_areas_floor_the_half_pixel` — 601 px wide, or a 1919 px work area, both give x = 659; **why**: integer division must be stable — the odd pixel of slack goes to the right, never alternating between two rounds on repeated docks.
- `dock_follows_a_monitor_left_of_primary` — a work area at x = −1920 docks at x = −1260; **why**: negative origins are real (see the sanitizer's left-monitor case) — the i64 math must not clamp them onto the primary.
- `a_top_docked_taskbar_pushes_the_dock_below_it` — a work area starting at y = 40 docks at y = 48; **why**: the work area excludes the taskbar, so a top taskbar shows up as y > 0 and the window must land under it, not behind it.
- `a_window_wider_than_the_display_aligns_to_its_left_edge` — a 600 px window on a 500 px work area at x = 100 docks at x = 100; **why**: negative slack would centre the window off both edges and put the title bar out of reach — hugging the left edge keeps it grabbable.
- `dock_preset_size_scales_with_dpi_and_clamps_to_the_display` — 100 % on 1920×1040 → 600×520 (45 % = 468 is under the floor); 150 % on 3840×2100 → 900×945; a tall display gets the unclamped fraction; width never exceeds the display; height never exceeds the area under the margin; NaN/0/negative/∞ scale reads as 100 %; **why**: the preset must read comfortably at any DPI and never produce a window wider or taller than the screen — and a garbage scale factor must never become a 0×0 window.

### `core/src/store/settings.rs` (59 tests; `a_locked_file_is_unreadable_not_corrupt_and_is_never_overwritten` is `#[cfg(windows)]`)

- `first_run_loads_defaults` — empty dir → `Settings::default()`, default hotkey, always-on-top true; **why**: first run must be fully defined, not partially null.
- `first_run_loads_one_empty_default_profile` — no file at all → exactly one `Default` interview profile, active `"default"`, `Camera` placement, `Tail` follow; **why**: the user gets a profile to fill in — never an empty profile list the answer path would have to special-case.
- `a_fully_valid_file_loads_every_field` — a complete §2-shape file (two profiles with the SECOND active, every enum at its non-default value, all three keys, bounds) loads whole-struct equal; **why**: baseline the corruption tests degrade from — every fallback below is observable only because the good file is non-default everywhere.
- `patch_round_trips_through_disk` — a full-form patch (carrying `expected_revision: 1`, the revision of a fresh store) plus saved bounds reload identically from a fresh store; **why**: that is what persistence means here.
- `unparseable_json_is_quarantined_then_loads_as_defaults` — a syntactically broken file is renamed to `settings.json.corrupt-1000` (injected clock) with its bytes intact, `settings.json` is gone, the store holds defaults, `storageWarning` names the copy, a save then works, clears the warning and never touches the backup; **why**: a broken file must not crash the app, and replacing it with defaults without a copy destroyed the only record of the user's profiles and keys (FINAL-REVIEW §5).
- `non_object_json_is_quarantined_then_loads_as_defaults` — arrays/strings/numbers/bools/null and an empty file each load as defaults with exactly one backup holding the original text and a warning; **why**: same, for the shape being wrong rather than the syntax.
- `invalid_utf8_is_damaged_content_not_an_io_error` — bytes that are not UTF-8 are quarantined byte for byte and saving works; **why**: `read_to_string` would report this as an I/O-shaped `InvalidData` error, and I/O errors are protected rather than quarantined — the bytes were read fine and are simply not settings.
- `a_missing_file_is_a_clean_first_run_with_no_backup_and_no_warning` — no file → no warning, no backup, saving works; **why**: a first run is not damage, and must not greet the user with a warning.
- `quarantine_never_overwrites_an_existing_backup` — with `settings.json.corrupt-1000` already present, the new copy lands at `…-1000-1` and the older backup is unchanged; **why**: Windows `rename` silently replaces a file, so a second damaged file in the same second would otherwise destroy the first backup.
- `a_failed_quarantine_leaves_the_original_untouched_and_blocks_every_write` — all ten backup names taken → defaults in memory, a warning, `apply_patch` fails with that warning as an `Internal` error, `save_window_bounds` does nothing, the damaged file is byte-identical afterwards, and the revision stays 1; **why**: "do not overwrite the original when preservation fails" — including the silent geometry write that used to persist defaults on the first window move.
- `a_locked_file_is_unreadable_not_corrupt_and_is_never_overwritten` (Windows) — a good file held open with `share_mode(0)` (a sharing violation) is not quarantined, the store runs on defaults with a "could not be read" warning, patches and geometry saves are refused, and once the lock is released the file is unchanged and loads whole; **why**: permission, sharing and transient errors say nothing about the content — treating them as corruption would replace a perfectly good file with defaults.
- `each_corrupt_field_falls_back_alone` — for 10 top-level fields (`profiles`, `activeProfileId`, `alwaysOnTop`, `llmProvider`, `answerStyle`, `hotkey`, `launchPlacement`, `streamFollow`, `deepgramKey`, `windowBounds`), corrupting one defaults exactly that field (whole-struct equality; a corrupt `profiles` VALUE becomes one empty Default profile, a non-string active id falls back to the first profile); **why**: the core promise of per-field validation — one bad value must not cost the rest.
- `corrupt_answer_style_never_costs_the_resume_or_the_keys` — a bad enum string keeps both profiles' text (the active one's resume included) and all three keys; **why**: the named disaster from the spec — one bad enum must not read as "corrupt file" and wipe everything.
- `unknown_launch_placement_falls_back_to_camera` — `"sideways"` reads as `Camera`; the active profile, keys and `streamFollow` survive; **why**: a hand-edited or downgraded value must not read as "corrupt file".
- `unknown_stream_follow_falls_back_to_tail` — `"middle"` reads as `Tail`; profile, keys and `launchPlacement` survive; **why**: same rule for the other new enum.
- `launch_placement_round_trips_through_disk` — `Remembered` then `Camera` through `apply_patch` land in memory, reload from disk, and are written as the lowercase wire strings; **why**: the placement is read at the next launch by a fresh store — memory, disk and the on-disk spelling must agree.
- `stream_follow_round_trips_through_disk` — `Top` then `Tail`, same three checks; **why**: same.
- `v3_flat_file_migrates_into_one_default_interview_profile` — a file with top-level `resume`/`jobDescription` and no `profiles` loads as one `{ id:"default", name:"Default", callType:interview }` profile carrying exactly that text, active `"default"`, keys/hotkey/bounds untouched, and the absent new fields at their defaults; **why**: the upgrade must load as exactly the prompt the user had — a lost resume on first launch after the update is the worst first impression an upgrade can make.
- `migrated_file_is_rewritten_in_the_new_shape_only` — after the first save of a migrated file the raw JSON has `profiles` (with the migrated text) and `activeProfileId`, NO top-level `resume`/`jobDescription`, the new enums written, the key kept, and it loads back equal to memory; **why**: the legacy shape is read once and never written — two sources of truth for the resume would drift the moment one is edited.
- `profiles_round_trip_through_disk` — five profiles (one per call type, whitespace-edged resume) plus an active id patch reload from a fresh store equal to memory, verbatim, and the view returned by the patch equals a fresh load's view (at the same revision — revisions are per run); **why**: that is what persistence means here — and the view the UI re-seeds from must be the view the next launch produces.
- `each_corrupt_profile_field_falls_back_alone` — for 7 fields INSIDE a profile (`callType`→interview, `resume`/`jobDescription`/`focus`/`extraInstructions`→`""`, `name`→`"Untitled"`, non-string `id`→repaired `p1`), corrupting one costs only that value; the profile stays active and the keys survive; **why**: per-field fallback applies inside a profile too — one bad value must never cost the profile, its neighbours, or the file.
- `non_object_profile_entries_are_dropped_and_the_rest_survive` — numbers, strings, `null` and arrays inside `profiles` are dropped and the object entries load as if the junk were not there (active id and keys intact); **why**: there is nothing in a number to salvage as a profile, and one junk entry must not discard the real ones.
- `corrupt_profiles_value_loads_as_default_profile_without_losing_keys` — a `profiles` that is a number, string, object, `null` or bool yields one empty Default profile while the hotkey, all three keys and the bounds survive; **why**: "corrupt file → wiped keys" must be impossible through this field as through every other.
- `empty_profiles_array_yields_a_default_profile` — `[]` on disk (with an unknown active id) and `Some(vec![])` in a patch both become the Default profile; **why**: the form cannot delete the last profile, but the core must not rely on the form — the invariant "never empty" is the store's.
- `more_than_max_profiles_are_truncated` — 12 profiles on disk keep the first 8 in order and an active id that fell off the end falls back to the first; 12 in a patch keep 8 and honour a surviving active id; **why**: the cap is enforced on both paths, and truncation must never leave `activeProfileId` pointing at a profile that no longer exists.
- `duplicate_or_invalid_ids_are_repaired_deterministically` — `""`, a duplicate, a space, an over-long id and `café` become the smallest unused `p<n>` in list order (a 40-char id survives), the same input yields the same output twice, and the repair the store returns to the UI equals what the next launch loads; **why**: ids are DOM ids and JSON keys downstream, and a random or clock-based repair would make the cached prefix nondeterministic through the store (ADR 007).
- `normalize_is_idempotent` — running `normalize_profiles` over its own output changes nothing (names trimmed once, resume kept verbatim, blank name → Untitled); **why**: a bare active-id switch re-runs normalize over the stored list — if a second pass could change anything, a switch would rewrite profile text.
- `unknown_active_id_falls_back_to_first_on_load` — `"vanished"` on disk loads as the first profile with nothing else moved; **why**: a deleted-then-referenced id must still start the app with a grounded profile.
- `unknown_active_id_keeps_current_active_on_patch` — a switch patch naming `"zzz"` leaves the active id unchanged in the view, in memory and on disk; **why**: a stale switch (the chip row raced a delete) must not jump to the first profile — the returned view tells the UI the truth.
- `switch_patch_changes_only_active_profile_id` — a patch carrying only `activeProfileId` changes the active id and not one byte of profile text, key or hotkey (compared decrypted, since DPAPI randomizes ciphertext); a later switch to an unknown id keeps the current one; **why**: §8 "a switch never rewrites text" — the profile switch is a cache write for the prompt, not a settings edit.
- `replacing_away_the_active_profile_falls_back_unless_patch_names_a_new_active` — a whole-array replace that drops the active profile without naming a new one falls back to the first; one that names a surviving id gets it; one that keeps the previous active keeps it; **why**: the form deletes profiles by replacing the array — the core must pick an active that exists without the form spelling it out every time.
- `per_profile_caps_apply_on_load_and_on_patch` — over-long `é` name/resume/JD/focus/extra are cut to `MAX_PROFILE_NAME_CHARS`/`MAX_PROFILE_CHARS`/`MAX_FOCUS_CHARS`/`MAX_EXTRA_INSTRUCTIONS_CHARS` on load and on patch (view, memory, reload), with no split character; **why**: characters, not bytes — a byte cut would poison the file for every later load.
- `profile_name_is_trimmed_and_blank_becomes_untitled` — `"  Rust  "` → `"Rust"`, whitespace-only and empty names → `"Untitled"`; **why**: a name is a chip label — padding and blanks would render an invisible button.
- `over_length_resume_is_truncated_on_load_and_on_patch` — the `MAX_PROFILE_CHARS` cap applies to a legacy flat file on load and to a profiles-shape patch; **why**: an unbounded resume bloats every prompt and the settings file.
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
- `revision_starts_at_one_and_advances_once_per_committed_patch` — a fresh store is at 1; each committed patch returns the next revision, and the store's view equals the returned view; **why**: the revision is the order the UI applies views in (ADR 016).
- `a_stale_form_is_rejected_with_settings_conflict_and_changes_nothing` — after a chip commit, a form patch carrying the older `expected_revision` fails with `SettingsConflict` and `MSG_SETTINGS_CONFLICT`, and memory, revision and the file on disk are unchanged; the same form with the current revision then commits and keeps the chip's change; **why**: a form seeded from an old view must not overwrite a newer commit with its whole `profiles` array (R5).
- `single_field_patches_without_a_revision_merge_whatever_order_they_commit_in` — a style patch and a profile switch without revisions, in either order, both land on disk and the revision is 3; **why**: the chips send distinct fields with no revision and must keep working concurrently.
- `a_failed_write_never_advances_the_revision` — with the tmp path blocked after a healthy load, a form patch fails with `Internal`, the revision and memory stay put, and the same form commits once the disk recovers; **why**: FINAL-REVIEW §3.1 — a revision that advanced on a failed write would make the next, valid save of the same form look stale.
- `geometry_saves_never_advance_the_revision` — two bounds saves leave the revision unchanged, a form seeded before them still commits, and the form's save keeps the latest bounds; **why**: moving the window must never invalidate an open Settings form.
- `racing_full_form_saves_from_one_revision_commit_exactly_once` — eight threads submit forms seeded from the same revision: exactly one commits, the rest get `SettingsConflict`, the revision advanced once and the disk holds the winner; **why**: the compare and the commit share one lock — a check-then-act gap would let two stale forms both win.

### `core/src/store/effects.rs` (10 tests; a recording fake `OsEffects`, real `SettingsStore` in a tempdir; the R5 acceptance tests for OS effects)

- `a_fresh_reconciler_applies_everything_once_then_nothing` — an unseeded reconciler registers the hotkey, sets always-on-top and docks once; the same desired state again is a no-op; **why**: the effect step runs after every save, so it must be idempotent.
- `a_seeded_reconciler_applies_only_what_changed` — seeded with the startup state, an unchanged state does nothing and a flag change sets only the flag; **why**: startup applies the OS state before the window shows; the first save must not re-register a hotkey it did not change.
- `a_refused_hotkey_is_retried_by_the_next_run_until_it_takes` — the fake OS refuses a combination "owned by another app": the run reports the attempt, nothing is registered, the next run tries again, and once the combination is free it registers and further runs are no-ops; **why**: FINAL-REVIEW §5 — a refused registration is a general, recoverable outcome; counting it as applied left the key dead until a restart even after the other app let go.
- `a_startup_hotkey_the_os_refused_is_retried_by_the_first_save` — a reconciler seeded with a refused startup hotkey registers it on its first run; **why**: `lib.rs` seeds with what the OS accepted, not with what was asked.
- `docking_fires_on_the_transition_into_camera_only` — Remembered → Camera docks, staying on Camera does not, Remembered → Camera docks again; **why**: choosing "dock under the camera" demonstrates itself, but an unrelated later save must never yank a window the user has since moved.
- `effects_run_in_reverse_commit_order_still_leave_the_newest_hotkey` — commit hotkey X then Y, run the effect steps in reverse: only Y is registered, the late run is a no-op; **why**: the old per-patch before/after diff would register Y then X, leaving the OS disagreeing with disk.
- `a_later_style_only_commit_never_swallows_an_earlier_hotkey_change` — commit a hotkey change then a style change and run only the second commit's step: the hotkey is registered; **why**: "skip older effects" would leave the old combination live forever (FINAL-REVIEW §3.4).
- `a_failed_persist_leaves_disk_memory_and_os_agreeing` — a write failure after a committed hotkey: the save errors, the effect step finds nothing to do, the revision is unchanged, and disk, memory and the fake OS still agree; **why**: an effect applied for a save that never reached disk would make the OS lie about the settings.
- `a_stale_form_overlapping_chip_saves_converges_after_reload` — a form seeded before a chip commit is refused with `SettingsConflict`, the chip's delayed effect step does nothing, the reloaded form (typed resume kept, chip's style taken) commits a hotkey and flag change, one step applies both, and disk, memory and OS agree; **why**: the R5 acceptance case — a full form overlapping chip saves.
- `concurrent_commits_and_effect_runs_converge_in_any_interleaving` — 20 rounds of four threads released by a barrier, each committing (two hotkey/flag patches, two style patches) and then running the effect step: the revision is 5 and disk, memory and OS agree every round with no extra step; **why**: the run that takes the lock last reads a state at least as new as every commit — the property that replaces ordering by construction.

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

### `core/src/store/mod.rs` (8 tests; the types and wire shapes — the store logic lives in `settings.rs`)

- `active_profile_resolves_by_id_and_falls_back_to_first` — the named profile, else the first, else the static `EMPTY_PROFILE` (interview, all fields empty) when the list is empty; **why**: a hand-built `Settings` that breaks the invariant must still answer with SOMETHING grounded rather than panic on the answer path.
- `as_prompt_borrows_every_field` — `CallProfile::as_prompt()` carries call type, resume, JD, focus and extra, and the strings are the profile's own bytes (`ptr::eq`); **why**: the prompt is rebuilt per session — copying 200 000-char fields for every build would be pure waste, and a field the view forgot would silently vanish from the prompt.
- `default_settings_hold_one_empty_default_profile_docked_and_tailing` — `Settings::default()` is one empty `Default` profile, active `"default"`, `Camera`, `Tail`; **why**: the defaults are the first-run experience and must match §2.
- `launch_placement_and_stream_follow_parse_or_default` — the lowercase names parse; `""`, unknown and wrong-case values read as `Camera`/`Tail`; `as_str()` and the serde form round-trip; **why**: the settings file is untrusted input (§8), and a value that survives save/load must be the same value.
- `typed_enum_patch_fields_reject_unknown_wire_values` — `SettingsPatch` fails to deserialize for `llmProvider:"bogus"`, `answerStyle:"verbose"`, `launchPlacement:"sideways"`, `streamFollow:"middle"` and a numeric provider, and accepts the valid set; **why**: §4 — the wire is typed: a value the TS enum cannot produce fails the invoke instead of being silently coerced to a default the user did not choose (`parse_or_default` is for the FILE only).
- `a_partial_profile_object_never_fails_the_patch` — `CallProfilePatch` entries with missing fields default to `""`, an unknown `callType` reads as interview, a bare `{ activeProfileId }` patch carries no profiles, and `"sales"` parses; **why**: a partial object in the array must never fail the whole patch — the lenient profile form is what lets a switch travel alone.
- `settings_view_serializes_the_wire_shape` — the serialized `SettingsView` equals the exact JSON pinned against `src/types.ts` (camelCase keys, lowercase enums, `has*Key` booleans, `revision`, a null `storageWarning`); **why**: the frontend destructures these exact keys — a serde rename is a silent break across the IPC boundary.
- `expected_revision_travels_on_the_patch_wire_and_is_optional` — `expectedRevision: 4` deserializes to `Some(4)`, a chip patch without it to `None`, and a string revision fails; **why**: the full form and the chips share one patch type — the chips must keep working without a revision, and a wrong-typed one must fail the invoke rather than skip the check.

### `core/tests/local_settings.rs` (1 integration test, run by the same `cargo test -p app-core`)

- `local_mode_round_trips_without_requiring_or_clearing_cloud_keys` — with cloud keys already stored, switching `llmProvider` to `local` (with a profile patch) persists, `active_llm_key()` is `None`, and a fresh load still finds the provider, the resume and BOTH stored keys; **why**: choosing free mode must not delete the keys the user paid to obtain — switching back to a cloud model must find them exactly where they were (§6.5).

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

### `src-tauri/src/commands.rs` (22 tests)

- `local_answers_need_no_cloud_keys` — `required_llm_key` fails for the default (Anthropic, no key) settings and returns `""` once the provider is `Local`; **why**: free mode must start with nothing stored — a key gate written as `== Local` instead of `needs_cloud_keys()` is exactly the kind of check that rots when a fourth provider appears.
- `cloud_providers_are_refused_without_their_own_key` — a stored Anthropic key satisfies Anthropic but Groq still fails with `NoLlmKey` and the "No Groq API key set…" message until a (trimmed) Groq key exists; **why**: each cloud provider is gated on ITS key — otherwise a Groq session starts and fails at the provider with a confusing 401 instead of the nudge to Settings.
- `deepgram_is_required_exactly_when_the_provider_transcribes_through_it` — no Deepgram key → `NoSttKey` with the pinned `MSG_NO_STT_KEY` for Anthropic and Groq; a padded key is trimmed; `Local` needs none; **why**: the cloud pair must fail BEFORE the socket opens, and local mode must record with no Deepgram key at all (§6.5).
- `the_prompt_is_built_from_the_active_profile_only` — `system_prompt_for` grounds the cached prefix in the active profile's text and nothing from the other; flipping `active_profile_id` swaps it entirely; the style suffix is unchanged by the switch; **why**: §8 — a profile switch is a cache write, not a per-question lookup, and text from an inactive profile leaking into the prompt would ground an answer in the wrong job.
- `ok_envelope_serializes_to_the_exact_wire_shape` — `{ ok: true, value }`; **why**: the frontend destructures these exact keys.
- `ok_envelope_with_unit_value_still_carries_the_value_key` — unit values serialize as `value: null`; **why**: `res.ok` / `res.value` must behave uniformly for stop/cancel.
- `err_envelope_serializes_code_and_message` — `{ ok: false, error: { code, message } }`; **why**: the error branch's exact shape is the UI's routing input.
- `err_envelope_never_carries_a_value_key_and_ok_never_an_error_key` — the two branches are disjoint; **why**: a stray key would make `in` checks lie.
- `start_envelopes_carry_the_id_and_the_outcome_so_far` — `{ ok: true, value: { sessionId, outcome: { status: "active" } } }`, and an already-failed session is still an ok envelope whose `outcome.status` is `failed` with its error code; **why** (R1): `start_session`/`ask` no longer resolve with a bare id — the UI destructures `value.sessionId` and settles the adopted attempt from `value.outcome`.
- `stop_not_taken_is_an_error_envelope` — NotTaken maps to an `internal` error envelope; **why**: the UI unsticks "Finalizing…" from the return value, not an event.
- `a_panicked_capture_open_is_an_internal_error_the_ui_can_show` — `capture_thread_failed()` is `Internal` with exactly "The audio capture thread failed to start. Try restarting the app."; **why**: RS-2 moved the WASAPI open onto the blocking pool — a `JoinError` there must reach the UI as an ordinary start failure (so the machine session is cancelled and the Record button recovers), never as a rejected invoke.
- `ask_text_is_trimmed` — surrounding whitespace is stripped; **why**: the trimmed text is what the LLM should answer.
- `empty_or_whitespace_ask_is_rejected` — blank asks fail validation; **why**: an empty prompt must be stopped at the gate.
- `ask_at_the_limit_passes_and_one_over_fails` — exact `MAX_ASK_CHARS` boundary; **why**: an off-by-one rejects the longest legal question.
- `ask_limit_counts_characters_not_bytes` — 8000 four-byte chars pass; **why**: a byte-counted check wrongly rejects multibyte questions well under the limit.
- `present_treats_blank_keys_as_missing` — `None`/`""`/whitespace read as no key, values are trimmed; **why**: a key of spaces passes `is_some()` and then fails at the provider with a confusing auth error.
- `a_refused_hotkey_does_not_count_as_applied_but_a_disabled_one_does` — `hotkey_took` is true for a registered accelerator and for an empty (disabled) one, false for a refused one; **why**: it decides what the effect reconciler records as applied, so a refused combination is retried by the next save (ADR 016).
- `only_the_local_provider_caps_input_bytes` — `caps_input_bytes()` is true for Local only; **why**: the pre-checks key off the capability, not `== Local`, like the key gates.
- `the_budget_command_previews_the_unsaved_draft_exactly_as_a_save_reads_it` — a draft with an unknown `callType` and an edge-padded resume budgets exactly like the equivalent interview `Profile` (trimmed bytes), status `ok`; **why**: the preview goes through `CallProfile::from`, the same lenient conversion a save uses, so it cannot read the draft differently from the store.
- `a_local_profile_with_no_room_is_refused_before_recording_with_the_gates_error` — a 7 000-byte resume with the local provider fails `local_budget_check(settings, "")` with exactly `local::oversize_error()`, and the same profile under Anthropic passes; **why**: R4 — the known overhead is checked before the device opens, not after the user has spoken, with the gate's verbatim message; cloud models have no byte cap.
- `a_typed_question_is_checked_at_its_real_size_before_any_session_starts` — a question exactly filling the remaining room passes, one byte more fails with the gate's error; **why**: typed Ask is validated at its actual size before a session is claimed, with no off-by-one against the gate.
- `the_pre_check_follows_the_active_profile_across_a_switch` — the same settings pass with the short profile active and fail once the long one is; **why**: the check uses the ACTIVE profile the answer would be grounded in, so a switch changes the verdict.

### `src-tauri/src/events.rs` (6 tests)

- `stt_partial_maps_to_its_name_and_bare_payload` — `stt:partial` + camelCase payload; **why**: the wire name/shape is the frontend contract.
- `llm_delta_maps_to_its_name_and_bare_payload` — `llm:delta` + `{ sessionId, delta }`; **why**: same contract per event.
- `llm_done_carries_camel_case_metrics_and_the_stop_reason` — `llm:done` with nested camelCase metrics and `stopReason: "token_limit"` (renamed from `llm_done_carries_camel_case_metrics` in R2); **why**: metrics keys are destructured verbatim by the UI, and the stop reason is what labels an answer "cut short".
- `session_error_carries_code_and_message` — `session:error` with the error object; **why**: error routing depends on the code reaching the UI intact.
- `audio_level_carries_rms` — `audio:level` + `{ sessionId, rms }`; **why**: the level meter's only input.
- `no_payload_ever_leaks_the_kind_tag` — the serde `kind` tag is stripped from every variant and `sessionId` is always present; **why**: the event NAME already carries the kind — leaking the tag silently changes every payload shape the UI destructures.

### `src-tauri/src/window.rs` (15 tests)

- `only_exclude_from_capture_counts_as_verified` — `capture_exclusion_verdict(Some(0x11))` passes; `0x00` (`WDA_NONE`), `0x01` (`WDA_MONITOR`) and a failed read (`None`) refuse with a message naming "Windows 10 version 2004"; **why**: `set_content_protected` cannot report a refusal (tao discards `SetWindowDisplayAffinity`'s result), so launch is gated on this read-back — and a black-rectangle `WDA_MONITOR` or an unreadable value is not the exclusion the app promises. The real window cannot be created under cargo, so the value mapping is what is tested.
- `latest_touch_owns_the_save_and_stale_tokens_get_nothing` — a superseded debounce token gets `None`, the newest gets the newest bounds, one-shot; **why**: saving the stale geometry would persist a position the user already moved past.
- `a_stale_wakeup_does_not_consume_the_pending_bounds` — a stale claim leaves the value for the rightful owner; **why**: a consuming stale wakeup would drop the save entirely.
- `close_flush_takes_pending_regardless_of_generation` — `take_pending` grabs the latest, once; **why**: the close-time flush must not be defeated by the debounce still counting down.
- `first_reload_is_allowed_immediately` — the first crash-reload passes; **why**: recovery must not wait out a gap that hasn't started.
- `reloads_inside_the_gap_are_denied` — reloads within the window are refused; **why**: a crash loop must not become a reload storm.
- `a_reload_after_the_gap_is_allowed_again` — the gap boundary reopens; **why**: recovery must eventually retry.
- `denied_attempts_do_not_push_the_window` — a storm of denials doesn't starve the retry at gap-end; **why**: sliding the window on denials would postpone recovery forever.
- `https_urls_are_allowed` — https (case-insensitive scheme) passes `is_safe_external_url`, the guard behind the window's `on_navigation` hook (`handle_navigation` → `open_in_browser`; the `open_external` command is gone, so this is the only way out of the app); **why**: RFC 3986 schemes are case-insensitive; a case-sensitive check breaks legitimate links.
- `non_https_schemes_are_rejected` — http/file/javascript/mailto/empty/bare `https://` are refused; **why**: the opener must never launch a non-web scheme from model-influenced content.
- `urls_that_could_split_an_argument_are_rejected` — spaces, quotes, newlines, tabs, control chars are refused; **why**: characters that split a shell argument turn "open URL" into "run something else".
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

### `src-tauri/src/local_voice.rs` (10 tests; R11 — the pure parsers and launch plan, with the probes/spawns kept in thin untested wrappers)

- `parse_tags_finds_the_model_under_either_key` — Ollama's `/api/tags` reports the model under `name` (older builds) or `model` (newer); both read as running + available; **why**: a build that only checked one key would tell the user to reinstall a model they have.
- `parse_tags_distinguishes_running_without_our_model_from_not_running` — other models or an empty list → `(true, false)`; an error object, a `models` that is not an array, a bare string or `null` → `(false, false)`; **why**: the two answers lead to different user actions ("finish the download" versus "start the service"), and a proxy page or error body must never read as Ollama running.
- `parse_health_needs_service_protocol_and_ready_together` — only `{ service: <ours>, protocol: 1, ready: true }` is healthy; the wrong service name, protocol 2, `ready: false`, stringly-typed `"1"`/`"true"`, `{}` and `null` are not; **why**: each field alone is a distinct wrong answer — another local server on the port, a protocol this build cannot speak, models still loading — and none may count as ready.
- `the_health_probe_targets_the_port_the_core_dials` — the core's `stt::local::URL` contains `127.0.0.1:<SPEECH_PORT>/` and the health URL is `http://127.0.0.1:<SPEECH_PORT>/health`; **why**: the shell probes `/health` and the core opens `/transcribe` — if the ports ever diverged the panel would report "ready" for a service the recording cannot reach (the shell may not touch `core/`, so the pin lives here).
- `parse_config_accepts_an_absolute_data_dir_with_or_without_a_bom` — `{ dataDir: <absolute> }` parses, with or without a leading U+FEFF; **why**: PowerShell's `Out-File` prepends a BOM that serde_json rejects — a setup that "worked" would then never be found.
- `parse_config_rejects_relative_paths_and_garbage_with_the_setup_error` — `"free-voice"`, `"./free-voice"`, `""`, a number, `{}`, non-JSON and empty input all fail with the pinned "Free voice is not installed yet. Run scripts\setup-free-voice.ps1 …" error; **why**: a relative `dataDir` would resolve `ollama.exe` against whatever the current directory is — the wrong binary, or a planted one.
- `launch_plan_starts_only_what_is_missing_ollama_first` — nothing running → `[Ollama, Speech]`; only speech missing → `[Speech]`; only Ollama missing → `[Ollama]`; all up → nothing; Ollama up but the model missing → nothing; **why**: a second `ollama serve` fails on the bound port and litters the log, and a missing model is not launchable — the download is the user's job and `prepare` reports it after the wait.
- `launch_specs_resolve_everything_under_the_setup_folder` — Ollama runs `<home>/ollama/ollama.exe serve` logging to `ollama.log`; speech runs `<home>/venv/Scripts/python.exe -u <home>/server.py --home <home>` logging to `speech.log` with an untouched environment; **why**: every path derives from the one configured folder — nothing on `PATH` is trusted, and the log names are what `prepare`'s error message tells the user to read.
- `ollama_is_pinned_to_loopback_with_no_cloud_and_local_models` — the Ollama spec's environment is exactly `OLLAMA_HOST=127.0.0.1:11434` (derived from the core's `ORIGIN`), `OLLAMA_NO_CLOUD=1`, `OLLAMA_MODELS=<home>/models/ollama`; **why**: the host the service binds must be the one the client dials, and free local mode must never fall back to a hosted model (§6.5).
- `local_voice_status_serializes_the_wire_shape` — `{ ollamaRunning, modelAvailable, speechReady }` in camelCase; **why**: pinned against `LocalVoiceStatus` in `src/types.ts` — the panel destructures these exact keys.

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

### `src/state/reducer.test.ts` (55 tests; ignored actions asserted by object identity)

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
- `llm:done finalizes the entry and returns to idle` — transcript/answer/metrics land, the entry reads `status: 'completed'`, ids clear; **why**: the terminal transition of the happy path.
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

**entry status (R2) (3)** — `HistoryEntry.status`/`reason`, set when the live entry settles

- `a token-capped done is kept as "limited" with its reason; a normal done is "completed"` — `stopReason: 'token_limit'` → `limited` + `LIMITED_REASON`, no error; `'complete'` → `completed`, reason null; **why**: a capped answer is not a failure, but it must not pass for a finished one either.
- `an interrupted answer stays marked incomplete after the next question clears the error` — delta then `session:error` → `incomplete` with the error message; the next `ask/start` clears the global error while the old entry keeps its status and the new one is `pending`; **why**: the whole point of per-entry status — the error box empties the moment the user asks again, but a half answer must still read as half an answer when they page back.
- `a superseded or aborted entry is marked cancelled, never incomplete` — `record/start` over a streaming entry → `cancelled` + `SUPERSEDED_REASON`; an `aborted` error → `cancelled` + `CANCELLED_REASON`, no banner; **why**: the user's own action is not a failure to flag.

**session/outcome reconciliation (R1) (6)** — the adoption-time action fed by the start envelope or the `session_outcome` lookup

- `a completed outcome settles the adopted attempt exactly like llm:done` — idle, ids cleared, text/metrics/status from the outcome; **why**: an answer that finished before adoption must look identical to one that streamed.
- `a failed outcome keeps the longer text, surfaces the error, marks the entry incomplete` — live "Half" + outcome partial "Half an answer" → the longer text, the outcome's transcript for an empty question, the error, `incomplete`; **why**: the lookup can know more of the text than the live stream delivered, never less.
- `cancelled settles silently; unknown settles with the lost-session error instead of hanging` — `cancelled` → idle, no error; `unknown` → idle with `LOST_SESSION_ERROR`; **why**: nothing will ever arrive for either — waiting is the R1 hang.
- `never settles another attempt or another id, and active changes nothing (identity)` — a stale key, a wrong id and `active` all return the SAME state; **why**: an older lookup must never settle a newer session.
- `is idempotent with the live terminal event: the second delivery is ignored` — after `llm:done`, a `failed` lookup and a repeated `llm:done` both return the same state; **why**: the live event and the lookup can both report the end; the first wins and the second must not rewrite it.
- `a failed outcome replaces a mid-text fragment with the full partial` — an entry showing only "middle" takes the outcome's "The middle part"; **why** (review F4): an entry that lost its first held deltas shows a fragment from the middle, which `startsWith` never matched.

### `src/state/useSession.test.ts` (53 tests; fake bridge wiring — its `on` returns `{ ready, unsubscribe }` with a per-test `listenReady`, and `sessionOutcome` defaults to `active`; `localPromptBudget` is present only because the `Bridge` type requires it — the hook never calls it)

- `idle -> starting -> recording -> finalizing -> answering -> idle` — the full round trip: bridge calls, event application, viewed entry with metrics; **why**: proves the WIRING between hook, bridge, and reducer — the moments calls are issued, not just the pure rules.
- `the hotkey event toggles exactly like the button` — `hotkey:toggle` drives the same transitions; **why**: two entry points, one behavior.

**hotkey gate (2)** — the `useSession({ hotkeyEnabled })` option that replaced App's bridge-wrapping gate (R4)

- `drops hotkey:toggle while hotkeyEnabled() is false and honors it once true` — with the closure returning false the event starts nothing; flipping the same closure to true (no re-render, no re-subscribe) makes the next event record; **why**: §9 — the shortcut is IGNORED while Settings is open (the user may be typing the accelerator itself), and the hook must consult the option per event so the caller's closure can flip without re-subscribing.
- `is enabled by default` — `useSession()` with no options records on the first `hotkey:toggle`; **why**: every caller that does not gate must still get the shortcut.
- `holds events arriving before the start call resolved, then replays only the adopted id's` — CONTRACT CHANGED in R1 (was `drops events arriving before the start call resolved (no buffering)`): nothing is dispatched before adoption, then the adopted id's partial is replayed while another id's partial and the audio level are discarded; **why**: dropping every pre-adoption event is how an early error or answer vanished (R1), while replaying any other id would let a superseded session write into the new one.
- `drops events carrying a different sessionId` — wrong-id partial/level/error do nothing; **why**: the staleness filter at the hook layer.
- `returns to idle silently and cancels the session if it starts anyway` — (waits for listener readiness first: an abort during the readiness wait never reaches `startSession`) abort-while-starting, then `cancelSession` on the orphan the core opened; **why**: the session that resolves after the abort must be cancelled, not leaked.
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
- `unmount unsubscribes every bridge listener` — 6 subscriptions before (five session events + the hotkey; R1's `session_outcome` is a command, not a listener), 0 after; **why**: leaked listeners apply events into an unmounted tree.

**delta coalescing (FE-1) (10; fake timers)** — the leading+trailing 16 ms `llm:delta` coalescer: the first delta of a burst dispatches synchronously, later deltas inside the window are merged per session and flushed by `setTimeout` (not rAF — WebView2 throttles rAF when minimized), and every non-delta handler flushes first so event ORDER is preserved

- `the first delta of a burst paints immediately` — one delta is visible synchronously, no timer needed; **why**: the leading edge is what keeps stop-to-first-word honest — a trailing-only coalescer would add 16 ms to the headline metric.
- `later deltas inside the window are merged and flushed by the timer, not rendered per token` — three deltas after the leading one cause zero renders until the 16 ms timer fires, then one render with `abcd`; **why**: a fast provider lands several tokens a millisecond, and each one used to be a reducer pass plus a render of the whole main view.
- `a steady stream renders once per window; a quiet window closes it` — a delta at 8 ms is flushed at 16 ms, one at 16 ms waits for the next window, and after a quiet window the next delta paints at once again; **why**: the window must roll while tokens keep arriving (bounded latency, bounded renders) and must close when they stop, or the first token of the next burst would wait.
- `a session:error after a buffered delta keeps the whole partial answer` — two deltas then an error inside one tick: idle, the error surfaced, and BOTH tokens in the retired entry; **why**: the flush runs before the teardown — a delta flushed after it would be dropped as stale and the reader would lose the tail they were reading.
- `llm:done after a buffered delta yields the full answer, and nothing fires late` — done after a buffered delta completes with the full text, and advancing the clock changes nothing; **why**: `done` must see the last token already applied, and the timer must be cleared so no stale flush lands on the finished entry.
- `a hotkey start after a buffered delta retires the old entry with its last token` — a `hotkey:toggle` with a token still buffered cancels session 1, retires its entry with `partial text`, and the new session adopts; **why**: supersession settles the streaming entry — a token still in the window when that happens would either vanish or land on the wrong entry.
- `a Record click after a buffered delta retires the old entry with its last token` — the same via `toggleRecord()` (the button path, which must drain the coalescer itself through the hoisted `flushRef` before `record/start` settles the entry), and nothing lands late; **why**: the button does not go through the event handlers, so without an explicit flush at the top of the supersede the buffered token was lost (FE-R1).
- `a typed ask after a buffered delta retires the old entry with its last token` — the same via `submitAsk`: session 1 cancelled, history `['q', 'second']`, the old entry ends with its last token, the new one starts empty; **why**: the third supersession path, same rule (FE-R1).
- `never merges a stale session's token into the live answer` — a zombie delta for session 1 buffered between two live deltas for session 2 is dropped by the reducer and the live text reads `live answer`; **why**: merging is per session id — concatenating across ids would paste a dead session's words into the live answer and defeat the §9 staleness rule.
- `unmount leaves no coalescing timer behind` — a buffered delta adds one timer; unmount removes it; **why**: a flush after unmount dispatches into a dead reducer and leaks the closure.

**adoption and reconciliation across the command/event boundary (R1) (14)** — the ADR 015 contract: readiness before start, held pre-adoption events replayed for the adopted id only, then one reconciliation

- `early deltas before adoption are shown exactly once, then live deltas append` — two deltas before `ask` resolves are invisible, appear once as "Early text" on adoption, live deltas append, done completes; **why**: pre-adoption text must be neither lost nor duplicated.
- `an immediate failure in the start envelope settles the attempt with its error and text` — `ask` resolves with `outcome: failed` (the local oversize case) → idle, error shown, entry `incomplete` with the message, no follow-up lookup; **why**: the reviewed reproduction left the UI "answering" forever.
- `an immediate success in the start envelope completes the answer, never an error` — `outcome: completed` → idle, answer + metrics, `completed`; **why**: an early success must not be turned into an error because the session is no longer active.
- `a delayed command response: the terminal event that raced ahead is replayed, not lost` — start → `session:error` for id 1 → invoke resolves → idle with the error; **why**: the exact R1 reproduction (the UI used to sit in "recording" with no error).
- `the adoption lookup settles a session whose terminal event never reached the UI` — envelope `active`, `sessionOutcome(12)` returns `completed` with `token_limit` → idle, `limited`; **why**: the residual race between the envelope and adoption.
- `duplicate terminal delivery (event, repeat, and lookup) settles exactly once` — two `llm:done` plus a late `failed` lookup → one entry, `completed`, no error; **why**: reconciliation must be idempotent with the live events.
- `an older lookup never settles the newer attempt` — the first ask's lookup resolves `failed` after a second ask was adopted → the second stays `answering`, no error; **why**: the attempt/id guard.
- `cancellation before adoption: held events are discarded and the orphan is cancelled once` — abort while starting with a held partial → `cancelSession(8)` exactly once, no entry, no lookup; **why**: capture/streams released exactly once and no stale text resurrected.
- `rapid supersession: the first attempt is cancelled and its early text never reaches the second` — ask A pending with a held "stale" delta, ask B adopted, then A resolves → A cancelled once, B's entry holds only its own text; **why**: a held event of a superseded attempt must never leak into the winner.
- `a delayed listener registration holds the start until the subscriptions are live` — with `ready` pending, Record shows "starting" and `startSession` is not called until `ready` resolves; **why**: an event emitted before its listener exists is never delivered.
- `a failed listener registration is a visible start error, never a silent hang, and is retried` — an `llm:done` registration failure fails the ask with that error and never calls `ask`; the next attempt re-subscribes and goes out; **why**: `bridge.on` used to swallow the failure and the UI waited forever.
- `a failed hotkey registration is shown but does not block Record` — the hotkey's failure is set as the error, recording still starts; **why**: the shortcut is not part of the start contract, but a dead shortcut must not be silent.
- `a listener registration that never settles fails the start after the timeout, and a later attempt retries` — fake timers: with `ready` hanging, Record sits in "starting", then after `LISTEN_TIMEOUT_MS` (3 s) returns to idle with `LISTEN_TIMEOUT_ERROR` and no `startSession`; once registration works the next Record re-subscribes and starts; **why** (review F2): a hung `listen()` used to leave the app unable to start a session, silently, until restart.
- `a held-buffer overflow evicts other sessions' events first, keeping the adopted id's early text` — one adopted-id delta then 70 non-mergeable older-id partials: the adopted entry still shows "Early"; **why** (review F4): evicting the oldest event could drop the adopted session's first delta and show a fragment.

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

### `src/state/bridge.test.ts` (11 tests; `@tauri-apps/api` mocked)

- `converts a rejected invoke into an internal-code error envelope` — a thrown Error becomes `{ ok:false, error:{ code:'internal' } }`; **why**: the bridge's whole job is one error path — the UI never catches rejections.
- `stringifies non-Error throwables so the message is never "[object Object]" surprise-free` — a string rejection carries its text; **why**: unreadable error text is unactionable.
- `passes a resolved envelope through untouched` — ok envelopes pass verbatim; **why**: the bridge must not rewrap success.
- `never rejects: every command method resolves even when invoke throws` — all eleven envelope-returning methods (`getSettings`, `setSettings`, `startSession`, `stopSession`, `ask`, `sessionOutcome`, `hotkeyStatus`, `dockToCamera`, `localVoiceStatus`, `prepareLocalVoice`, `localPromptBudget`) resolve with `ok:false`; **why**: one leaked rejection crashes the calling flow — and this is the ONLY place a malformed `set_settings` patch (a typed-enum field the core rejects at deserialization) is turned into an `internal` envelope.
- `uses the snake_case command names and argument shapes the core registered` — every command's exact name and args: `set_settings` wraps the patch as `{ patch }` (a bare `{ activeProfileId }` switch travels as-is), `stop_session`/`cancel_session`/`session_outcome` take `sessionId` (not `id`), `local_prompt_budget` takes `{ profile, answerStyle, question }` (the Rust `answer_style` camelCased), and `hotkey_status`, `dock_to_camera`, `local_voice_status`, `prepare_local_voice` take no args; **why**: Tauri camelCases the Rust `session_id` parameter — the wrong key fails every stop with a missing-key error, and a misspelt command name is a rejected invoke the user sees as "internal".
- `cancelSession swallows a rejection (cancel races teardown by design)` — no unhandled rejection; **why**: cancel legitimately races session teardown; a leaked rejection would fail the run.
- `delivers listened payloads to the handler and unlistens on unsubscribe` — `ready` resolves ok, payload delivery plus real unlisten; **why**: the event plumbing both directions.
- `unsubscribing before listen resolves still detaches` — `unsubscribe()` before the async registration completes still unlistens; **why**: the unmount-during-registration race leaks a listener otherwise.
- `a failed registration surfaces as an error envelope instead of being swallowed` — a rejected `listen()` makes `ready` an `internal` error naming the event and the cause; unsubscribing it is harmless; **why** (R1): the bridge used to `.catch(() => undefined)`, so sessions started with nobody listening.
- `ready stays pending until the registration actually completes` — `ready` does not settle before `listen()` resolves; **why**: readiness is what the start waits on — resolving early would reopen the race.
- `setBridge replaces what getBridge returns` — the test seam works; **why**: every other frontend test depends on this swap.

## Frontend — views

### `src/views/App.test.tsx` (22 tests; real `useSession` + real views over a `FakeBridge`; `renderApp()` waits up to 10 s for the gear to enable because a contended worker can take longer than the default second)

- `warms the lazy chunks 1.5 s after mount, and an unmount cancels the warm-up` (real timers on purpose) — synchronously after mount the PracticeLibrary chunk has not loaded; an unmount before 1.5 s means it never loads; a fresh mount loads it within the wait; **why**: FE-3 — the chunks must warm in the background so the first click on Interview prep or Settings does not pay a cold compile, and the timer must die with the tree or a torn-down App loads code into nothing.
- `walks the full pipeline against real state` — Record → partial → Stop → deltas → done, with status texts, live tag, and the latency chip carrying the metrics tooltip; **why**: the one test where every real layer (views, hook, reducer, bridge seam) runs together.
- `copies the markdown source and confirms` — Copy writes the SOURCE (`- first\n- second`), not rendered text, and shows "Copied ✓"; **why**: pasting into a doc must reproduce the markdown, not flattened list text.
- `surfaces a session error, and aborted renders nothing` — aborted → no alert; a real code → alert with the message; **why**: the aborted-is-silent contract at the full-app level.
- `sends the text, clears the input, and streams the answer` — accepted ask clears the field and stays enabled during answering; **why**: only starting/recording/finalizing lock the ask box.
- `keeps the input and shows the error when the ask is rejected` — rejected ask preserves the typed text; **why**: no retyping after a failure.
- `is disabled while recording` — the ask input is disabled mid-recording; **why**: an ask cannot supersede a recording from the form.
- `toggles recording like the Record button` — `hotkey:toggle` starts and stops; **why**: the global shortcut is a first-class entry point.
- `is ignored while Settings is open, and works again after close` — hotkey inert with Settings open, focus returns to the gear, hotkey works after; **why**: the user may be typing the accelerator itself into the hotkey field.
- `dock button calls the bridge and surfaces a refusal` — "Dock to camera" calls `dockToCamera` once and an `internal` refusal ("Could not find the display this window is on.") lands in the alert box; **why**: the one window operation the UI cannot verify itself — a silent refusal would leave the user pressing a button that does nothing.
- `a successful dock shows no error` — a successful dock renders no alert; **why**: the negative half — nothing to say when it worked.
- `focus mode toggles from the header and back` — the Focus mode button flips `aria-pressed`, the ask textbox leaves the a11y tree, the Record button stays visible, and a second click brings the textbox back; **why**: focus mode is a view-only toggle that must keep the primary control and be fully reversible.
- `lights the chip the SAVE returned, not the one clicked` — core coerces Brief→detailed and the UI follows the response; **why**: the persisted value is truth; the click is only a request.
- `is re-checked after any save while it is refused, since every save retries the registration` — with the hotkey reported refused, the "already taken by another app" notice shows; after a style-chip save (whose effect step retried and registered it) the notice is gone; **why**: ADR 016 — every save retries a refused hotkey, so the notice must not outlive a registration that has since succeeded.
- `a profile switch sends ONLY activeProfileId and lights the chip the save returned` — clicking the second profile chip calls `setSettings` with exactly `{ activeProfileId: 'b' }` and the pressed chip follows the returned view; **why**: §8 — a switch never rewrites profile text, so the id must travel alone; and the chip mirrors the persisted value, never the click.
- `a refused switch leaves the old chip lit and shows the error` — a failed `setSettings` shows its message in the alert box and the previously active chip stays pressed; **why**: after a failed save the app is still grounded in the old profile — lighting the new chip would claim a switch that did not happen.
- `sends only the touched key field and re-checks the hotkey` — the patch carries the typed key, omits untouched keys, sends the rest of the form whole, and `hotkeyStatus` is re-fetched after save; **why**: saving may re-register the shortcut, and untouched keys must never be re-sent.
- `responses delivered in reverse commit order never reinstall the older view` — a style click and a profile switch are held open; the switch's response (revision 3) is delivered first, the style's (revision 2, still naming profile `a`) second: the switched chip stays lit and Brief stays pressed; **why**: R5 — `applyView` keeps the highest revision, compared inside a functional state update so racing handlers cannot reinstall an older view.
- `a form made stale by a save it never saw gets the conflict, reloads, and saves both changes` — a commit lands in the fake core after Settings opened; Save → the "Settings changed elsewhere — reload" alert; Reload → "Reloaded…"; Save → Saved ✓ with `expectedRevision: 2`, the other commit's `answerStyle: 'detailed'` and the typed focus, and the fake core at revision 3; **why**: the full App path of the conflict contract — nothing typed is lost and nothing committed elsewhere is overwritten.
- `a startup storage warning is shown on the main view` — a `storageWarning` in the loaded view appears in the main alert box; **why**: a quarantined or unreadable settings file must be surfaced where the user is looking at launch, not only inside Settings.
- `refuses a question over the limit before sending anything, and keeps the text` — with the local provider and an `over` budget, Ask shows "This question is 12 bytes too long for free local mode…", `ask` is never called, the input keeps the text, and the budget was asked for the ACTIVE profile, the saved style and the actual question; a `tight` budget then lets the same question through; **why**: R4 — typed Ask is validated at its real size before anything is superseded, and only `over` blocks.
- `cloud providers never ask for a budget` — an Anthropic Ask goes straight to `ask` with no `localPromptBudget` call; **why**: the byte cap is local-only; cloud asks must not pay an extra round trip.

### `src/views/MainView.test.tsx` (69 `it(...)` → 80 runtime cases; hand-built `SessionApi`, assertions on rendered DOM)

**status line (10 source / 13 cases)**

- `idle without a registered hotkey` — the ready line with `role="status"`; **why**: the status line is the app's narrator and must be announced.
- `idle appends the formatted hotkey when registered` — ready line + formatted accelerator; **why**: the shortcut is only advertised when it actually works.
- `%s` (it.each: starting/recording/finalizing/answering) — each state shows its exact status text; **why**: the four pinned in-flight strings.
- `explains the auto-stop when the recording cap was hit` — the 120s-limit wording; **why**: an unexplained auto-stop looks like a bug.
- `shows the first-run nudge when the Deepgram key is missing` — first-run text; **why**: the empty-state path to Settings.
- `first-run keys off the SELECTED provider` — Groq selected + missing Groq key → nudge; **why**: the nudge must track the provider in use.
- `a missing key for the UNSELECTED provider is not first-run` — ready line despite a missing unused key; **why**: nagging about an unused provider's key is noise.
- `renders an empty line while settings are null, keeping the live region mounted` — with `settings: null` the `.status-line` exists with `role="status"` and empty text, and neither "Ready" nor "First run" is shown; **why**: before settings load neither line is known to be true — a wrong first frame that flips a moment later is what users see, and the live region must stay mounted (UX-12) so only its text changes.
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

**recording row (3)**

- `shows the level meter and mm:ss timer while recording` — meter `aria-valuenow` and formatted timer; **why**: live feedback that audio is being heard.
- `is absent when idle` — no meter; **why**: the meter and timer exist only during recording.
- `keeps the row container mounted while idle` — `.recording-row` is in the DOM while idle; **why**: the row never unmounts — its reserved `min-height` is what keeps the answer from jumping when the meter appears.

**question heard panel (6)**

- `shows the placeholder when nothing was ever recorded` — placeholder text; **why**: the empty state must explain itself.
- `shows Listening… and the live tag while recording with nothing heard` — Listening… + live; **why**: silence during recording needs distinct feedback from idle.
- `shows the live transcript text while recording` — the partial text renders; **why**: the transcript is the user's confirmation the right question was heard.
- `drops the live tag once recording ends` — no live tag when idle; **why**: "live" on a finished transcript is a lie.
- `hides the live tag when viewing an OLDER entry mid-recording` — an older entry viewed during recording shows its text with no live tag; **why**: the tag belongs to the entry the STT is feeding — a pulsing "live" on finished text claims the visible words are still moving while the real transcript updates out of view.
- `keeps the live tag when viewing the LIVE entry mid-recording` — viewing the newest entry mid-recording keeps the tag; **why**: the positive half of the scoping rule — the genuinely live entry must still say so.

**transcript strip auto-collapse (4)**

- `stays expanded while idle with nothing heard, so the placeholder is visible` — the "Question heard" disclosure button is `aria-expanded="true"` with `aria-controls="transcript-body"` and the pinned placeholder is visible; **why**: the pinned idle placeholder must stay readable — collapsing an empty strip would hide the one line that explains what the panel is for.
- `collapses to a one-line caption after recording ends, and expands on toggle` — recording → idle flips the button to `aria-expanded="false"`, hides `#transcript-body` via the `hidden` attribute, shows the question as a caption in the head strip (NOT inside the button, whose accessible name stays exactly "Question heard"); a click expands it again; **why**: after the answer lands the question is context, not content — the strip must shrink so the answer gets the space, while the caption keeps the question glanceable and the button's name stays stable for screen readers (FE-R2).
- `keeps "Question heard" a real heading with the disclosure button inside it` — `heading level 2 "Question heard"` contains the button, and the `live` tag is a sibling, not part of the button's name; **why**: a heading nested inside a button drops out of the screen reader's heading list (a button's descendants are presentational) — the standard disclosure pattern is the button inside the heading (FE-R2).
- `a new recording starts a fresh auto cycle after a manual expand` — a manual expand on a finished entry, then a new recording (expanded), then its completion collapses again; **why**: the manual toggle is a per-question preference, not a permanent override — otherwise one expand would pin the strip open for the rest of the call.

**ask form (5 source / 8 cases)**

- `is disabled while %s` (it.each: starting/recording/finalizing) — input disabled; **why**: an ask mid-pipeline is refused, so the control must look refused.
- `stays enabled while %s` (it.each: idle/answering) — input enabled; **why**: asking during answering is legal (it supersedes).
- `clears the input only when the ask is accepted` — accepted → cleared; **why**: clearing on a refusal would eat the question.
- `keeps the input when the ask is refused so the user can retry` — refused → text intact; **why**: same guarantee, negative case.
- `offers Interview prep only for an interview profile` — the toggle exists for an interview profile and is absent when the active profile is a sales one; **why**: the practice library is interview-only content — offering it under a sales call would ground the user in the wrong questions.

**hints under the ask form (4)**

- `shows the hotkey tip only before the first answer and only with a registered hotkey` — the "Tip: press <hotkey> from the meeting window — the answer streams here." line appears with an empty history and a registered hotkey, disappears once an entry exists, and never appears with `registered:false`; **why**: the tip teaches the primary flow once; after the first answer it is noise, and advertising a dead shortcut misleads.
- `warns when the active profile has neither resume nor job description` — a profile with whitespace resume and empty JD shows exactly "No resume or job description saved for Bare — answers won't be grounded. Add them in Settings."; **why**: an ungrounded profile answers from nothing — the user must learn why the answers are generic before the interview, not after.
- `does not stack the ungrounded hint on top of the first-run nudge` — with the Deepgram key also missing only the first-run status shows; **why**: two competing calls to Settings are worse than one — keys come first.
- `is silent when the active profile is grounded` — the default profile shows no hint; **why**: the hint must vanish the moment there is nothing to fix.

**style chips (2)**

- `pressed state mirrors the persisted style, not the clicked chip` — clicking Brief while settings still say detailed keeps Detailed pressed; **why**: after a failed or coerced save, lighting the clicked chip shows a style that is not in effect.
- `surfaces a failed style save in the error box` — `setError` called with the failure; **why**: a silent failed save leaves the user thinking the style changed.

**profile chips (5)** — the `ProfileChips` component (`role="group" aria-label="Call profile"`), rendered by MainView

- `are hidden with a single profile` — no group with one profile; **why**: a one-option switcher is noise and steals a row from the answer.
- `show one chip per profile once there are two, pressed mirroring activeProfileId` — two chips, the active one `aria-pressed="true"`; **why**: the chip row is the only in-call view of which profile grounds the answers.
- `click sends the id but the pressed chip follows the PERSISTED value, not the click` — clicking the inactive chip calls `onSelectProfile('b')` and the pressed state does NOT move until settings say so; **why**: UX-12 — after a failed or refused save, lighting the clicked chip shows a profile that is not in effect.
- `clicking the active chip is a no-op (no cache write for nothing)` — `onSelectProfile` is not called; **why**: a switch to the same profile is a settings write and a prompt-cache write that buys nothing.
- `surfaces a failed switch in the error box` — a failing `onSelectProfile` reaches `session.setError` with the code and message; **why**: a silent failed switch leaves the user believing the next answer is grounded in the other job.

**error box (2)**

- `renders real errors with role alert` — alert contains the message; **why**: errors must be announced.
- `renders nothing for aborted` — no alert; **why**: aborted is invisible by contract.

**history bar (7)**

- `is hidden with fewer than two entries` — no navigation; **why**: navigation over one entry is noise.
- `appears at two entries with an n/m label and nav arrows` — 2/2, Next disabled at the end, Prev fires; **why**: the bar's core affordances.
- `renders inside the answer panel head` — the `nav "Answer history"` is a descendant of `.answer-panel .panel-head`; **why**: §9 — navigation lives next to what it pages, not in a separate row above the transcript.
- `disables prev at the oldest entry` — Prev disabled, Next enabled; **why**: end clamping made visible.
- `disables Clear unless idle` — Clear disabled during answering; **why**: clearing mid-pipeline is refused, so the button must show it.
- `Clear clears, announces, and moves focus to Record` — clearHistory fires, "History cleared" announced, focus lands on Record; **why**: post-clear focus must not be lost on a removed element.
- `announces History cleared again on a second Clear` — the announcement appears, is wiped, and appears again on the next Clear; **why**: a second identical string is a state update React bails out of — no DOM change reaches the live region and the screen reader hears nothing (§9 pins the announcement for every Clear).

**header (7)**

- `opens settings from the gear` — `onOpenSettings` fires; **why**: the settings entry point.
- `keeps the gear disabled until settings load` — disabled with `settings: null`; **why**: opening a form over unhydrated settings shows blanks that then jump.
- `docks to the camera from the header button` — "Dock to camera" calls `onDock` once; **why**: the header button is the any-time dock affordance the Settings help text promises.
- `the dock button works before settings load` — enabled with `settings: null`; **why**: a window operation, not a settings edit — nothing to wait for.
- `the focus toggle reports focusMode and calls onToggleFocus` — "Focus mode" starts `aria-pressed="false"` and a click calls `onToggleFocus`; **why**: the pressed state is the prop, not local state — App owns the mode.
- `Ctrl+Shift+F toggles focus mode from the window` — the chord on the window calls `onToggleFocus`; **why**: an in-window shortcut (NOT a global one) so a reader can go answer-only without reaching for the mouse mid-call.
- `Ctrl+Shift+F is ignored while typing in a text field` — with the ask textbox focused the chord does nothing; **why**: inside a text field the chord could be a legitimate edit.

**focus mode (2)**

- `hides the ask row, chips, banner and transcript but keeps Record, status and the answer` — with `focusMode: true` (two profiles, local provider) the root carries `.main-view--focus`, the Focus button is pressed, the ask textbox / "Question heard" button / profile group / "Current mode" banner leave the a11y tree and the tip is not visible, while the Record button, the Answer style group, the ready status and the answer placeholder stay visible; **why**: focus mode is "only what you read and the one button you press" — hiding the Record button or the status line would strand a user mid-call.
- `keeps the transcript element mounted so a toggle back needs no remount` — `.transcript-panel` carries the `hidden` attribute rather than being unmounted; **why**: the live regions and the transcript's state must survive the toggle (UX-12) — a remount would drop the collapsed/expanded state and re-announce.

**regenerate visibility (4)**

- `is shown when the viewed entry has a question and state is idle` — button present; **why**: regeneration is offered exactly when it can act.
- `is shown during answering` — present mid-stream; **why**: re-asking during a stream is legal supersession.
- `is hidden while recording` — absent; **why**: regenerating over a live recording would kill it.
- `is hidden when the viewed entry has no question` — absent; **why**: nothing to re-ask.

### `src/views/SettingsView.test.tsx` (45 tests; the fake `onSave`/budget promises that must stay open are settled by hand, never by timing)

**focus and dismissal (3)**

- `moves focus into the heading on open` — the Settings heading has focus; **why**: keyboard/screen-reader users must land inside the dialog.
- `Escape closes` — `onBack` fires on a CLEAN form; **why**: the standard dismissal path (UX-12 pins that a clean form closes on the first Escape).
- `Back closes` — `onBack` fires from the button; **why**: the visible dismissal path.

**dirty guard (5)**

- `a dirty form blocks Escape and Discard then closes` — after typing into Focus, Escape does not close but shows `role="alertdialog" "Unsaved changes"` whose accessible description is "Discard unsaved changes?" (via `aria-describedby`, FE-R3), with focus on "Keep editing"; clicking "Discard" closes; **why**: a stray Escape mid-edit used to throw away a pasted resume; focus lands on a button, so the question must be the dialog's description or it is never announced, and the least destructive action holds focus.
- `Back on a dirty form asks too, and Keep editing keeps the draft` — the Back button raises the same guard; "Keep editing" dismisses it with the draft intact and `onBack` never called; **why**: both dismissal paths must guard, and keeping must really keep.
- `Escape on the guard means keep editing` — a second Escape dismisses the guard and does NOT close; **why**: a second Escape must never be the destructive answer.
- `a typed-then-reverted edit is clean again` — typing a character and backspacing it makes Escape close directly; **why**: dirtiness is a value comparison against the seeded props, not a "was touched" flag — otherwise every glance at the form ends in a dialog.
- `a successful save makes the form clean` — after Save and "Saved ✓", Escape closes directly; **why**: the saved draft IS the new baseline.

**key fields (4)**

- `are always empty, with a replace placeholder only where a key exists` — key inputs render empty; "saved — type to replace" only where a key is stored; **why**: stored key material never round-trips into the DOM, and promising "saved" where nothing is saved is a lie.
- `are password inputs so keys never show on screen shares` — all three key fields are `type="password"`; **why**: this app is used during screen-shared calls.
- `only the fields the provider needs are shown; local hides all three` — Anthropic shows Deepgram + Anthropic and hides Groq (via the `hidden` attribute on the `.field` wrapper); local hides all three and shows the "Free local voice setup" region; Groq shows Deepgram + Groq and drops the region; **why**: a key field for a provider not in use invites the user to paste a key that will never be read — and the fields stay mounted (hidden, not removed) so a typed draft survives a provider round trip.
- `lists the providers in PROVIDER_ORDER with their catalogue labels` — the Answer model select lists exactly the three catalogue labels in order; **why**: the provider list is derived from one table — a label drifting from the catalogue would mislabel what the user is paying for.

**saving (10)**

- `omits untouched key fields but always sends the rest of the form` — patch has no key fields but carries `profiles` (the whole array), `activeProfileId`, `alwaysOnTop`, `llmProvider`, `answerStyle`, `hotkey`, `launchPlacement` and `streamFollow`; **why**: sending an empty key field would wipe a stored key on every unrelated save, while the non-key form travels whole.
- `sends a typed key, and only that key` — one key in the patch; **why**: minimal-diff key updates.
- `a typed-then-cleared key sends "" to wipe the stored key` — explicit `""` travels; **why**: type-then-clear is the deliberate delete gesture and must be distinguishable from untouched.
- `a successful save resets key fields to untouched` — a second save omits the key again; **why**: a second save must not silently re-send (and re-store) the same key.
- `sends changed provider, style, hotkey, and always-on-top values` — all four changed values land in the patch; **why**: the non-key form travels whole and current.
- `sends a changed launch placement and stream follow` — selecting "Dock under the camera (top centre)" and "Stay at the opening sentence (teleprompter)" sends `launchPlacement:'camera'` and `streamFollow:'top'`; **why**: the two new window options ride the same form and must reach the core as the wire enums.
- `the hotkey placeholder is the core default accelerator` — with an empty hotkey the Global shortcut input's placeholder is `DEFAULT_HOTKEY`; **why**: an empty field means "disabled" (§8) — the placeholder shows what to type without springing the default back.
- `shows Saved ✓ briefly after a successful save` — appears then disappears; **why**: confirmation that also gets out of the way.
- `reports a failed save in a settings-local error box` — alert with the message, no Saved ✓; **why**: a failed save that looks like success loses the user's edits.
- `re-seeds every field from the returned view` — a save whose response repaired the profile id to `p1`, the name to `Untitled` and the hotkey to `Ctrl+Alt+K` shows those values in the form afterwards; **why**: the core may repair ids and trim names — the form must show what was stored, not what it proposed.

**profiles (8)** — the old 6,500-byte counter test is replaced by the R4 block below

- `edits only the selected profile and sends the whole array plus its id as active` — selecting profile `b` and typing into Focus sends both profiles with `a` untouched, `b`'s focus extended, and `activeProfileId:'b'`; **why**: the form owns the whole draft (a whole-array replace, §4), and the selected profile becomes the active one on save.
- `New adds a blank interview profile, selects it, and is disabled at the cap` — New shows a "New profile" interview profile with an empty resume, and the saved patch carries three profiles with a valid `[A-Za-z0-9_-]{1,40}` id that is also the active id; **why**: the id is generated client-side (`crypto.randomUUID()` with a fallback) and must already be in the core's alphabet, or every new profile would come back renamed.
- `New and Duplicate are disabled at MAX_PROFILES; Delete with one profile` — eight profiles disable New and Duplicate (Delete stays enabled); one profile disables Delete; **why**: the form must not offer what the core would refuse (>8) or repair (an empty list → Default).
- `Duplicate copies every field under a new id with a " copy" name` — a full Sales profile duplicates with all seven fields equal, name "Backend copy", a fresh id, the source untouched, and the copy selected; **why**: duplicating is how a user clones a grounded profile for a second job — a field the copy forgot would silently unground it.
- `Delete removes the selected profile and selects the previous neighbour` — deleting `c` of `[a, b, c]` selects `b` and saves `[a, b]` with `b` active; **why**: the selection must land on something that exists, and the previous neighbour is the least surprising choice.
- `labels switch with the call type` — interview shows "Resume"/"Job description"; switching the Call type to sales relabels them "About you (optional)"/"Call context (account, product, agenda)" with the same values; **why**: a sales rep has no "job description" — the label must describe what the model will read the text as, without losing what was typed.
- `shows a character counter per text field` — Resume/JD carry `n / 200,000 characters` and Focus/Extra `0 / 2,000 characters` as their accessible descriptions; **why**: the core caps by characters (§8) — the counter tells the user where the cut will fall before it happens.
- `the local budget line is gone for cloud providers and never asks the core` — with Anthropic selected, no budget call is made (after longer than the debounce) and no local-mode text renders; **why**: the budget is a local-only concern.

**save lock (R3) (3)**

- `locks every editing and navigation control while a save is in flight, then unlocks` — with `onSave` held open: Save reads "Saving…" and is disabled, a "Saving…" status shows, Resume/JD/Focus/Profile name/Call type/Profile/Answer model/Answer style/Deepgram key/Global shortcut and New/Duplicate/Delete/Back are all disabled, typing into Resume changes nothing; resolving re-enables everything with "Saved ✓" and exactly one save; **why**: R3 — text typed during the save used to be overwritten by the response; a lock that covers every path is the smallest change that makes loss impossible.
- `refuses Escape and Back while saving, even on a dirty form` — neither closes nor shows the discard guard mid-save; after the save, Escape closes; **why**: unmounting mid-save drops the response and leaves the user unsure what was stored.
- `gives focus back to Save when the lock lifts, if the lock dropped it` — focus is pushed to `<body>` during the save (the webview's focus fixup for a disabled focused button, simulated because jsdom does not do it); after the response Save has focus again; **why**: a keyboard user must not restart from the top of the window after every save.
- `a failed save restores interactivity and keeps the draft and the typed key` — an error response shows the alert, and the form is editable with the typed focus and the typed key intact; **why**: FINAL-REVIEW §3 — a failed save must not cost the draft, and the lock must never outlive the save.

**revisions (R5) (5)**

- `a save sends the revision the form was seeded from, then the one its response committed` — `expectedRevision` is 7 for a form seeded at 7, and 8 on the next save after a response at 8; **why**: the saved response is the form's new base — re-sending the old revision would make every second save a conflict.
- `a conflict keeps the draft and blocks Save; Reload rebases the edits onto the newer view` — a `settings_conflict` response shows the "Settings changed elsewhere — reload" alert, disables Save and keeps the typed resume; Reload (a newer view with a different style and an edited profile B) keeps the typed resume and key, takes the new style and B's new focus, and the next save sends revision 3 with all of them; **why**: the reload path must lose neither the user's edits nor the other writer's.
- `an untouched open form silently follows a newer committed view` — a newer view through props re-seeds an unedited form (new style shown, no banner) and its next save sends the new revision; **why**: a form nobody touched has nothing to protect, so a chip save that landed after Settings opened must not force a reload.
- `a dirty open form is told at once when a newer view arrives, and keeps its edits` — the banner appears before any save, with the typed focus and the old style still in the form; **why**: telling the user before they press Save beats a refused save.
- `undoing the edits that raised the banner follows the newer view and clears the banner` — an edited form gets the banner when a newer view arrives; deleting the edit makes the form clean, so it follows the newer view (style `brief`), the banner goes, Save is enabled and sends the newer revision; **why**: a banner that outlives its cause blocks Save for no reason.
- `shows the startup storage warning` — a `storageWarning` renders as a `role="note"`; **why**: the quarantine/unreadable message must be visible where settings are edited.

**local budget (R4) (4)** — the fake bridge's `localPromptBudget` answers per test

- `previews the UNSAVED draft and shows the remaining bytes` — "6,000 bytes left for the question" appears, typing "Rust" into Focus asks the core again with the unsaved focus, Balanced and an empty question, and the new figure renders; **why**: FINAL-REVIEW §4 — the preview must reflect what is being typed, not the last save.
- `separates little room (a warning) from over the limit (blocking local use), and Save stays enabled` — `tight` renders "Only 150 bytes left…" in the warning style; `over` renders "Too long for free local mode … 7,274 of 7,000 … you can still save it for a cloud model" in the error style; Save still saves; **why**: the 200-byte reserve is a warning, only exceeding the limit blocks, and only local use — a profile kept for cloud models must stay saveable.
- `discards an obsolete answer when the style changes again before it lands` — the first (Balanced) request is held, the style changes to Detailed and its answer renders, then the held answer arrives and is ignored; **why**: out-of-order IPC answers must never show a figure for a draft that no longer exists.
- `says so when the size check itself fails, instead of "Checking…" forever` — an error envelope from `localPromptBudget` shows "Could not check the free local mode size…"; **why**: a check that will never finish must not claim to be in progress.
- `switching the edited profile asks for that profile and never shows the previous one's figure` — profile a's figure disappears as soon as b is selected and b's own figure renders; **why**: a figure is shown only for the profile id and style it was computed for.

### `src/views/LocalMode.test.tsx` (3 `test(...)`; drives SettingsView's `LocalVoicePanel` and MainView through the `FakeBridge`, whose defaults are "nothing running" for status and "all ready" for the warm-up)

- `free mode saves with no cloud keys and preserves shared profile and controls` — choosing "local" in Answer model shows the "Free local voice setup" region, and Save sends `llmProvider:'local'` with the untouched profiles/active id/style and NO key fields; **why**: switching to free mode must not touch the stored cloud keys (the core keeps them; the form must not send `""`), and the profile is shared across modes.
- `warm-up updates readiness and exposes actionable setup failures` — the panel calls `localVoiceStatus` on mount (showing "Not found"), "Start and warm free mode" calls `prepareLocalVoice` once and shows "Ready. Save this mode…" and "Installed"; a refused warm-up shows its message ("Free disk space, then rerun setup.") and the button stays enabled for a retry; **why**: the warm-up is the one long-running local action — the user needs to see it succeed, and a failure must carry the fix, not just "error".
- `main local mode records without the missing cloud-key nudge` — with the local provider and no keys at all the main view shows "Free local voice · no API fees", no "add your API keys" nudge, and Record toggles the session; **why**: the first-run nudge keys off `needs_cloud_keys`/`uses_deepgram` — nagging a free-mode user for keys they will never need blocks the primary flow.

## Frontend — components

### `src/components/AnswerPanel.test.tsx` (28 tests)

- `shows the placeholder when no entry exists` — the stream-here placeholder; **why**: the empty state explains what the panel is for.
- `marks streaming with the generating tag and aria-busy` — generating… tag, `aria-busy="true"`, `aria-live="polite"`; **why**: assistive tech must know the region is mid-update.
- `drops aria-busy when the stream settles` — `aria-busy="false"`, no tag; **why**: a permanently-busy region is never announced as done.
- `shows the latency chip with the metrics tooltip once metrics exist` — chip text + `title` tooltip; **why**: the headline metric and its breakdown.
- `hides the latency chip while metrics are missing` — no chip mid-stream; **why**: a chip with no data would show a placeholder lie.
- `labels an interrupted answer incomplete and keeps its text and reason` — an `incomplete` entry keeps its text, shows an `incomplete` tag (class `status-incomplete`, reason as `title`) and the reason as a caption; **why** (R2): the reason rides on the ENTRY, so it survives the next question clearing the global error box.
- `labels a token-capped answer "cut short", separately from the timing chip` — a `limited` entry shows `cut short` and its caption while the latency chip still renders; **why**: outcome and timing are separate facts — the label must never replace the metrics.
- `labels a replaced answer "stopped"` — a `cancelled` entry shows `stopped`; **why**: a superseded answer must not pass for a finished one.
- `shows no outcome label for a completed or still-streaming entry` — no tag and no caption for `completed` or a streaming `pending` entry; **why**: the label exists to flag the exceptions — on every normal answer it would be noise.
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

**history navigation in the head (3)** — `HistoryBar` now renders inside the panel head from the `historyLength`/`viewIndex`/`idle`/`onPrev`/`onNext`/`onClear` props

- `is hidden below two entries` — no `nav "Answer history"` with one entry; **why**: a one-entry navigator is noise.
- `renders inside the panel head, before Regenerate and Copy, and fires the callbacks` — the nav sits in `.answer-panel .panel-head`, the head's buttons read `Previous answer, Next answer, Clear, Regenerate, Copy` in that order, and each nav button fires its callback once; **why**: §9 pins prev/next next to what they page, before the per-answer actions — and the props are primitives plus stable callbacks so the panel's `memo` still holds.
- `Clear is enabled only while idle` — `idle: false` disables Clear; **why**: clearing mid-pipeline is refused by the reducer, so the button must show it.

**stream follow (4)** — `streamFollow` seeds `nearBottomRef` (`tail`: following at mount; `top`: not), leaving the stick branch itself untouched so every autoscroll test above stays valid

- `tail carries a fresh reader to the bottom of the first stream` — with `tail` (the pinned default) the first tokens of a stream scroll a never-scrolled reader to the bottom; **why**: the v3 behaviour, now explicit: a reader who never touched the scrollbar is following.
- `top parks a fresh reader at the opening sentence while the stream lands` — with `top` the same stream leaves `scrollTop` at 0; **why**: the teleprompter option — a reader who wants to say the first sentence while the rest streams must not be dragged to the tail.
- `top does not re-engage sticking on an entry switch, even when the new entry fits` — switching to a new live entry that fits and then streaming past the fold stays at 0 (the `tail` counterpart of this scenario lands at 50); **why**: `resetScrollForNewEntry` re-derives "following" as `follow === 'tail' && fits` — a `top` reader must never be re-armed by a short entry.
- `top still resets to the top when switching history entries` — a `top` reader scrolled to 400 lands at 0 on a history switch; **why**: the §9 "old answers open at the top" rule is independent of the follow option.

### `src/components/ErrorBox.test.tsx` (3 `it(...)` → 15 runtime cases)

- `renders %s as an alert carrying the message` (it.each over the 13 rendered codes: `no_stt_key`, `no_llm_key`, `stt_connect`, `stt_error`, `stt_timeout`, `no_speech`, `llm_auth`, `llm_http`, `llm_rate_limit`, `llm_first_token_timeout`, `llm_timeout`, `internal`, `settings_conflict`) — each renders a `role="alert"` with the raw message plus a human title; **why**: every code in the closed set must have a rendering — an unmapped code would show nothing for a real failure.
- `renders nothing for aborted — the user did that on purpose` — empty DOM; **why**: the aborted-is-silent contract at the component that would otherwise paint it.
- `renders nothing when there is no error` — empty DOM; **why**: the null state must be truly empty, not an empty frame.

### `src/components/HistoryBar.test.tsx` (6 tests)

- `hides below two entries` — no navigation for one entry; **why**: a one-entry navigator is noise.
- `shows the n/m position label` — `2/2`; **why**: orientation within history.
- `disables the arrows at the ends and fires them in between` — mid-list arrows both fire; **why**: the callbacks are the component's contract.
- `disables prev at the oldest and next at the newest` — end clamping in the DOM; **why**: disabled ends prevent no-op clicks and signal position.
- `permits Clear only while idle` — disabled when not idle; **why**: mirrors the reducer's clear guard visually.
- `fires onClear` — the callback fires; **why**: the destructive action's wiring.

### `src/components/AskForm.test.tsx` (6 tests; the PracticeLibrary chunk is deliberately NOT imported at the top of the file — the load counter must start at 0 — and every `findBy*` that waits for it uses a 10 s `CHUNK_TIMEOUT`)

- `fetches the PracticeLibrary chunk on hover, before any click opens it` — hovering the Interview prep toggle loads the chunk (load count 0 → 1) while the toggle stays `aria-expanded="false"` and no question button exists; **why**: FE-3 — the chunk warms on `pointerenter`/focus so the click that opens the library is instant, but warming must never open it.
- `stages a question for editing without sending it, then submits the edited draft` — clicking a library question fills and focuses the input without calling `onAsk`; the edited draft is what Ask sends, and the input clears; **why**: a practice question is a starting point the user edits, not a one-click send — and the edited text must be exactly what goes out.
- `filters questions by category and search, and can reset an empty result` — the category select and the search box narrow the "n of 12 questions" count down to 0, and "Reset filters" restores 12 with both controls cleared; **why**: an empty result with no way back is a dead end mid-prep.
- `keeps guides available during recording and disables question selection` — with the form disabled the question buttons are disabled (a click sends nothing) while the "Before the call" guide still opens; **why**: reading a guide mid-recording is fine; staging a question would race the live transcript.
- `hides the Interview-prep toggle and closes an open library when showPrep turns off` — an open library disappears with the toggle when `showPrep` flips false; flipping it back returns the toggle closed, not sprung open, and the input stays usable; **why**: the library is interview-only content — a sales/support profile hides the toggle and must also close a library the user left open, or it lingers under a toggle that no longer exists.
- `sends once and preserves a rejected draft for retry` — two synchronous submits call `onAsk` once and disable the input with a "Sending…" button; a rejection re-enables the form with the draft intact; **why**: a double-click must not send twice, and a rejected question must not have to be retyped.

### `src/components/useTransient.test.ts` (6 tests; fake timers)

**useTransient (5)** — the shared "show for a beat, then revert" hook behind Copied ✓, Saved ✓ and the two live-region announcements (R7)

- `reverts to idle exactly after the delay` — true at 1 199 ms, idle at 1 200 ms; **why**: the four hand-rolled timer sites drifted in their delays — one hook, one number.
- `a second set inside the window restarts the clock` — a second set 800 ms into a 1 000 ms window is still shown 800 ms later and reverts 200 ms after that; **why**: a second Copy 800 ms into the note used to be cut short by the FIRST timer 200 ms later, so the confirmation of the second click vanished almost immediately.
- `setting the idle value by hand is a plain reset with no timer left behind` — setting the idle value cancels the pending revert; **why**: a stale timer firing after a manual reset would clear a value set later.
- `the setter is referentially stable, so it can sit in useCallback deps` — the setter is the same function across renders; **why**: an unstable setter would invalidate every `useCallback` that closes over it and re-render the memoised panels for nothing.
- `unmount clears the pending revert` — the timer count returns to baseline on unmount; **why**: a timer firing after unmount sets state on a dead component.

**useAnnouncer (1)**

- `starts empty, holds the text, then wipes it after 1.5 s by default` — `''` → "History cleared" → `''` at 1 500 ms; **why**: the wipe is what makes a REPEATED announcement audible — an identical string is a state update React bails out of, so no DOM change reaches the live region and the screen reader hears nothing (§9 pins the announcement for every Clear).

---

## Known test-environment notes

- **Two paused-clock Deepgram tests** (`keepalives_flow_on_an_idle_open_socket`, `no_keepalive_is_sent_once_close_is_requested` in `core/src/stt/deepgram.rs`) interact with tokio's paused-clock auto-advance while using real loopback sockets. With the clock paused, an idle runtime auto-advances to the nearest timer, which can fire the 5 s connect timeout before real loopback bytes ever move, or let a drain cap race real socket readiness. They were made deterministic by (a) pausing only after the handshake / proving establishment (a transcript round-trip) before stopping, and (b) ending the drain via the server sending its own WebSocket Close rather than relying on the drain cap timing out.
- **jsdom boot on this machine is slow**, so `vite.config.ts` sets vitest's `testTimeout` (and `hookTimeout`) to 20 s — a cold jsdom start must not read as a test failure.
- **Lazy chunks compile in real time under jsdom.** `AskForm.test.tsx` gives every `findBy*` that waits for the PracticeLibrary chunk a 10 s timeout (`CHUNK_TIMEOUT`), and `App.test.tsx`'s `renderApp()` waits up to 10 s for the gear to enable: the app now warms both lazy chunks 1.5 s after mount, sixteen jsdoms run at once, and a `findBy*` default of 1 s flaked whenever the chunk loaded cold. The App preloading test uses real timers on purpose — faking the clock around a React tree that later tests in the file render is how a whole file turns flaky.
- **The delta-coalescing tests use `vi.useFakeTimers()`** because the 16 ms window is a `setTimeout`. The pre-existing `useSession` tests never assert on a second delta by itself: wherever one emits a second `llm:delta` it pairs it with `llm:done` or `session:error` in the same `act`, and those handlers flush the window synchronously — so a test that adds a lone second delta and expects it painted must advance the fake clock by 16 ms.
- **jsdom under vitest 3 runs a real rAF clock** (`pretendToBeVisual`): `requestAnimationFrame` fires on its own ~16 ms frame clock, NOT the timer queue, so flushing the answer panel's frame-coalesced paint with a `setTimeout(0)` only caught a pending frame when the machine happened to be slow — a latent flake. The `frame()` helpers in `App.test.tsx` and `AnswerPanel.test.tsx` therefore await a `requestAnimationFrame` of their own: rAF callbacks run in scheduling order, so our frame firing guarantees any frame the panel queued earlier has already fired. (`vitest.setup.ts` keeps a macrotask rAF polyfill only for environments with no rAF at all.)

# ADR 007 — Byte-stable two-block prompt with the cache breakpoint after the profile

**Status**: accepted

## Context

Anthropic prompt caching is a **byte-prefix match**: the cached prefix must be
byte-identical across calls or every call silently pays a cache write (1.25×
input price) instead of a read (0.1×). The prompt here splits naturally into
two parts with opposite change rates: the role instructions + resume + job
description (large, stable for a whole interview) and the answer-style policy
(small, flipped mid-call via the Brief/Balanced/Detailed chips). Style flips
must be latency-free — SPEC §3 and §7 make both the split and the exact
strings load-bearing product behavior.

There is also an honesty problem worth recording: Haiku's minimum cacheable
prefix is 4096 tokens, so a typical 1–2 K-token profile makes the cache marker
a **silent no-op** (README, "An honest note on prompt caching").

## Decision

- The system prompt is built as `SystemPrompt { cached_prefix, style_suffix }`
  (`src-tauri/core/src/llm/prompt.rs:141-159`): role instructions + the whole
  active profile in the prefix, style policy in the suffix. Anthropic
  receives it as **two blocks with `cache_control` on the first**
  (`anthropic.rs`, `request_body`); Groq and the local provider get the
  halves joined with one blank line (`prompt.rs:152-158`).
- **The prefix order is fixed** (`prompt.rs:175-228`, ADR 014): role
  instructions · the call-type line (nothing for an interview) · resume (or
  "about the user") · JD (or "call context") · the grounding note, iff resume
  or JD is present — focus alone does not trigger it, a list of things to
  emphasise is a steer, not a background to ground in · focus · extra
  instructions. The header set per call type comes from `sections_for`, a
  straight-line `match` (`prompt.rs:165-173`): no map, no clock.
- **Byte-stability is a construction rule**, not a hope: straight-line
  concatenation of owned inputs — no timestamps, no unordered joins, resume
  always before JD (order pinned by
  `sections_appear_in_call_resume_jd_grounding_focus_extra_order`,
  50-rebuild determinism pinned in `prompt_is_byte_stable_across_repeated_builds`
  with a full Sales profile). The store side honours the same rule:
  `normalize_profiles`, the only writer of the profile list, is pure and
  deterministic so that nothing between the settings file and the prefix can
  smuggle in nondeterminism (`core/src/store/settings.rs:186-253`).
- **New sections are added beside the pinned strings, never inside them**:
  the call-type lines, the `ABOUT THE USER` / `CONTEXT FOR THIS CALL`
  headers, `WHAT TO EMPHASIZE` and `ADDITIONAL INSTRUCTIONS FROM THE USER`
  are new constants (`prompt.rs:96-116`), each pinned verbatim. An interview
  profile with empty focus/extra builds a prefix **byte-identical to v3**
  (`migrated_v3_profile_yields_a_byte_identical_prefix`), so the upgrade
  changed nothing the model reads for existing users and cost them no cache
  write.
- Profile text is trimmed **at the edges only** — interior formatting of a
  resume is meaning the model reads, and it must survive verbatim
  (`prompt.rs:177-182`).
- The **user message lives outside the system prompt** (`prompt.rs:234-242`)
  so the per-question transcript never touches the cached prefix.
- The grounding note is appended only when a profile section exists — telling
  the model to ground in an absent resume produces hedging about nothing
  (`prompt.rs:205-211`).
- Cache engagement is observable, not assumed: the provider parses
  `usage.cache_read_input_tokens` and exposes it for logging
  (`anthropic.rs`, the field doc on `last_cache_read_input_tokens` and its
  getter).

## Consequences

- Flipping answer style costs zero latency and never invalidates the cached
  profile — the flip only touches bytes after the breakpoint.
- **Switching the active call profile is a cache write** (ADR 014): a
  different profile is a different prefix, so the next question after a
  switch pays one write (1.25×) instead of a read — only for profiles above
  the 4 096-token minimum, and only once, because the prefix is then stable
  again for the whole call. That is the deliberate cost of a between-calls
  gesture; it is also exactly why per-question profile switching is not
  built. The prefix is byte-stable **per active profile**, not globally.
- The retry policy's byte-identical rule (ADR 006) and this ADR reinforce each
  other: a rebuilt body that differed would also be a cache miss.
- With a large profile (~16 K+ characters) the cache pays real money at 0.1×
  reads on the biggest part of every request.
- The local provider joins the two blocks into one string and counts every
  byte of it — focus and extra instructions included — toward its 7 KB input
  cap (`core/src/llm/local.rs:16`, SPEC §6.5); the cache split buys nothing
  there, and the oversize message names the whole profile.

Costs, honestly:

- Below 4096 tokens of prefix the marker does nothing — the app carries the
  two-block machinery for a benefit most profiles don't reach. Kept because
  the runtime cost is zero and the code cost is small; the README says so out
  loud instead of implying savings that aren't happening.
- **Every prompt string is pinned verbatim by test** (`role_instructions_are_verbatim`,
  `call_type_lines_and_new_headers_are_verbatim` and siblings): "improving
  the wording" is a product decision and breaks a test by design. That is
  friction, and it is the point — and it now covers nine more sentences.
- The 5-minute TTL means cache reads only land during an active interview
  rhythm — a question every few minutes keeps it warm, a long gap re-pays the
  write.

## If revisited

If Anthropic lowers the minimum cacheable prefix or lengthens the TTL, the
cache starts paying for typical profiles with no code change. If the prompt
ever needs per-question dynamic context (say, retrieved notes), it must go in
the user turn — where the transcript already lives — or the stable-prefix
premise collapses and this whole design needs re-deciding.

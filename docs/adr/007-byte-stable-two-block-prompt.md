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
  (`src-tauri/core/src/llm/prompt.rs:63-81`): role instructions + optional
  resume/JD sections + grounding note in the prefix, style policy in the
  suffix. Anthropic receives it as **two blocks with `cache_control` on the
  first** (`anthropic.rs`, `request_body`); Groq gets the halves joined with one blank
  line (`prompt.rs:74-81`).
- **Byte-stability is a construction rule**, not a hope: straight-line
  concatenation of owned inputs — no timestamps, no unordered joins, resume
  always before JD (`prompt.rs:1-13`; order pinned at `prompt.rs:183-191`,
  50-rebuild determinism pinned in `prompt_is_byte_stable_across_repeated_builds`).
- Profile text is trimmed **at the edges only** — interior formatting of a
  resume is meaning the model reads, and it must survive verbatim
  (`prompt.rs:88-101`).
- The **user message lives outside the system prompt** (`prompt.rs:120-123`)
  so the per-question transcript never touches the cached prefix.
- The grounding note is appended only when a profile section exists — telling
  the model to ground in an absent resume produces hedging about nothing
  (`prompt.rs:102-107`).
- Cache engagement is observable, not assumed: the provider parses
  `usage.cache_read_input_tokens` and exposes it for logging
  (`anthropic.rs`, the field doc on `last_cache_read_input_tokens` and its
  getter).

## Consequences

- Flipping answer style costs zero latency and never invalidates the cached
  profile — the flip only touches bytes after the breakpoint.
- The retry policy's byte-identical rule (ADR 006) and this ADR reinforce each
  other: a rebuilt body that differed would also be a cache miss.
- With a large profile (~16 K+ characters) the cache pays real money at 0.1×
  reads on the biggest part of every request.

Costs, honestly:

- Below 4096 tokens of prefix the marker does nothing — the app carries the
  two-block machinery for a benefit most profiles don't reach. Kept because
  the runtime cost is zero and the code cost is small; the README says so out
  loud instead of implying savings that aren't happening.
- **Every prompt string is pinned verbatim by test** (`prompt.rs:129-137` and
  siblings): "improving the wording" is a product decision and breaks a test
  by design. That is friction, and it is the point.
- The 5-minute TTL means cache reads only land during an active interview
  rhythm — a question every few minutes keeps it warm, a long gap re-pays the
  write.

## If revisited

If Anthropic lowers the minimum cacheable prefix or lengthens the TTL, the
cache starts paying for typical profiles with no code change. If the prompt
ever needs per-question dynamic context (say, retrieved notes), it must go in
the user turn — where the transcript already lives — or the stable-prefix
premise collapses and this whole design needs re-deciding.

# Blind code review — AI Call Assistant v3

**Time box: 60 minutes.** Then stop, whatever state you're in.

## Ground rules — read this before you open any code

- Review the code as it stands at `git log -1` (currently `f1cf524`). Don't look at git
  history/blame to "find" the answer — read like a reviewer seeing a PR-sized cross-section
  cold.
- **Do NOT open these before you finish** — they are spoilers, not references:
  - `docs/REVIEW.md` — an existing staff review of an earlier commit. It contains most of
    the answers, some now stale. It is the sealed answer key's starting point.
  - `docs/adr/` — 17 ADRs that name the exact design decisions and the bugs behind them.
  - `docs/learn/` (10 files) and `docs/SPEC.md` §5/§9 — narrate the same invariants.
  - `docs/TROUBLESHOOTING.md`
  - This project's `training/incidents/` and `training/_answers/`.
- Everything else — `docs/README.md`, `docs/DEVELOPMENT.md`, the source tree — is fair game.

## Scope

Pick **one** of the two areas below (don't try to cover both in 60 minutes):

**A. The answer pipeline** — `src-tauri/core/src/llm/` (anthropic.rs, groq.rs, retry.rs,
sse.rs, mod.rs) and `src-tauri/core/src/stt/deepgram.rs`. This is the STT→LLM streaming
path: connect, stream tokens, retry, cancel, timeout.

**B. The frontend session shell** — `src/state/useSession.ts`, `src/state/reducer.ts`,
`src/bridge.ts`. This is where session state, the delta coalescer, and supersession/
cancellation live.

## What to produce

A flat list of findings, each with:

1. **Severity** — HIGH / MEDIUM / LOW, using production impact as the ranking axis:
   correctness, security, data integrity, concurrency, failure handling, performance,
   operability, tests, maintainability (in roughly that priority order).
2. **Evidence** — exact `path:line` (or line range) and a one-sentence description of what
   the code actually does there — not what the comment says it does.
3. **Why it matters in production** — the concrete failure scenario, not "this could be
   bad."
4. **Is it a typical old-AI-generated smell?** (fake integration, an assertion-free test, a
   hardcoded secret/fallback, a missing transaction boundary, reskinned boilerplate, docs
   claiming something the code doesn't do) — yes/no, and why.

Aim for 10–20 findings in your chosen area. Quality over quantity: a finding with exact
evidence beats three vague ones.

## Before you write anything

Answer these in one or two sentences each, in writing, before your first finding:

1. What is this codebase's single hardest invariant to keep (in your chosen area), in your
   own words?
2. Name one place a comment states a guarantee. Does the code one line away actually deliver
   it, or does it check something *adjacent* to the guarantee?
3. Where would you expect this repo's own tests to have a blind spot, given how they're
   written (check `vitest.setup.ts` or a Rust test's `#[tokio::test]` attributes for a clue)?

## When the clock runs out

Open `training/review/COMPARE.md` and follow it. Only then open
`training/_answers/review.md`.

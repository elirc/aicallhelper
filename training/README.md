# Training materials — AI Call Assistant v3

Batch B04, tier **DRILL**. Built against commit `f1cf524` (working tree was clean; nothing
outside `training/` and two incident branches was touched). Focus: AI latency & streaming
failure modes — buffered/batched token forwarding, cancellation that doesn't actually stop
billed work, and a retry policy racing a cancel signal.

DRILL tier = Phase 3 (blind review + incidents) plus a small Phase 5 interview kit
(`PROJECT_STORIES.md` + 10 `TECHNICAL_QUESTIONS.md`, not the full kit — no `SYSTEM_DESIGN.md`,
`MOCK_DEFENSE.md`, or `FLASHCARDS.md` for this tier; no `ladder/`, `navigation/`, or
`agentic/` either — those are LAB-tier phases this project didn't get).

## Suggested order (~2.5-3.5 hours total)

1. **`review/EXERCISE.md`** (60 min, timeboxed) — blind review of one area of the codebase.
   Read the "ground rules" section first; it names what NOT to open yet.
2. **`review/COMPARE.md`** (15-20 min) — grade yourself before looking at the answer.
3. **`_answers/review.md`** — the sealed reference. Also a lesson in its own right: it
   re-verifies an existing in-repo review against current HEAD and finds several of its
   findings already fixed.
4. **`incidents/001-ask-supersede-cancel-dropped.md`** (30-45 min) — frontend
   (`src/state/useSession.ts`), verifiable with `npx vitest run src/state/useSession.test.ts`.
5. **`incidents/002-retry-cancel-race.md`** (30-45 min) — Rust (`src-tauri/core/src/llm/retry.rs`),
   verifiable with `cargo check -p app-core` (compiles clean either way — this is a pure
   logic bug) and, if you have ~8 min for a cold `cargo test` compile,
   `cargo test -p app-core --lib llm::retry::`.
6. **`interview/PROJECT_STORIES.md`** (30-45 min) — write your own answers after 1-5, not
   before. Story 2 asks you to read one ADR as a deliberate, earned spoiler.
7. **`interview/TECHNICAL_QUESTIONS.md`** (30-40 min) — 10 questions, the last 5 specifically
   on latency budgets for AI features, all keyed to this repo's actual constants
   (`limits::STT_FINALIZE`, `LLM_FIRST_TOKEN`, `DELTA_COALESCE_MS`, etc.). Sealed answers in
   `_answers/interview-technical-questions.md`.

## Gate — do not open before you've earned them

- `docs/REVIEW.md` — an existing staff review of an earlier commit (`d18cd33`). Heavily
  overlaps `review/EXERCISE.md`'s answer.
- `docs/adr/` (17 files) — names the exact decisions and bugs behind most findings and both
  incidents (ADR 006 is the retry policy INC-002 lives in; ADR 015 is the adoption
  mechanism INC-001's fix depends on).
- `docs/learn/` (10 files) and `docs/SPEC.md` §5/§9 — narrate the same invariants in
  teaching-doc form.
- `docs/TROUBLESHOOTING.md`, `docs/IDEAS.md`.

## Incidents at a glance

| Incident | Branch | Layer | Symptom (as reported) | Verify with |
|---|---|---|---|---|
| INC-001 | `training/incidents/001-ask-supersede-cancel-dropped` | TS frontend | Anthropic usage shows output tokens for questions the user never saw finish | `npx vitest run src/state/useSession.test.ts` → 2 of 53 fail |
| INC-002 | `training/incidents/002-retry-cancel-race` | Rust `app-core` | Stop doesn't always stop the Anthropic request; a deterministic unit test fails | `cargo check -p app-core` (compiles) + `cargo test -p app-core --lib llm::retry::` → 7 of 8 pass, 1 fails |

Both incidents are single-purpose, PR-sized diffs (1 line removed, 3 lines removed,
respectively) on top of `f1cf524`, with a plausible "refactor/cleanup" commit message and no
hint of the cause in the incident brief itself. Neither touches `training/`.

## What was NOT built for this project (and why)

- **Ladder / navigation / agentic exercises** — DRILL tier, per the batch rules, skips
  Phase 2, 4, and most of Phase 5. This is a very mature, already-well-tested codebase
  (docs/REVIEW.md's own honest verdict: "genuinely good code"), so the highest-value use of
  a DRILL budget is exactly what got built: real incidents in the one area this batch is
  short on (AI/LLM integration: latency, streaming, cancellation) plus a review exercise
  that leverages the repo's own excellent existing documentation rather than duplicating it.
- A **full interview kit** (`SYSTEM_DESIGN.md`, `MOCK_DEFENSE.md`, `FLASHCARDS.md`) — DRILL
  tier's Phase 5 is explicitly scoped down to stories + 10 questions.

See the report file for the competency-graph links, curriculum needs, and existing
learning-doc verdicts — those aren't duplicated here per the B04 rule against writing to
shared files from inside a project.

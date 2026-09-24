# Project stories — AI Call Assistant v3

Fill these in yourself, after doing the review and both incidents. Each is a STAR shape
(Situation, Task, Action, Result) grounded in something you actually traced in this repo —
not a hypothetical. Write 4-8 sentences each; an interviewer will follow up on specifics, so
leave yourself real detail to draw from, not a summary you'd have to reconstruct live.

## 1. Debugging story — INC-001 or INC-002

Situation/Task are given by the incident brief. Write Action (how you actually localized it
— which file you suspected first and why, what the failing test told you, what you ruled
out) and Result (the fix, the regression test, what you'd tell a teammate to prevent the
class of bug). If you did both incidents, pick the one where your first hypothesis was
*wrong* — that's the more interesting story to an interviewer, because it shows how you
recover from a bad lead, not just that you eventually found it.

## 2. Trade-off story — pick one ADR and defend the alternative

Read exactly one of `docs/adr/002-one-session-slot-supersession.md`,
`docs/adr/006-retry-once-delta-guard.md`, or `docs/adr/011-session-tagged-events-stale-drop.md`
(you've earned the spoiler — do this after the review and incidents, not before). Then write
the story as if *you* were the one arguing for the alternative the ADR rejected — a queue
instead of one-slot supersession, retry-until-success instead of retry-exactly-once, a
central dispatcher instead of frontend stale-id dropping. What would you have needed to be
true for your alternative to be the right call? What did the actual ADR's authors know that
made them reject it? This tests whether you can steelman a design you didn't choose, which
is what "trade-off" questions in interviews are actually probing for.

## 3. Incident-caught-by-an-agent story

Both INC-001 and INC-002 are exactly the shape of bug an AI coding agent introduces:
plausible commit message ("refactor: consolidate", "drop the redundant check"), small diff,
compiles clean, passes a casual read. Write the story of what *verification step* — not
"read the diff more carefully," an actual concrete step — would have caught each one before
merge if an agent had proposed it. (Hint: one is caught by `npm test`, the other by `cargo
test -p app-core --lib llm::retry::` specifically — not by `cargo check`, which both
incidents pass.) This is the story to tell when asked "how do you verify AI-generated code
you didn't write yourself."

## 4. Latency/performance story — the delta coalescer

`src/state/useSession.ts`'s `DELTA_COALESCE_MS = 16` window (leading-edge, trailing-merge)
exists because a fast LLM provider was landing several deltas a millisecond, each one a full
reducer pass and a re-render of the main view — see the `FE-1` comment at the top of the
file. Write the story as if you were the engineer who found and fixed this: what would the
symptom have looked like before the fix (stutter, dropped frames, a laggy-feeling UI despite
"the model is fast")? Why 16ms specifically, and why `setTimeout` instead of
`requestAnimationFrame` (the file's own comment explains the WebView2-minimized-window
reason — restate it in your own words)? What would you measure to prove the fix worked?

## 5. (Optional) A design question you'd push back on

Pick one LOW-severity finding from `training/_answers/review.md` (e.g., "finalizing has no
timeout," "corrupt settings.json destroys the original on next save") and write it as "I'd
raise this in a design review, here's the fix I'd propose, here's the trade-off that might
be why it was left as-is." This shows judgment about severity calibration, which senior
interviews probe for directly.

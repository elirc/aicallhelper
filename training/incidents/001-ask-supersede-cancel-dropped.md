# INC-001 — Anthropic usage shows sessions running long after the UI moved on

**Branch:** `training/incidents/001-ask-supersede-cancel-dropped` (based on `f1cf524`)
**Reported by:** support, forwarded from a beta user's screenshot of their Anthropic console
**Severity:** as filed — Sev3 (no visible user-facing bug, cost anomaly only)

## What was reported

> "I asked a question, the answer was still typing itself out, so I typed a follow-up and
> hit Ask again before it finished. The app looked totally normal — the first answer's text
> got replaced by the second one like always. But I keep half an eye on my Anthropic
> console because I'm paying for API access directly, and I noticed today that my
> `output_tokens` usage for this account is noticeably higher than the number of questions
> I actually asked. Like every 'interrupted' question is still costing me the full answer
> even though I never saw the rest of it."

A teammate who spot-checked their own console confirms: sessions where they know they asked
a follow-up before the first answer finished streaming show **two** separate Anthropic
Messages API line items close together in time, both running to completion (`message_stop`
before either one errors), even though only the second answer's text ever appeared on
screen.

## What you have

- The user's description above (their word for word report, kept verbatim — don't assume
  they've correctly diagnosed the mechanism, only trust what they *observed*).
- Full read access to the repo at this branch's `HEAD`.
- `npx vitest run src/state/useSession.test.ts` runs in well under a minute once vitest's
  dep cache is warm.
- The suspect PR (this branch's one commit over `f1cf524`) — you can `git log -p -1` it once
  you've formed a hypothesis, but try to localize the bug from the symptom and the test
  suite first, the way an on-call engineer would from a bug report with no diff attached.

## Not given

- The root cause.
- Which file the bug is in (there are two candidate areas in this codebase for
  "cancellation happens on the wrong condition" — the frontend session shell and the Rust
  retry policy. This incident is in exactly one of them.)

## Your task

1. Reproduce the mechanism *in your head* first: what has to be true about this app's
   session-lifecycle code for "the UI shows one answer but the API bills for two full
   answers" to happen at all? (Hint: nothing about billing lives in the frontend — so what
   frontend behavior would produce this Anthropic-side symptom?)
2. Run the test suite for your top suspect file. Read the failure.
3. Find the exact line removed/changed and explain, in your own words, why it's wrong —
   not just "the test expected X and got Y."
4. Write the fix as a diff (you don't have to apply it — a `diff`-shaped answer is fine).
5. Say what regression test would have caught this **before merge**, distinct from the one
   that already exists (or explain why the existing one *should* have caught it and ask why
   it didn't block the merge — a real "how did this ship" question).

Seal your answer, then open `training/_answers/incidents/001-ask-supersede-cancel-dropped.md`.

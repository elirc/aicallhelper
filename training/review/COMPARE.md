# Compare your review against the reference

Fill this in with your own list still in front of you — before you open
`training/_answers/review.md`.

## 1. Coverage

| Your finding | Severity you gave | Matches a reference finding? (Y/N/partial) |
|---|---|---|
| | | |

## 2. What you missed

For every reference finding you didn't have, write one line: *why* you missed it. Pick from
(or name your own):

- Didn't read that file at all in the time box.
- Read the file, read the comment, believed the comment.
- Saw the code, didn't trace the specific failure scenario to the end.
- Didn't check whether the code's *guard* matches its *stated invariant* (checks a proxy for
  the condition, not the condition).
- Didn't consider what the test harness structurally cannot catch (e.g. a single-threaded
  paused-clock runtime can't exercise a real data race).
- Found it, under-rated its severity (or over-rated it).

## 3. What the reference got wrong or missed

This repo's `docs/REVIEW.md` is dated (an earlier commit than HEAD). Some of its findings
are now fixed. Did you independently notice any of the following without being told:

- A finding in `docs/REVIEW.md` that no longer reproduces at HEAD `f1cf524` (check: did the
  code change, or did you just not verify it?).
- A real issue that isn't in `docs/REVIEW.md` at all (the reference doc is not exhaustive —
  it's dated 2026-08-20 and two feature commits have landed since).

## 4. The pattern behind your misses

One paragraph. Not "I should read more carefully" — name the *category* of miss (see the
option list in §2) and one concrete habit that would have caught it. Example shape: "I
trust a module-doc's claim about what a guard checks instead of reading the guard's actual
condition — I need a checklist step: for every 'X is guaranteed because Y' comment, find Y
in the code and ask what Y actually tests."

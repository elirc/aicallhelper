# 09 — Rendering untrusted model output: structural safety and the streaming invariant

**Concept.** There are two ways to render rich text from an untrusted source.
The common one: generate HTML, then *sanitize* — an allowlist chasing an
open-ended attack surface, where every parser quirk in your sanitizer-vs-
browser pair is a potential bypass. The structural one: never produce markup
from content at all. Parse to an AST of plain strings, render every string as
a **text node**, and derive **no attribute** from content. Then there is no
injection point to defend; the security property is a *shape* of the code,
checkable by audit, rather than a filter you hope is complete.

Streaming adds a second, less famous invariant: the renderer sees every
*prefix* of the document, many times per second, and (a) no prefix may throw,
(b) the final DOM must be byte-identical to a one-shot render of the full
text, (c) completed blocks must keep their DOM nodes across updates. Markdown
makes (a) and (b) genuinely hard — a half-arrived ``` fence or `**` delimiter
flips the meaning of everything after it.

**Where this repo stakes its life on it.** `src/markdown/` renders the LLM's
answer (§10: "Model output is UNTRUSTED"). No markdown library, no sanitizer
— the subset is written in-repo *to* the security spec.

## Structural safety, clause by clause

The renderer's header is the contract (`Markdown.tsx:1-20`):

- "Every string from the model reaches the DOM as a React TEXT NODE. There is
  no dangerouslySetInnerHTML anywhere and no code path that could add one."
  In `renderInlines` (Markdown.tsx:24-39), a text node is
  `<Fragment key={idx}>{node.text}</Fragment>` — JSX child interpolation,
  which React escapes by construction.
- "No attribute value is ever derived from model text — no id, no className,
  no title." The *one* attribute ever emitted is `<ol start>`, and its value
  is laundered at the parser: "parseInt over a digits-only capture: the start
  number that reaches the DOM is a validated integer, never model text"
  (parse.ts:129-131). Even then it is emitted only when it carries information
  (`start !== 1`), "keeping the emitted-attribute surface as small as possible
  for the XSS audit" (Markdown.tsx:69-79). The fence info string — the classic
  `language-${x}` className vector — is dropped at parse time
  (Markdown.tsx:58-59).
- "Keys come from block/child INDEX, never from content, so adversarial
  repeated text cannot collide keys and confuse reconciliation."
- **Links are deliberately not parsed** (parse.ts:9-11): `[text](url)` stays
  literal visible text. "There is no href, so there is nothing to sanitize and
  no `javascript:` to smuggle." Removing the feature removes the attack
  surface — the strongest sanitization is absence.

The XSS suite (`xss.test.tsx`) checks the property *structurally*: eight
payloads (script tag, `onerror`, fence breakout, attribute injection…) must
produce zero script/iframe/img/a elements, pass a whole-tree tag/attribute
audit — and remain **visible as literal text**, because "swallowing the
payload would hide what the model actually said." One test even asserts the
audit holds for *benign* input ("otherwise the 'safe set' rots into an
allowlist of accidents").

## The streaming invariant, and the O(doc) heresy

§10(e) permits incremental parsing "behind the invariant test" but warns that
a wrong incremental parse is worse than an honest O(doc) one. The parser
chooses honesty and says why:

```ts
// parse.ts:13-19
 * This is a FULL re-parse on every call, memoized on the exact source string
 * (see the single-slot cache at the bottom). An incremental parser that resumes
 * from the last stable block boundary was considered and rejected: a wrong
 * incremental parse silently diverges from the batch parse, which is worse than
 * an honest O(doc) one. Documents here are single LLM answers (a few KB), so
 * O(doc) per streamed frame is well within budget.
```

This is lesson 04's complexity discipline running in the *opposite* direction,
and that is the point: complexity choices follow from measured input sizes,
not reflexes. A transcript grows for 120 s at 10 Hz — incremental state wins.
An answer is a few KB — O(doc) per frame is nothing, and the incremental
parser's failure mode (silent divergence between streamed and final DOM)
attacks the exact invariant that matters. The DOM-stability requirement is
then met by memoization instead of bookkeeping: a single-slot cache keyed on
the exact source string (parse.ts:539-551) gives referential equality on
unchanged input, `memo` + `useMemo` (Markdown.tsx:85-91) turn that into a
skipped render, and deterministic parse + index keys mean completed blocks
re-render to identical elements React leaves alone.

The invariant test (`streaming.test.tsx`) walks **every cut point** of a
15-document corpus — lesson 03's exhaustive-split idea, applied to a grammar:
each prefix must render without throwing, and the fully-streamed DOM must be
byte-identical to the one-shot render. The corpus is curated hostility:
`unterminatedFence`, `fenceContainingMarkdown`, `snakeCaseIdentifiers`, plus
two audit-regression documents (`fenceInListItem`, whose pending-blank bug
"inverted fence parity" and ate the rest of the answer; `yearAtLineStart`,
below).

## Grammar decisions as product decisions

The parser's edge cases are all "what does an LLM actually emit" calls, each
pinned: `#hashtags` stay prose (models emit hashtags, parse.ts:49);
`snake_case` must never italicize — "THE bug that bites markdown renderers on
model output"; `3.14` and `-rf` are not list items (parse.ts:53-55). The
richest one is paragraph interruption:

```ts
// parse.ts:140-151 (abridged)
 * CommonMark restricts which list markers may INTERRUPT a paragraph ...
 * bullets, and ordered markers that are exactly `1.`/`1)` — and only when the
 * item has content. The rule exists for prose, not pedantry: spoken answers
 * wrap lines starting with years and figures ("…since\n1997. That year we…"),
 * and without this restriction every such line became an <ol start="1997">
 * that mangled the middle of the answer.
```

— while *after a blank line* any start number is honored, because §10 requires
`7. item` to yield `<ol start="7">` (parse.ts:149-151). One grammar rule, two
contexts, opposite behavior, both load-bearing.

## Exercises

**Reading 1.** The heading matcher demotes model `#` to `h3` and caps at `h6`
(parse.ts:71-74). This looks cosmetic. Find the two non-cosmetic reasons —
one in §10's wording, one in the type of `BlockNode.heading`.

<details><summary>Answer</summary>

§10: "the page owns h1/h2" — the app's own chrome must outrank anything the
model says; an un-demoted `#` would let every answer inject a top-level
heading into the document outline, confusing assistive-tech navigation (the
answer panel is an `aria-live` region). And the AST type is
`level: 3 | 4 | 5 | 6` (parse.ts:37) — the demotion-and-cap is what makes the
`as` cast at parse.ts:73 true, and the renderer's
`createElement(`h${block.level}`)` (Markdown.tsx:54) safe: the tag name is
derived from a value whose type admits only four strings. A seventh `#` can't
walk past `h6` into a nonsense element because `Math.min` runs before the
type is claimed. Structural safety again, at the type level.
</details>

**Reading 2.** `streaming.test.tsx` includes the test `an unchanged source is
a no-op`, asserting `parseMarkdown` returns the *referentially same* result.
Why is referential equality the assertion, rather than deep equality?

<details><summary>Answer</summary>

Deep equality would pass even if the parser re-parsed from scratch every call
— it checks the answer, not the work. Referential equality "proves the parse
was skipped, not repeated per render" (TESTING.md): the single-slot cache hit
returned the same array, which is also what lets React's `memo`/`useMemo`
bail out — reference identity is React's change signal. The streaming caller
re-renders its parent on every animation frame; without the cache, every
frame re-runs the block mapping "even when nothing changed"
(Markdown.tsx:85-87). The test is pinning a performance *mechanism*, and
reference identity is the only observable that distinguishes the mechanism
from its absence.
</details>

**Break it.** In `canItemInterruptParagraph` (parse.ts:152-157), accept every
marker:

```ts
function canItemInterruptParagraph(line: string): boolean {
  return matchListItem(line) !== null;
}
```

Run `npm test -- -t 'keeps "1997." at a wrapped line start inside the paragraph'`
— and then the streaming suite, where the `yearAtLineStart` corpus document
fails its prefix walk.

The Markdown test feeds a paragraph whose soft-wrapped second line begins
`1997. That year…` and asserts it stays *inside* the paragraph. With the
restriction gone, `matchListItem` happily reads `1997.` as an ordered marker
and the renderer emits `<ol start="1997">` mid-sentence — a real regression
this repo shipped and fixed ("a naive 'any N. interrupts' turned a
mid-sentence figure into an `<ol start="1997">` mid-answer", TESTING.md). The
streaming failure is the deeper one: the damage appears at a specific
*prefix* — the moment the stream has delivered `…since\n1997.` but not the
rest — which is exactly the class of bug the every-cut-point walk exists to
catch. Grammar leniency, like wire-input leniency in lesson 04, breaks at the
boundary where the input is most ordinary: years, prices, and section numbers
at wrapped line starts appear in spoken answers constantly; attackers are
optional, prose is not.

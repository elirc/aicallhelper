# ADR 008 — In-repo markdown renderer with links deliberately unparsed

**Status**: accepted

## Context

The answer panel renders **model output, which is untrusted** (SPEC §10). The
conventional stack — a markdown library plus a sanitizer — has a strange
shape when you look at it directly: one dependency's job is to build an HTML
string from hostile input, and a second dependency's job is to clean that
string up afterwards. Two supply chains to track CVEs on, and the correctness
of the whole thing rests on the sanitizer's default configuration staying
right across upgrades.

The needs here are narrow: spoken-style answers use paragraphs, lists, bold,
inline code, the occasional heading or fence. And the renderer must be
**streaming-safe**: it is called with a growing source string many times per
second, every prefix must render without throwing, and the final DOM must be
byte-identical to a one-shot render (SPEC §10).

## Decision

Write the renderer in-repo, with a structural rather than filter-based
security posture:

- The parser produces a **plain AST of strings** and never touches the DOM
  (`src/markdown/parse.ts:1-19`). Every string reaches the DOM as a React
  **text node**; there is no `dangerouslySetInnerHTML` and no attribute value
  is ever derived from model text — the single emitted attribute,
  `<ol start>`, is an integer parsed from a digits-only capture
  (`src/markdown/Markdown.tsx:1-20`, `65-80`). The fenced-code info string is
  dropped at parse time so a language name never becomes a class attribute
  (`Markdown.tsx:57-64`).
- **Links are deliberately NOT parsed** — `[text](url)` stays literal visible
  text (`parse.ts:9-11`). No href exists, so there is nothing to sanitize and
  no `javascript:` to smuggle. This converts the classic markdown-XSS surface
  from "filtered" to "absent".
- Headings are demoted (model `#` → `h3`, capped at `h6`) because the page
  owns h1/h2 (`parse.ts:36-37`, `62-75`).
- Streaming is handled by a **full re-parse memoized on the exact source
  string**. Incremental parsing was considered and rejected: a wrong
  incremental parse silently diverges from the batch parse, which is worse
  than an honest O(doc) one — and the documents are single few-KB answers
  (`parse.ts:13-18`). Block-index keys plus a deterministic parse mean
  completed blocks re-render to identical elements and React leaves their DOM
  alone (`Markdown.tsx:16-19`).

## Consequences

- XSS is structurally impossible in the render path, not filtered after the
  fact. The `xss.test.tsx` suite attacks it anyway; the streaming-vs-batch
  invariant is pinned in `streaming.test.tsx`.
- Zero rendering dependencies to audit or upgrade, and no CSP exception ever
  had to be added on a library's behalf. The shipped policy is
  `style-src 'self'` with no inline allowance at all (see SECURITY.md); the
  renderer emits text nodes only, so it never needed one.

Costs, honestly:

- It is a hand-built CommonMark-ish parser, and the edge cases were real
  work: flanking rules so `snake_case` doesn't italicize, lazy continuation of
  wrapped list items, exact-length backtick closers, unterminated fences at
  EOF (`parse.ts:44-55` and onward). That code is now owned here forever, with
  tests as the spec.
- The subset is a ceiling: no links, tables, or images. For spoken answers
  this costs nothing today; a genuinely useful URL renders as literal
  bracket-noise.
- A full re-parse per streamed frame is O(doc) work each paint — fine at a
  few KB, and coalesced to one paint per animation frame (SPEC §9), but it is
  a budget that long answers spend.

## If revisited

If clickable links ever become a requirement, the answer is still not a
sanitizer: parse the URL, validate the scheme against an allowlist, and route
clicks through the existing external-open guard
(`src-tauri/src/window.rs:207-243`), which already refuses everything but
clean https. If answers ever grow to tens of KB, revisit incremental parsing —
but only behind the streaming-equals-batch invariant test, as SPEC §10
insists.

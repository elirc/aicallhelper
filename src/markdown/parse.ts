/**
 * Markdown subset parser for untrusted LLM output (§10).
 *
 * The parser produces a plain AST of strings — it never touches the DOM. Every
 * string in the AST is later rendered as a React TEXT NODE, which is the whole
 * security story: there is no HTML pass-through anywhere, so there is nothing
 * to sanitize.
 *
 * DESIGN DECISION, not an omission: links are NOT parsed. `[text](url)` stays
 * literal visible text. There is no href, so there is nothing to sanitize and
 * no `javascript:` to smuggle.
 *
 * This is a FULL re-parse on every call, memoized on the exact source string
 * (see the single-slot cache at the bottom). An incremental parser that resumes
 * from the last stable block boundary was considered and rejected: a wrong
 * incremental parse silently diverges from the batch parse, which is worse than
 * an honest O(doc) one. Documents here are single LLM answers (a few KB), so
 * O(doc) per streamed frame is well within budget.
 */

/* ------------------------------------------------------------------- AST -- */

export type InlineNode =
  | { kind: 'text'; text: string }
  | { kind: 'code'; text: string }
  | { kind: 'em'; children: InlineNode[] }
  | { kind: 'strong'; children: InlineNode[] };

export interface ListItemNode {
  /** Tight items always hold exactly one paragraph; a second one forces loose. */
  paragraphs: InlineNode[][];
}

export type BlockNode =
  | { kind: 'paragraph'; children: InlineNode[] }
  /** Levels start at 3: the page owns h1/h2, so model `#` lands on h3 (§10). */
  | { kind: 'heading'; level: 3 | 4 | 5 | 6; children: InlineNode[] }
  | { kind: 'thematicBreak' }
  | { kind: 'codeBlock'; text: string }
  | { kind: 'list'; ordered: boolean; start: number; loose: boolean; items: ListItemNode[] };

/* ---------------------------------------------------------- line matchers -- */

const BLANK_RE = /^[ \t]*$/;
// Same char at least three times, spaces allowed between — checked BEFORE list
// markers so `- - -` becomes a break instead of three empty bullet items.
const THEMATIC_RE = /^ {0,3}([-_*])[ \t]*(?:\1[ \t]*){2,}$/;
// `#texting` without a space is prose, not a heading — models emit hashtags.
const HEADING_RE = /^ {0,3}(#{1,6})(?:[ \t]+(.*))?$/;
const FENCE_OPEN_RE = /^ {0,3}(`{3,}|~{3,})[ \t]*(.*)$/;
const FENCE_CLOSE_RE = /^ {0,3}(`{3,}|~{3,})[ \t]*$/;
// Marker must be followed by whitespace or end-of-line, so "3.14" and "-rf"
// stay prose instead of becoming surprise list items.
const BULLET_RE = /^( {0,3})([-+*])(?:([ \t]+)(.*))?$/;
const ORDERED_RE = /^( {0,3})(\d{1,9})([.)])(?:([ \t]+)(.*))?$/;

interface HeadingStart {
  level: 3 | 4 | 5 | 6;
  text: string;
}

function matchHeading(line: string): HeadingStart | null {
  const m = HEADING_RE.exec(line);
  if (!m) return null;
  const hashes = m[1] ?? '';
  let text = (m[2] ?? '').trimEnd();
  // A trailing run of #s is a closing sequence, not content (`## title ##`).
  const closing = /[ \t]+#+$/.exec(text);
  if (closing) text = text.slice(0, closing.index).trimEnd();
  else if (/^#+$/.test(text)) text = '';
  // Demotion: model # -> h3, capped at h6 so deep nesting cannot walk past
  // valid heading elements.
  const level = Math.min(hashes.length + 2, 6) as 3 | 4 | 5 | 6;
  return { level, text };
}

interface FenceStart {
  char: '`' | '~';
  len: number;
}

function matchFenceOpen(line: string): FenceStart | null {
  const m = FENCE_OPEN_RE.exec(line);
  if (!m) return null;
  const run = m[1] ?? '';
  const info = m[2] ?? '';
  const char = run.charAt(0) as '`' | '~';
  // A backtick in the info string means this is more plausibly an inline code
  // span (`` ```code``` `` on one line) than a fence opener.
  if (char === '`' && info.includes('`')) return null;
  return { char, len: run.length };
}

function isFenceClose(line: string, open: FenceStart): boolean {
  const m = FENCE_CLOSE_RE.exec(line);
  if (!m) return false;
  const run = m[1] ?? '';
  // The closer must be at least as long as the opener, so a fence can safely
  // CONTAIN shorter fences (e.g. a markdown tutorial inside ````).
  return run.charAt(0) === open.char && run.length >= open.len;
}

interface ItemStart {
  ordered: boolean;
  start: number;
  contentIndent: number;
  content: string;
}

function matchListItem(line: string): ItemStart | null {
  const b = BULLET_RE.exec(line);
  if (b) {
    const indent = (b[1] ?? '').length;
    const spacing = b[3] ?? '';
    return {
      ordered: false,
      start: 1,
      contentIndent: indent + 1 + Math.max(spacing.length, 1),
      content: b[4] ?? '',
    };
  }
  const o = ORDERED_RE.exec(line);
  if (o) {
    const indent = (o[1] ?? '').length;
    const digits = o[2] ?? '1';
    const spacing = o[4] ?? '';
    return {
      ordered: true,
      // parseInt over a digits-only capture: the start number that reaches the
      // DOM is a validated integer, never model text.
      start: parseInt(digits, 10),
      contentIndent: indent + digits.length + 1 + Math.max(spacing.length, 1),
      content: o[5] ?? '',
    };
  }
  return null;
}

/**
 * CommonMark restricts which list markers may INTERRUPT a paragraph (i.e. with
 * no blank line before them): bullets, and ordered markers that are exactly
 * `1.`/`1)` — and only when the item has content. The rule exists for prose,
 * not pedantry: spoken answers wrap lines starting with years and figures
 * ("…since\n1997. That year we…"), and without this restriction every such
 * line became an <ol start="1997"> that mangled the middle of the answer.
 * Empty markers can't interrupt either — a lone `-` or `2.` at a line break is
 * far more plausibly prose than a list item with nothing in it.
 *
 * Deliberately NOT applied at block start (after a blank line): §10 requires
 * start numbers there, so `7. item` on its own still yields <ol start="7">.
 */
function canItemInterruptParagraph(line: string): boolean {
  const item = matchListItem(line);
  if (item === null) return false;
  if (item.content.trim() === '') return false;
  return !item.ordered || item.start === 1;
}

/* ------------------------------------------------------------ block parse -- */

function parseBlocks(src: string): BlockNode[] {
  const lines = src.split('\n');
  // A trailing newline produces a phantom '' line that would otherwise become
  // an extra blank line INSIDE an unterminated fence.
  if (lines.length > 0 && lines[lines.length - 1] === '') lines.pop();

  const blocks: BlockNode[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? '';

    if (BLANK_RE.test(line)) {
      i++;
      continue;
    }

    const fence = matchFenceOpen(line);
    if (fence) {
      i++;
      const content: string[] = [];
      while (i < lines.length && !isFenceClose(lines[i] ?? '', fence)) {
        content.push(lines[i] ?? '');
        i++;
      }
      // No closer by EOF: the fence deliberately stays an open block, because
      // during streaming the closer simply has not arrived yet.
      if (i < lines.length) i++;
      blocks.push({ kind: 'codeBlock', text: content.length > 0 ? content.join('\n') + '\n' : '' });
      continue;
    }

    if (THEMATIC_RE.test(line)) {
      blocks.push({ kind: 'thematicBreak' });
      i++;
      continue;
    }

    const heading = matchHeading(line);
    if (heading) {
      blocks.push({ kind: 'heading', level: heading.level, children: parseInlines(heading.text) });
      i++;
      continue;
    }

    const item = matchListItem(line);
    if (item) {
      const list = parseList(lines, i, item);
      blocks.push(list.node);
      i = list.next;
      continue;
    }

    // Paragraph. Other block starts interrupt it without a blank line, because
    // LLM answers routinely glue "Header text:" straight onto a list — but
    // list markers only under the CommonMark interruption rule (see
    // canItemInterruptParagraph), so wrapped prose mentioning a year at a line
    // break stays prose.
    const para: string[] = [line.trim()];
    i++;
    while (i < lines.length) {
      const next = lines[i] ?? '';
      if (
        BLANK_RE.test(next) ||
        THEMATIC_RE.test(next) ||
        matchFenceOpen(next) !== null ||
        matchHeading(next) !== null ||
        canItemInterruptParagraph(next)
      ) {
        break;
      }
      para.push(next.trim());
      i++;
    }
    blocks.push({ kind: 'paragraph', children: parseInlines(para.join('\n')) });
  }
  return blocks;
}

/**
 * Flat lists only (no nesting): a deterministic subset beats a half-right
 * nested parser whose output shifts under streaming. Indented sub-markers are
 * treated as continuation text of the current item.
 */
function parseList(
  lines: string[],
  startIdx: number,
  first: ItemStart,
): { node: BlockNode; next: number } {
  // items -> paragraphs -> raw lines; inline parsing happens once at the end.
  let curPara: string[] = [first.content.trim()];
  let curItem: string[][] = [curPara];
  const items: string[][][] = [curItem];
  const ordered = first.ordered;
  const start = first.start;
  let contentIndent = first.contentIndent;
  let loose = false;
  let pendingBlank = false;

  let i = startIdx + 1;
  while (i < lines.length) {
    const line = lines[i] ?? '';

    if (BLANK_RE.test(line)) {
      // A blank line ends the list ONLY if no item (or item content) follows,
      // so we just remember it and decide when the next real line arrives.
      pendingBlank = true;
      i++;
      continue;
    }

    // `- - -` and `***` are breaks even mid-list; checked before the marker.
    if (THEMATIC_RE.test(line)) break;

    const m = matchListItem(line);
    if (m) {
      // A different family (bullet vs ordered) is a new list, not a new item.
      if (m.ordered !== ordered) break;
      if (pendingBlank) loose = true;
      curPara = [m.content.trim()];
      curItem = [curPara];
      items.push(curItem);
      contentIndent = m.contentIndent;
      pendingBlank = false;
      i++;
      continue;
    }

    if (pendingBlank) {
      // Fences and headings interrupt here EXACTLY as they do on the no-blank
      // path below — the asymmetry was a real bug: an indented ```python after
      // a blank line was swallowed as literal item text, and its CLOSING ```
      // (arriving with pendingBlank cleared) then hit the interrupt break and
      // OPENED a brand-new top-level fence that ate the entire remainder of
      // the answer as an unterminated code block. Breaking out here keeps
      // opener/closer parity: the opener ends the list, the fence opens at top
      // level, and the closer closes it. (Thematic breaks are already handled
      // above, before the marker check.)
      if (matchFenceOpen(line) !== null || matchHeading(line) !== null) break;
      // After a blank, only a line indented to the item's content column is a
      // second paragraph of the same item — which is what makes a list loose.
      const indent = line.length - line.trimStart().length;
      if (indent >= contentIndent) {
        loose = true;
        curPara = [line.trim()];
        curItem.push(curPara);
        pendingBlank = false;
        i++;
        continue;
      }
      break;
    }

    // Fences and headings interrupt like they interrupt paragraphs; lazy
    // continuation is only for plain wrapped text.
    if (matchFenceOpen(line) !== null || matchHeading(line) !== null) break;

    curPara.push(line.trim());
    i++;
  }

  const node: BlockNode = {
    kind: 'list',
    ordered,
    start,
    loose,
    items: items.map((item) => ({
      paragraphs: item.map((p) => parseInlines(p.join('\n'))),
    })),
  };
  return { node, next: i };
}

/* ----------------------------------------------------------- inline parse -- */

// ASCII-only punctuation is enough for flanking decisions over LLM output and
// keeps the rules auditable.
const ASCII_PUNCT = /[!-/:-@[-`{-~]/;

interface DelimTok {
  kind: 'delim';
  char: '*' | '_';
  count: number;
  /** Original run length, needed for the CommonMark "multiple of 3" rule. */
  orig: number;
  canOpen: boolean;
  canClose: boolean;
}

type WorkNode = InlineNode | DelimTok;

/** Next run of EXACTLY `len` backticks; runs of other lengths are skipped whole. */
function findBacktickRun(src: string, from: number, len: number): number {
  let i = from;
  const n = src.length;
  while (i < n) {
    if (src.charAt(i) === '`') {
      let j = i;
      while (j < n && src.charAt(j) === '`') j++;
      if (j - i === len) return i;
      i = j;
    } else {
      i++;
    }
  }
  return -1;
}

function parseInlines(src: string): InlineNode[] {
  const work: WorkNode[] = [];
  let buf = '';
  const flush = (): void => {
    if (buf !== '') {
      work.push({ kind: 'text', text: buf });
      buf = '';
    }
  };

  let i = 0;
  const n = src.length;
  while (i < n) {
    const c = src.charAt(i);

    if (c === '\\') {
      const next = src.charAt(i + 1);
      // Escaped punctuation is consumed HERE, before delimiter scanning, so an
      // escaped `*` or backtick can never open emphasis or a code span.
      if (next !== '' && ASCII_PUNCT.test(next)) {
        buf += next;
        i += 2;
        continue;
      }
      buf += '\\';
      i++;
      continue;
    }

    if (c === '`') {
      let runEnd = i;
      while (runEnd < n && src.charAt(runEnd) === '`') runEnd++;
      const len = runEnd - i;
      // The closer must be a run of EXACTLY the same length (§10); a longer or
      // shorter run cannot close this span.
      const closeAt = findBacktickRun(src, runEnd, len);
      if (closeAt === -1) {
        buf += src.slice(i, runEnd);
        i = runEnd;
        continue;
      }
      flush();
      // Newlines inside a span were soft wraps in the source, not content.
      let content = src.slice(runEnd, closeAt).replace(/\n/g, ' ');
      // One space of padding stripped from each end, so `` ` `code` ` `` can
      // show a span that itself starts with a backtick. Never strips an
      // all-space span down to nothing.
      if (
        content.length >= 2 &&
        content.startsWith(' ') &&
        content.endsWith(' ') &&
        content.trim() !== ''
      ) {
        content = content.slice(1, -1);
      }
      work.push({ kind: 'code', text: content });
      i = closeAt + len;
      continue;
    }

    if (c === '*' || c === '_') {
      let runEnd = i;
      while (runEnd < n && src.charAt(runEnd) === c) runEnd++;
      const count = runEnd - i;
      const prev = i === 0 ? '' : src.charAt(i - 1);
      const next = runEnd >= n ? '' : src.charAt(runEnd);
      const prevWs = prev === '' || /\s/.test(prev);
      const nextWs = next === '' || /\s/.test(next);
      const prevPunct = prev !== '' && ASCII_PUNCT.test(prev);
      const nextPunct = next !== '' && ASCII_PUNCT.test(next);
      const leftFlanking = !nextWs && (!nextPunct || prevWs || prevPunct);
      const rightFlanking = !prevWs && (!prevPunct || nextWs || nextPunct);
      // `_` additionally requires a word BOUNDARY on the outer side. This is
      // the rule that keeps snake_case identifiers — everywhere in model
      // answers — from italicizing.
      const canOpen = c === '*' ? leftFlanking : leftFlanking && (!rightFlanking || prevPunct);
      const canClose = c === '*' ? rightFlanking : rightFlanking && (!leftFlanking || nextPunct);
      if (canOpen || canClose) {
        flush();
        work.push({ kind: 'delim', char: c, count, orig: count, canOpen, canClose });
      } else {
        buf += src.slice(i, runEnd);
      }
      i = runEnd;
      continue;
    }

    buf += c;
    i++;
  }
  flush();
  return processEmphasis(work);
}

function delimToText(d: DelimTok): InlineNode {
  return { kind: 'text', text: d.char.repeat(d.count) };
}

/**
 * One token of the inline sequence, threaded through TWO lists at once: the
 * inline order (what gets rendered) and the stack of still-live delimiters
 * (what the opener search walks). Sharing one cell lets a pairing leave either
 * list in O(1).
 */
interface Cell {
  node: WorkNode;
  /**
   * Index in the original token order. Never renumbered, which is what lets
   * `openersBottom` hold plain numbers instead of cells that may since have
   * been unlinked. Only delimiter cells are ever compared by it.
   */
  pos: number;
  prev: Cell | null;
  next: Cell | null;
  /** Null on non-delimiters and on delimiters that have left the stack. */
  prevDelim: Cell | null;
  nextDelim: Cell | null;
}

/**
 * CommonMark's `openers_bottom` slot for a closer. Whether a candidate opener
 * matches depends on the CLOSER only through these three facts, and a
 * candidate's own facts (char, flanking, original run length) never change —
 * so once a closer has proven a region holds no opener for its slot, that
 * proof holds for every later closer in the same slot and the search can stop
 * there instead of walking back to the start of the paragraph every time.
 */
function openersBottomSlot(closer: DelimTok): number {
  return (closer.char === '*' ? 0 : 6) + (closer.canOpen ? 3 : 0) + (closer.orig % 3);
}

/**
 * CommonMark's process-emphasis. Pairing decisions are exactly the ones the
 * plain-array version made; only the cost changed, because this runs over the
 * whole answer on every streamed frame — inside the stop-to-first-word window
 * the product is built around (§10 e).
 *
 * The array version was quadratic in two independent ways, and adversarial
 * model output (a paragraph of alternating `*_` is one delimiter per
 * character) hit both: the opener search walked back over every dead token to
 * index 0, and each pair did up to three `splice`s near the FRONT of the
 * array, shifting the entire tail each time. The delimiter stack kills the
 * first (dead delimiters unlink, and `openersBottom` cuts the search off), and
 * relinking kills the second.
 */
function processEmphasis(nodes: WorkNode[]): InlineNode[] {
  let head: Cell | null = null;
  let tail: Cell | null = null;
  let firstDelim: Cell | null = null;
  let lastDelim: Cell | null = null;
  for (let pos = 0; pos < nodes.length; pos++) {
    const node = nodes[pos];
    if (node === undefined) continue;
    const cell: Cell = { node, pos, prev: tail, next: null, prevDelim: null, nextDelim: null };
    if (tail === null) head = cell;
    else tail.next = cell;
    tail = cell;
    if (node.kind === 'delim') {
      cell.prevDelim = lastDelim;
      if (lastDelim === null) firstDelim = cell;
      else lastDelim.nextDelim = cell;
      lastDelim = cell;
    }
  }

  /** Leaves the delimiter stack only: the cell still renders, as literal text. */
  const unstack = (cell: Cell): void => {
    if (cell.prevDelim !== null) cell.prevDelim.nextDelim = cell.nextDelim;
    if (cell.nextDelim !== null) cell.nextDelim.prevDelim = cell.prevDelim;
    cell.prevDelim = null;
    cell.nextDelim = null;
  };

  /** Leaves both lists: the pair consumed these characters entirely. */
  const dropCell = (cell: Cell): void => {
    if (cell.prev === null) head = cell.next;
    else cell.prev.next = cell.next;
    if (cell.next !== null) cell.next.prev = cell.prev;
    unstack(cell);
  };

  // -1 is "search all the way back"; entries are original token positions.
  const openersBottom = new Array<number>(12).fill(-1);

  let closerCell = firstDelim;
  while (closerCell !== null) {
    const closer = closerCell.node;
    if (closer.kind !== 'delim' || !closer.canClose) {
      closerCell = closerCell.nextDelim;
      continue;
    }

    const slot = openersBottomSlot(closer);
    const bottom = openersBottom[slot] ?? -1;
    let openerCell: Cell | null = null;
    let opener: DelimTok | null = null;
    for (
      let cand = closerCell.prevDelim;
      cand !== null && cand.pos > bottom;
      cand = cand.prevDelim
    ) {
      const tok = cand.node;
      if (tok.kind !== 'delim' || tok.char !== closer.char || !tok.canOpen) continue;
      // "Multiple of 3" rule: without it, `*foo**bar*` would pair the wrong
      // delimiters and emit crossed emphasis.
      const bothMultiple = tok.orig % 3 === 0 && closer.orig % 3 === 0;
      const forbidden =
        (tok.canClose || closer.canOpen) && (tok.orig + closer.orig) % 3 === 0 && !bothMultiple;
      if (forbidden) continue;
      openerCell = cand;
      opener = tok;
      break;
    }

    if (openerCell === null || opener === null) {
      // Everything from here down is now proven opener-free for this slot, so
      // record the floor. It only ever rises: delimiters are only removed,
      // never added, and the facts the match depends on never change, so the
      // proof cannot expire.
      const floor = closerCell.prevDelim === null ? -1 : closerCell.prevDelim.pos;
      if (floor > bottom) openersBottom[slot] = floor;
      const next = closerCell.nextDelim;
      // A closer that can never open is dead — freeze it as literal text so it
      // cannot be re-examined forever.
      if (!closer.canOpen) {
        closerCell.node = delimToText(closer);
        unstack(closerCell);
      }
      closerCell = next;
      continue;
    }

    const use = opener.count >= 2 && closer.count >= 2 ? 2 : 1;
    // Delimiters trapped between the pair lost their chance to match; they
    // become literal text inside the new node.
    const children: InlineNode[] = [];
    for (let inner = openerCell.next; inner !== null && inner !== closerCell; inner = inner.next) {
      const nd = inner.node;
      children.push(nd.kind === 'delim' ? delimToText(nd) : nd);
    }
    const wrapped: Cell = {
      node: use === 2 ? { kind: 'strong', children } : { kind: 'em', children },
      // Inserted cells are never delimiters, so this position is never
      // compared; it only keeps the field honest about where the node sits.
      pos: openerCell.pos,
      prev: openerCell,
      next: closerCell,
      prevDelim: null,
      nextDelim: null,
    };
    // Four assignments swallow the whole span, however long it is — including
    // every trapped delimiter's exit from the stack.
    openerCell.next = wrapped;
    closerCell.prev = wrapped;
    openerCell.nextDelim = closerCell;
    closerCell.prevDelim = openerCell;

    opener.count -= use;
    closer.count -= use;
    if (opener.count === 0) dropCell(openerCell);
    if (closer.count === 0) {
      const next = closerCell.nextDelim;
      dropCell(closerCell);
      closerCell = next;
    }
    // Otherwise the closer still has characters and may close again
    // (`***a***`), so it is deliberately re-examined in place.
  }

  const out: InlineNode[] = [];
  for (let cell = head; cell !== null; cell = cell.next) {
    const nd = cell.node;
    out.push(nd.kind === 'delim' ? delimToText(nd) : nd);
  }
  return out;
}

/* ------------------------------------------------------------ memoization -- */

let cachedSource: string | null = null;
let cachedBlocks: BlockNode[] | null = null;

/**
 * Single-slot cache keyed on the exact source string: the streaming caller
 * re-renders with an unchanged source many times per second, and referential
 * equality here lets React bail out of all block work.
 */
export function parseMarkdown(source: string): BlockNode[] {
  if (source === cachedSource && cachedBlocks !== null) return cachedBlocks;
  // CRLF from a Windows-hosted model must not defeat the line-based grammar.
  const blocks = parseBlocks(source.replace(/\r\n?/g, '\n'));
  cachedSource = source;
  cachedBlocks = blocks;
  return blocks;
}

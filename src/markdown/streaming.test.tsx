/**
 * The streaming-vs-batch invariant (§10 streaming requirements a–e).
 *
 * The renderer is fed a GROWING source many times per second. The only safe
 * contract is: every prefix renders without throwing, and once the full text
 * has arrived the DOM is byte-identical to a one-shot render. These tests walk
 * EVERY cut point, because fence/emphasis/list bugs live at the exact byte
 * where a delimiter is half-arrived.
 */
import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/react';
import { Markdown } from './index';
import { parseMarkdown } from './parse';

const CORPUS: Record<string, string> = {
  kitchenSink: [
    '# Title',
    '',
    'Intro with **bold *nested italic* inside** and snake_case names.',
    '',
    '7. seventh `` `tick` `` item',
    '8. eighth item',
    '',
    '---',
    '',
    '```',
    '# not a heading',
    '- not a list',
    '```',
    '',
    'Tail paragraph with \\*escaped stars\\* and `code`.',
  ].join('\n'),
  nestedEmphasis: 'a **bold *ital* tail** and *em **strong** tail* end',
  numberedFromSeven: 'intro\n\n7. seventh\n8. eighth\n9. ninth',
  looseList: '- alpha\n\n- beta\n\n- gamma',
  lazyContinuation: '- first line\nwrapped lazily\n- second item',
  // The fence never closes: every prefix AND the final text are an open block.
  unterminatedFence: 'before\n\n```\nconst x = 1;\nstill open at EOF',
  fenceContainingMarkdown: '```\n# heading inside\n**bold inside**\n- item inside\n```\nafter',
  headingsEveryLevel: '# one\n## two\n### three\n#### four\n##### five\n###### six',
  snakeCaseIdentifiers: 'use snake_case, SCREAMING_SNAKE_CASE and foo_bar_baz here',
  inlineCodeDoubleBacktick: 'combine `` code with ` inside `` and ` x ` padded',
  escapes: 'keep \\*stars\\*, \\_underscores\\_, \\`ticks\\` and a lone \\\\ visible',
  thematicBreaks: 'above\n\n---\n\nmiddle\n\n***\n\nbelow',
  mixedParagraphs:
    'first paragraph\nsecond line of it\n\nsecond paragraph with **bold**, *em* and `code`',
  // Regression (audit): an indented fence after a blank line inside a list
  // item once inverted fence parity — the opener was swallowed as item text
  // and the closer opened a top-level fence that ate the rest of the document.
  // Every prefix of this doc walks through that exact state.
  fenceInListItem: '- Example:\n\n  ```python\n  code\n  ```\n\nafter',
  // Regression (audit): a wrapped paragraph line starting with a year + dot
  // once interrupted the paragraph as <ol start="1997"> mid-answer.
  yearAtLineStart:
    'The company took off in\n1997. was a big year for them\n\nand a closing paragraph',
  // Regression (audit, performance): emphasis pairing was quadratic, and these
  // are its two worst shapes — one where every character opens or closes, one
  // where every closer is dead. They live in THIS suite because the fix (a
  // delimiter stack plus an openers-bottom cutoff) is only allowed if the
  // output stays byte-identical, and a half-arrived delimiter run is exactly
  // where a changed pairing decision would first show. Kept short because
  // every cut point is rendered; full-size versions live in the cost guard.
  emphasisStorm: `storm ${'*_'.repeat(24)} end`,
  deadClosers: 'a_ b_ c_ d_ e_ f_ g_ h_ i_ j_ k_ l_ m_ n_ o_ end',
};

describe('streaming-vs-batch invariant', () => {
  for (const [name, doc] of Object.entries(CORPUS)) {
    it(`every prefix renders and the stream converges: ${name}`, () => {
      const batch = render(<Markdown source={doc} />);
      const expected = batch.container.innerHTML;
      batch.unmount();

      const stream = render(<Markdown source="" />);
      for (let cut = 0; cut <= doc.length; cut++) {
        // Any throw here fails the test — requirement (a) — and the loop
        // doubles as the incremental feed for requirement (b).
        stream.rerender(<Markdown source={doc.slice(0, cut)} />);
      }
      expect(stream.container.innerHTML).toBe(expected);
    });
  }
});

describe('emphasis cost guard', () => {
  it('parses delimiter-dense answers well inside a frame budget', () => {
    // The parser re-runs over the WHOLE answer on every streamed frame, so a
    // quadratic inline pass burns the stop-to-first-word window the product is
    // built around (§10 e). Both shapes the audit found are here, at the top of
    // the size an adversarial answer can reach under max_tokens (~8 KB): one
    // delimiter per character with every pair matching, and a run of dead
    // closers that the old opener search re-walked to index 0 every time.
    const docs = [
      `storm ${'*_'.repeat(4096)} end`,
      Array.from({ length: 1300 }, (_, k) => `w${k}_`).join(' '),
    ];
    // Distinct sources on purpose: the single-slot cache would otherwise make
    // every parse after the first a pointer comparison and measure nothing.
    const started = performance.now();
    for (const doc of docs) parseMarkdown(doc);
    const elapsed = performance.now() - started;

    // Loose ON PURPOSE. CI and dev machines differ by an order of magnitude, a
    // single GC pause costs tens of milliseconds, and vitest runs suites in
    // parallel processes that contend for cores — a cold, disk-loaded run of
    // this workload was measured at ~400 ms on the dev machine even with the
    // fix in place. This asserts a complexity class, not a benchmark: the
    // quadratic version took ~1.6 s on the same machine, so 1000 ms separates
    // the two with real headroom on both sides and exists only to fail loudly
    // if the pairing loop ever goes quadratic again.
    expect(elapsed).toBeLessThan(1000);
  });
});

describe('DOM stability across updates', () => {
  it('completed blocks keep their DOM nodes as the stream grows', () => {
    // If these identities break, a user's text selection is destroyed on
    // every streamed token and the answer visibly flickers.
    const r = render(<Markdown source={'# Title\n\nfirst paragraph\n\n'} />);
    const heading = r.container.querySelector('h3');
    const para = r.container.querySelector('p');
    const textNode = para?.firstChild ?? null;
    expect(heading).not.toBeNull();
    expect(textNode).not.toBeNull();

    r.rerender(<Markdown source={'# Title\n\nfirst paragraph\n\n- item one\n'} />);
    r.rerender(<Markdown source={'# Title\n\nfirst paragraph\n\n- item one\n- item two'} />);

    expect(r.container.querySelector('h3')).toBe(heading);
    expect(r.container.querySelector('p')).toBe(para);
    expect(r.container.querySelector('p')?.firstChild).toBe(textNode);
  });

  it('keeps earlier list items stable while the last item is still streaming', () => {
    const r = render(<Markdown source={'- alpha\n- beta\n- gam'} />);
    const first = r.container.querySelectorAll('li')[0] ?? null;
    expect(first).not.toBeNull();
    r.rerender(<Markdown source={'- alpha\n- beta\n- gamma'} />);
    expect(r.container.querySelectorAll('li')[0]).toBe(first);
  });

  it('an unchanged source is a no-op', () => {
    const doc = '# stable\n\nbody text';
    // Referential equality proves the parse itself was skipped, not repeated.
    expect(parseMarkdown(doc)).toBe(parseMarkdown(doc));

    const r = render(<Markdown source={doc} />);
    const heading = r.container.querySelector('h3');
    const para = r.container.querySelector('p');
    r.rerender(<Markdown source={doc} />);
    expect(r.container.querySelector('h3')).toBe(heading);
    expect(r.container.querySelector('p')).toBe(para);
  });
});

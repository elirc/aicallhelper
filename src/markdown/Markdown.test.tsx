/**
 * Ordinary-syntax coverage. Assertions compare innerHTML where the exact DOM
 * shape is the contract (tight vs loose lists, demoted headings), and use
 * queries where only presence/absence matters.
 */
import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/react';
import { Markdown } from './index';

function html(source: string): string {
  const { container } = render(<Markdown source={source} />);
  const first = container.firstElementChild;
  return first ? first.innerHTML : '';
}

describe('headings', () => {
  it('demotes every model level by two, capped at h6', () => {
    // The page owns h1/h2; a model heading must never outrank the app chrome.
    expect(html('# one\n## two\n### three\n#### four\n##### five\n###### six')).toBe(
      '<h3>one</h3><h4>two</h4><h5>three</h5><h6>four</h6><h6>five</h6><h6>six</h6>',
    );
  });

  it('requires a space after the hashes so #hashtags stay prose', () => {
    expect(html('#nospace')).toBe('<p>#nospace</p>');
  });

  it('treats seven hashes as prose, not a heading', () => {
    expect(html('####### seven')).toBe('<p>####### seven</p>');
  });

  it('strips a closing hash run', () => {
    expect(html('## title ##')).toBe('<h4>title</h4>');
  });

  it('parses inline markdown inside a heading', () => {
    expect(html('# has **bold**')).toBe('<h3>has <strong>bold</strong></h3>');
  });
});

describe('lists', () => {
  it('preserves the start number of a numbered list', () => {
    expect(html('7. seven\n8. eight')).toBe('<ol start="7"><li>seven</li><li>eight</li></ol>');
  });

  it('emits no start attribute when the list starts at 1', () => {
    const { container } = render(<Markdown source={'1. a\n2. b'} />);
    const ol = container.querySelector('ol');
    expect(ol).not.toBeNull();
    expect(ol?.hasAttribute('start')).toBe(false);
  });

  it('supports the paren marker form', () => {
    expect(html('3) c\n4) d')).toBe('<ol start="3"><li>c</li><li>d</li></ol>');
  });

  it('accepts -, * and + bullets', () => {
    for (const m of ['-', '*', '+']) {
      expect(html(`${m} a\n${m} b`)).toBe('<ul><li>a</li><li>b</li></ul>');
    }
  });

  it('renders tight items without paragraph wrappers', () => {
    expect(html('- a\n- b')).toBe('<ul><li>a</li><li>b</li></ul>');
  });

  it('renders loose items with paragraph wrappers', () => {
    expect(html('- a\n\n- b')).toBe('<ul><li><p>a</p></li><li><p>b</p></li></ul>');
  });

  it('does NOT end a list at a blank line when another item follows', () => {
    // The failure mode: a streaming model emits items separated by blank
    // lines and the renderer shatters them into many single-item lists.
    const { container } = render(<Markdown source={'- a\n\n- b\n- c'} />);
    expect(container.querySelectorAll('ul').length).toBe(1);
    expect(container.querySelectorAll('li').length).toBe(3);
  });

  it('ends the list at a blank line followed by non-item text', () => {
    const { container } = render(<Markdown source={'- a\n\nafter'} />);
    expect(container.querySelectorAll('ul').length).toBe(1);
    expect(container.querySelectorAll('li').length).toBe(1);
    expect(container.querySelector('p')?.textContent).toBe('after');
  });

  it('lazily continues a wrapped item onto the next line', () => {
    expect(html('- first line\nwrapped lazily\n- second')).toBe(
      '<ul><li>first line\nwrapped lazily</li><li>second</li></ul>',
    );
  });

  it('keeps an indented second paragraph inside its item and goes loose', () => {
    expect(html('- a\n\n  more\n- b')).toBe(
      '<ul><li><p>a</p><p>more</p></li><li><p>b</p></li></ul>',
    );
  });

  it('starts a new list when the family changes', () => {
    const { container } = render(<Markdown source={'1. one\n- bullet'} />);
    expect(container.querySelectorAll('ol').length).toBe(1);
    expect(container.querySelectorAll('ul').length).toBe(1);
  });

  it('lets a list interrupt a paragraph without a blank line', () => {
    // Models constantly glue "Here are the steps:" straight onto item one.
    const { container } = render(<Markdown source={'Steps:\n- one\n- two'} />);
    expect(container.querySelector('p')?.textContent).toBe('Steps:');
    expect(container.querySelectorAll('li').length).toBe(2);
  });

  it('lets a `1.` item interrupt a paragraph without a blank line', () => {
    // CommonMark allows exactly `1.`/`1)` (plus bullets) to interrupt.
    expect(html('Steps:\n1. one\n2. two')).toBe(
      '<p>Steps:</p><ol><li>one</li><li>two</li></ol>',
    );
  });

  it('keeps "1997." at a wrapped line start inside the paragraph', () => {
    // CommonMark's interruption rule exists precisely for this: prose wraps at
    // a year or figure, and a naive "any N. interrupts" turned the rest of a
    // spoken answer into an <ol start="1997"> mid-flow.
    expect(html('The company was founded in\n1997. was a big year for us')).toBe(
      '<p>The company was founded in\n1997. was a big year for us</p>',
    );
    expect(html('as shown in\n2019) which grew fast')).toBe(
      '<p>as shown in\n2019) which grew fast</p>',
    );
  });

  it('still starts an <ol> at any number after a blank line', () => {
    // Block-start behavior is unchanged: §10 requires start numbers, so only
    // the mid-paragraph interruption path is restricted to `1.`.
    expect(html('para\n\n7. item')).toBe('<p>para</p><ol start="7"><li>item</li></ol>');
  });

  it('does not let an empty marker interrupt a paragraph', () => {
    // A lone `-` or `1.` at a line break is prose (CommonMark: only non-empty
    // items interrupt); an empty surprise <li> mid-answer helps nobody.
    expect(html('text\n-')).toBe('<p>text\n-</p>');
    expect(html('text\n1.')).toBe('<p>text\n1.</p>');
  });

  it('lets an indented fence after a blank line interrupt the list', () => {
    // Regression (audit): the pending-blank branch used to swallow the
    // indented ```python as literal item text; the CLOSING ``` then opened a
    // brand-new top-level fence that ate the entire rest of the answer.
    const { container } = render(
      <Markdown source={'- Example:\n\n  ```python\n  code\n  ```\n\nafter'} />,
    );
    // The fence content is a real code block (opener/closer parity restored)…
    expect(container.querySelectorAll('pre').length).toBe(1);
    expect(container.querySelector('pre > code')?.textContent).toBe('  code\n');
    // …the item itself keeps only its own text…
    expect(container.querySelector('li')?.textContent).toBe('Example:');
    // …and the trailing text is an ordinary paragraph, not swallowed by an
    // unterminated fence.
    const after = Array.from(container.querySelectorAll('p')).find(
      (p) => p.textContent === 'after',
    );
    expect(after).toBeDefined();
    expect(after?.closest('pre')).toBeNull();
  });

  it('does not mistake decimals or flags for list markers', () => {
    expect(html('3.14 is pi')).toBe('<p>3.14 is pi</p>');
    expect(html('-rf is a flag')).toBe('<p>-rf is a flag</p>');
  });
});

describe('fenced code blocks', () => {
  it('drops the info string instead of turning model text into a class', () => {
    const { container } = render(<Markdown source={'```js\nconst x = 1;\n```'} />);
    const code = container.querySelector('pre > code');
    expect(code?.textContent).toBe('const x = 1;\n');
    expect(code?.attributes.length).toBe(0);
  });

  it('supports tilde fences', () => {
    expect(html('~~~\ntext\n~~~')).toBe('<pre><code>text\n</code></pre>');
  });

  it('keeps an unterminated fence open to EOF', () => {
    expect(html('```\nabc')).toBe('<pre><code>abc\n</code></pre>');
  });

  it('requires the closer to be at least as long as the opener', () => {
    // A short run inside a longer fence is CONTENT (nested fence example).
    expect(html('````\ncode\n```\nmore\n````')).toBe('<pre><code>code\n```\nmore\n</code></pre>');
  });

  it('leaves markdown inside a fence completely inert', () => {
    const { container } = render(
      <Markdown source={'```\n# not a heading\n**not bold**\n- not a list\n```'} />,
    );
    expect(container.querySelector('h3')).toBeNull();
    expect(container.querySelector('strong')).toBeNull();
    expect(container.querySelector('li')).toBeNull();
    expect(container.querySelector('code')?.textContent).toBe(
      '# not a heading\n**not bold**\n- not a list\n',
    );
  });

  it('renders an empty fence as an empty block', () => {
    expect(html('```\n```')).toBe('<pre><code></code></pre>');
  });
});

describe('inline code', () => {
  it('renders a simple span', () => {
    expect(html('`code`')).toBe('<p><code>code</code></p>');
  });

  it('lets a double-backtick span contain a single backtick', () => {
    expect(html('``has ` tick``')).toBe('<p><code>has ` tick</code></p>');
  });

  it('requires the closer run to match the opener length exactly', () => {
    // `` opened, only ` available: nothing closes, all backticks stay text.
    expect(html('``x`')).toBe('<p>``x`</p>');
  });

  it('strips one space of padding from each end', () => {
    expect(html('` x `')).toBe('<p><code>x</code></p>');
    expect(html('`` ` ``')).toBe('<p><code>`</code></p>');
  });

  it('does not strip an all-space span to nothing', () => {
    expect(html('a ` ` b')).toBe('<p>a <code> </code> b</p>');
  });

  it('leaves an unclosed backtick literal', () => {
    expect(html('`unclosed')).toBe('<p>`unclosed</p>');
  });

  it('protects emphasis characters inside a span', () => {
    expect(html('`a *b* c`')).toBe('<p><code>a *b* c</code></p>');
  });
});

describe('emphasis', () => {
  it('renders bold, italic, and both', () => {
    expect(html('**bold**')).toBe('<p><strong>bold</strong></p>');
    expect(html('*em*')).toBe('<p><em>em</em></p>');
    expect(html('***both***')).toBe('<p><em><strong>both</strong></em></p>');
    expect(html('__bold__')).toBe('<p><strong>bold</strong></p>');
    expect(html('_em_')).toBe('<p><em>em</em></p>');
  });

  it('never italicizes snake_case identifiers', () => {
    // THE bug that bites markdown renderers on model output: identifiers are
    // everywhere and naive `_` handling shreds them.
    expect(html('use snake_case and foo_bar_baz here')).toBe(
      '<p>use snake_case and foo_bar_baz here</p>',
    );
    expect(html('SCREAMING_SNAKE_CASE')).toBe('<p>SCREAMING_SNAKE_CASE</p>');
  });

  it('allows intraword asterisk emphasis but not intraword underscore', () => {
    expect(html('in*tra*word')).toBe('<p>in<em>tra</em>word</p>');
    expect(html('in_tra_word')).toBe('<p>in_tra_word</p>');
  });

  it('supports nesting in both directions', () => {
    expect(html('**bold *ital* tail**')).toBe(
      '<p><strong>bold <em>ital</em> tail</strong></p>',
    );
    expect(html('*em **strong** tail*')).toBe('<p><em>em <strong>strong</strong> tail</em></p>');
  });

  it('applies the multiple-of-3 rule so runs do not cross-pair', () => {
    expect(html('*foo**bar*')).toBe('<p><em>foo**bar</em></p>');
  });

  it('leaves a partly-consumed opener as literal text', () => {
    expect(html('**foo*')).toBe('<p>*<em>foo</em></p>');
  });

  it('leaves space-surrounded and unclosed delimiters literal', () => {
    expect(html('a * b * c')).toBe('<p>a * b * c</p>');
    expect(html('*not closed')).toBe('<p>*not closed</p>');
  });
});

describe('backslash escapes', () => {
  it('disarms emphasis and code delimiters', () => {
    expect(html('\\*literal\\*')).toBe('<p>*literal*</p>');
    expect(html('\\`no code\\`')).toBe('<p>`no code`</p>');
    expect(html('\\_flat\\_')).toBe('<p>_flat_</p>');
  });

  it('keeps the backslash before non-punctuation', () => {
    expect(html('C:\\Users\\Owner')).toBe('<p>C:\\Users\\Owner</p>');
  });

  it('escapes a backslash itself', () => {
    expect(html('a \\\\ b')).toBe('<p>a \\ b</p>');
  });

  it('a leading escaped hash is not a heading', () => {
    const { container } = render(<Markdown source={'\\# not a heading'} />);
    expect(container.querySelector('h3')).toBeNull();
    expect(container.querySelector('p')?.textContent).toBe('# not a heading');
  });
});

describe('thematic breaks', () => {
  it('accepts the three characters and spaced forms', () => {
    for (const src of ['---', '***', '___', '- - -', '*  *  *']) {
      const { container } = render(<Markdown source={src} />);
      expect(container.querySelector('hr'), src).not.toBeNull();
      expect(container.querySelector('li'), src).toBeNull();
    }
  });

  it('rejects two-character runs', () => {
    expect(html('--')).toBe('<p>--</p>');
  });
});

describe('paragraphs and links', () => {
  it('keeps soft-wrapped lines in one paragraph and splits on blanks', () => {
    expect(html('line one\nline two\n\nsecond para')).toBe(
      '<p>line one\nline two</p><p>second para</p>',
    );
  });

  it('renders [text](url) as literal visible text with no anchor', () => {
    // Links are deliberately unsupported: no href exists, so there is no URL
    // scheme to sanitize.
    const { container } = render(<Markdown source={'[click](https://example.com)'} />);
    expect(container.querySelector('a')).toBeNull();
    expect(container.querySelector('p')?.textContent).toBe('[click](https://example.com)');
  });

  it('renders the empty string as an empty container', () => {
    const { container } = render(<Markdown source="" />);
    expect(container.firstElementChild?.childElementCount).toBe(0);
    expect(container.textContent).toBe('');
  });

  it('normalizes CRLF input', () => {
    expect(html('# hi\r\n\r\ntext')).toBe('<h3>hi</h3><p>text</p>');
  });
});

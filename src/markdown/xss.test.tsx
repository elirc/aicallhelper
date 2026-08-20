/**
 * The XSS suite. Model output is UNTRUSTED; the renderer's guarantee is
 * structural (every string becomes a text node), and these tests audit the
 * whole rendered tree rather than trusting the implementation: every element
 * must come from the small tag set we emit, and no element may carry any
 * attribute beyond the validated integer <ol start>.
 */
import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/react';
import { Markdown } from './index';

const EMITTED_TAGS = new Set([
  'DIV',
  'P',
  'H3',
  'H4',
  'H5',
  'H6',
  'UL',
  'OL',
  'LI',
  'PRE',
  'CODE',
  'EM',
  'STRONG',
  'HR',
]);

/** Collects violations instead of asserting per-node, so a failure names them all. */
function auditTree(container: HTMLElement): string[] {
  const violations: string[] = [];
  for (const el of Array.from(container.querySelectorAll('*'))) {
    if (!EMITTED_TAGS.has(el.tagName)) {
      violations.push(`unexpected element <${el.tagName.toLowerCase()}>`);
    }
    for (const attr of Array.from(el.attributes)) {
      const allowed = el.tagName === 'OL' && attr.name === 'start' && /^\d+$/.test(attr.value);
      if (!allowed) {
        violations.push(`<${el.tagName.toLowerCase()}> has attribute ${attr.name}="${attr.value}"`);
      }
    }
  }
  return violations;
}

interface Payload {
  name: string;
  source: string;
  /** The text a user must SEE — the attack surfaced as visible literal text. */
  visible: string;
}

const PAYLOADS: Payload[] = [
  {
    name: 'script tag',
    source: '<script>alert(1)</script>',
    visible: '<script>alert(1)</script>',
  },
  {
    name: 'img onerror',
    source: '<img src=x onerror=alert(1)>',
    visible: '<img src=x onerror=alert(1)>',
  },
  {
    name: 'fence breakout',
    // The classic: close the <pre> "from inside" the code block. The fence
    // content must stay a text node inside <code>, never markup.
    source: '```\n</pre><script>alert(1)</script>\n```',
    visible: '</pre><script>alert(1)</script>',
  },
  {
    name: 'javascript: link',
    source: '[click](javascript:alert(1))',
    visible: '[click](javascript:alert(1))',
  },
  {
    name: 'attribute injection via quotes',
    source: '" onmouseover="alert(1)',
    visible: '" onmouseover="alert(1)',
  },
  {
    name: 'iframe',
    source: '<iframe src="https://evil.example/steal"></iframe>',
    visible: '<iframe src="https://evil.example/steal"></iframe>',
  },
  {
    name: 'html comment',
    // A real comment node would be invisible — the user must SEE the bytes.
    source: '<!-- hidden --> visible tail',
    visible: '<!-- hidden --> visible tail',
  },
  {
    name: 'comment plus handler element',
    source: '<!--x--><b onclick=alert(1)>y</b>',
    visible: '<!--x--><b onclick=alert(1)>y</b>',
  },
];

describe('XSS payloads render as inert visible text', () => {
  for (const { name, source, visible } of PAYLOADS) {
    it(name, () => {
      const { container } = render(<Markdown source={source} />);

      expect(container.querySelectorAll('script,iframe,img,a').length).toBe(0);
      expect(auditTree(container)).toEqual([]);
      // The payload must not vanish either: silently swallowing text hides
      // what the model actually said.
      expect(container.textContent).toContain(visible);
    });
  }

  it('a fence-breakout payload stays inside the code element', () => {
    const { container } = render(
      <Markdown source={'```\n</pre><script>alert(1)</script>\n```'} />,
    );
    expect(container.querySelector('pre > code')?.textContent).toBe(
      '</pre><script>alert(1)</script>\n',
    );
  });

  it('no comment nodes are ever created', () => {
    const { container } = render(<Markdown source={'<!-- hidden -->'} />);
    const walker = document.createTreeWalker(container, NodeFilter.SHOW_COMMENT);
    expect(walker.nextNode()).toBeNull();
  });

  it('emits zero attributes on ordinary rich output', () => {
    // The audit must hold for BENIGN input too, or a "safe set" would rot into
    // an allowlist of accidents.
    const doc =
      '# h\n\npara **b** *i* `c` snake_case\n\n7. a\n8. b\n\n- x\n- y\n\n```js\ncode\n```\n\n---\n';
    const { container } = render(<Markdown source={doc} />);
    expect(auditTree(container)).toEqual([]);
  });

  it('event-handler attributes are absent from every element', () => {
    // Redundant with the attribute audit, but this is the assertion a security
    // review greps for, so it exists by name.
    const { container } = render(
      <Markdown source={'<img src=x onerror=alert(1)> " onmouseover="alert(1)'} />,
    );
    for (const el of Array.from(container.querySelectorAll('*'))) {
      for (const attr of Array.from(el.attributes)) {
        expect(attr.name.startsWith('on')).toBe(false);
      }
    }
  });
});

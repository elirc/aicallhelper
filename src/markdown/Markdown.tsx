/**
 * Renders the LLM's answer as a markdown subset (§10). MODEL OUTPUT IS
 * UNTRUSTED, so the security posture is structural, not filter-based:
 *
 * - Every string from the model reaches the DOM as a React TEXT NODE. There is
 *   no dangerouslySetInnerHTML anywhere and no code path that could add one.
 * - No attribute value is ever derived from model text — no id, no className,
 *   no title. The single attribute we emit, <ol start>, is an integer parsed
 *   from a digits-only capture, never a pass-through string.
 * - Keys come from block/child INDEX, never from content, so adversarial
 *   repeated text cannot collide keys and confuse reconciliation.
 *
 * Links are deliberately NOT parsed (see parse.ts): `[text](url)` stays
 * literal visible text, so there is no href to sanitize.
 *
 * Streaming: this is a full parse memoized on the source string (parse.ts
 * explains why incremental parsing was rejected). Block-index keys plus a
 * deterministic parse mean completed blocks re-render to identical elements,
 * so React leaves their DOM nodes untouched — no flicker, no lost selection.
 */
import { createElement, Fragment, memo, useMemo, type ReactNode } from 'react';
import { parseMarkdown, type BlockNode, type InlineNode, type ListItemNode } from './parse';

function renderInlines(nodes: InlineNode[]): ReactNode {
  return nodes.map((node, idx) => {
    switch (node.kind) {
      case 'text':
        // Fragment exists only to carry the positional key; the string itself
        // is rendered as a text node by JSX.
        return <Fragment key={idx}>{node.text}</Fragment>;
      case 'code':
        return <code key={idx}>{node.text}</code>;
      case 'em':
        return <em key={idx}>{renderInlines(node.children)}</em>;
      case 'strong':
        return <strong key={idx}>{renderInlines(node.children)}</strong>;
    }
  });
}

function renderItem(item: ListItemNode, loose: boolean): ReactNode {
  // Tight items render bare so no <p> margin appears; the parser guarantees a
  // tight list item holds exactly one paragraph.
  if (!loose) return renderInlines(item.paragraphs[0] ?? []);
  return item.paragraphs.map((p, idx) => <p key={idx}>{renderInlines(p)}</p>);
}

function renderBlock(block: BlockNode, key: number): ReactNode {
  switch (block.kind) {
    case 'paragraph':
      return <p key={key}>{renderInlines(block.children)}</p>;
    case 'heading':
      // Levels arrive pre-demoted (h3..h6) from the parser.
      return createElement(`h${block.level}`, { key }, renderInlines(block.children));
    case 'thematicBreak':
      return <hr key={key} />;
    case 'codeBlock':
      // The info string was dropped at parse time: a language name is model
      // text and must not become a class attribute.
      return (
        <pre key={key}>
          <code>{block.text}</code>
        </pre>
      );
    case 'list': {
      const items = block.items.map((item, idx) => (
        <li key={idx}>{renderItem(item, block.loose)}</li>
      ));
      if (block.ordered) {
        // Only emit start when it carries information, keeping the emitted
        // attribute surface as small as possible for the XSS audit.
        return block.start !== 1 ? (
          <ol key={key} start={block.start}>
            {items}
          </ol>
        ) : (
          <ol key={key}>{items}</ol>
        );
      }
      return <ul key={key}>{items}</ul>;
    }
  }
}

// memo makes an unchanged source a true no-op: the streaming caller re-renders
// its parent every frame, and without this every frame would re-run the block
// mapping even when nothing changed.
const MarkdownInner = memo(function MarkdownInner({ source }: { source: string }): JSX.Element {
  const blocks = useMemo(() => parseMarkdown(source), [source]);
  return <div>{blocks.map((block, idx) => renderBlock(block, idx))}</div>;
});

export function Markdown({ source }: { source: string }): JSX.Element {
  return <MarkdownInner source={source} />;
}

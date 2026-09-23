import { describe, expect, it, vi } from 'vitest';
import { act, render, screen, fireEvent, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ComponentProps } from 'react';
import { formatLatency, latencyTitle } from '../format';
import type { Metrics } from '../types';
import { AnswerPanel } from './AnswerPanel';
import { makeEntry } from '../views/testUtils';

type PanelProps = ComponentProps<typeof AnswerPanel>;

function renderPanel(overrides: Partial<PanelProps> = {}) {
  const props: PanelProps = {
    viewed: makeEntry(),
    streaming: false,
    canRegenerate: false,
    onRegenerate: vi.fn(),
    onCopyError: vi.fn(),
    historyLength: 1,
    viewIndex: 0,
    idle: true,
    onPrev: vi.fn(),
    onNext: vi.fn(),
    onClear: vi.fn(),
    streamFollow: 'tail',
    ...overrides,
  };
  const utils = render(<AnswerPanel {...props} />);
  const body = utils.container.querySelector('.answer-body') as HTMLDivElement;
  return {
    ...utils,
    props,
    body,
    rerenderPanel: (next: Partial<PanelProps>) => {
      Object.assign(props, next);
      utils.rerender(<AnswerPanel {...props} />);
    },
  };
}

/**
 * Flush the animation frame the panel batches paints on. jsdom's rAF runs on
 * its own ~16ms frame clock, NOT the timer queue, so a 0ms setTimeout only
 * catches a pending frame when the machine happens to be slow — a flake. rAF
 * callbacks run in scheduling order, so awaiting a frame of our own
 * guarantees any frame the panel queued earlier has already fired.
 */
async function frame() {
  await act(async () => {
    await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
  });
}

/** jsdom has no layout; pin the geometry the stick-to-bottom math reads. */
function mockGeometry(el: HTMLElement, scrollHeight: number, clientHeight: number) {
  Object.defineProperty(el, 'scrollHeight', { configurable: true, value: scrollHeight });
  Object.defineProperty(el, 'clientHeight', { configurable: true, value: clientHeight });
}

describe('content states', () => {
  it('shows the placeholder when no entry exists', () => {
    renderPanel({ viewed: null });
    expect(screen.getByText('Your AI-suggested answer will stream here.')).toBeInTheDocument();
  });

  it('marks streaming with the generating tag and aria-busy', () => {
    const { body } = renderPanel({ streaming: true });
    expect(screen.getByText('generating…')).toBeInTheDocument();
    expect(body).toHaveAttribute('aria-busy', 'true');
    expect(body).toHaveAttribute('aria-live', 'polite');
  });

  it('drops aria-busy when the stream settles', () => {
    const { body } = renderPanel({ streaming: false });
    expect(body).toHaveAttribute('aria-busy', 'false');
    expect(screen.queryByText('generating…')).not.toBeInTheDocument();
  });

  it('shows the latency chip with the metrics tooltip once metrics exist', () => {
    const metrics: Metrics = { sttFinalizeMs: 150, firstTokenMs: 850, totalMs: 2100 };
    renderPanel({ viewed: makeEntry({ metrics }) });
    const chip = screen.getByText(`${formatLatency(850)} to first word`);
    expect(chip).toHaveAttribute('title', latencyTitle(metrics));
  });

  it('hides the latency chip while metrics are missing', () => {
    renderPanel({ viewed: makeEntry({ metrics: null }) });
    expect(screen.queryByText(/to first word/)).not.toBeInTheDocument();
  });
});

describe('entry outcome (R2)', () => {
  it('labels an interrupted answer incomplete and keeps its text and reason', () => {
    // The reason rides on the ENTRY, so it survives the next question
    // clearing the global error box.
    renderPanel({
      viewed: makeEntry({
        answer: 'Half an',
        status: 'incomplete',
        reason: 'Anthropic stopped sending before the answer was finished, so it is incomplete. Try again.',
      }),
    });
    expect(screen.getByText('Half an')).toBeInTheDocument();
    const tag = screen.getByText('incomplete');
    expect(tag).toHaveClass('status-incomplete');
    expect(tag).toHaveAttribute('title', expect.stringContaining('before the answer was finished'));
    expect(screen.getByText(/before the answer was finished/, { selector: 'p' })).toBeInTheDocument();
  });

  it('labels a token-capped answer "cut short", separately from the timing chip', () => {
    const metrics: Metrics = { sttFinalizeMs: 0, firstTokenMs: 700, totalMs: 4000 };
    renderPanel({
      viewed: makeEntry({ status: 'limited', reason: 'Cut short: the answer reached its length limit.', metrics }),
    });
    expect(screen.getByText('cut short')).toHaveClass('status-limited');
    expect(screen.getByText('Cut short: the answer reached its length limit.')).toBeInTheDocument();
    // Timing stays in its own chip; the outcome never replaces it.
    expect(screen.getByText(`${formatLatency(700)} to first word`)).toBeInTheDocument();
  });

  it('labels a replaced answer "stopped"', () => {
    renderPanel({ viewed: makeEntry({ status: 'cancelled', reason: 'Stopped: a newer question replaced this one.' }) });
    expect(screen.getByText('stopped')).toHaveClass('status-cancelled');
  });

  it('shows no outcome label for a completed or still-streaming entry', () => {
    const { rerenderPanel } = renderPanel({ viewed: makeEntry({ status: 'completed' }) });
    expect(screen.queryByText(/^(incomplete|cut short|stopped)$/)).toBeNull();
    rerenderPanel({ viewed: makeEntry({ status: 'pending', answer: 'Use' }), streaming: true });
    expect(screen.queryByText(/^(incomplete|cut short|stopped)$/)).toBeNull();
    expect(document.querySelector('.answer-caption')).toBeNull();
  });
});

describe('regenerate', () => {
  it('renders and fires only when allowed', async () => {
    const user = userEvent.setup();
    const { props, rerenderPanel } = renderPanel({ canRegenerate: true });
    await user.click(screen.getByRole('button', { name: 'Regenerate' }));
    expect(props.onRegenerate).toHaveBeenCalledTimes(1);
    rerenderPanel({ canRegenerate: false });
    expect(screen.queryByRole('button', { name: 'Regenerate' })).not.toBeInTheDocument();
  });
});

describe('copy', () => {
  it('copies the markdown source and shows Copied ✓ briefly', async () => {
    const user = userEvent.setup();
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    renderPanel({ viewed: makeEntry({ answer: '- a\n- b' }) });
    await user.click(screen.getByRole('button', { name: 'Copy' }));
    expect(writeText).toHaveBeenCalledWith('- a\n- b');
    expect(await screen.findByText('Copied ✓')).toBeInTheDocument();
    // Screen readers get told too — the visual swap alone is silent.
    expect(screen.getByText('Answer copied to clipboard')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText('Copied ✓')).not.toBeInTheDocument(), {
      timeout: 3000,
    });
  });

  // The live region must PRE-EXIST its content (several SR/browser combos
  // ignore a region inserted together with its text) and must be wiped
  // between copies — a second identical write is a DOM no-op React skips, so
  // only a cleared region makes the next copy audible.
  it('announces every copy: text lands, clears, and lands again', async () => {
    const user = userEvent.setup();
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const { container } = renderPanel({ viewed: makeEntry({ answer: 'the answer' }) });
    const region = container.querySelector('[role="status"]') as HTMLElement;
    expect(region).toBeInTheDocument();
    expect(region.textContent).toBe('');

    await user.click(screen.getByRole('button', { name: 'Copy' }));
    expect(region.textContent).toBe('Answer copied to clipboard');
    await waitFor(() => expect(region.textContent).toBe(''), { timeout: 3000 });

    await user.click(screen.getByRole('button', { name: 'Copy' }));
    expect(region.textContent).toBe('Answer copied to clipboard');
  });

  it('routes a clipboard failure to the error box callback', async () => {
    const user = userEvent.setup();
    const writeText = vi.fn(async () => {
      throw new Error('denied');
    });
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const { props } = renderPanel();
    await user.click(screen.getByRole('button', { name: 'Copy' }));
    expect(props.onCopyError).toHaveBeenCalledWith(
      expect.objectContaining({ code: 'internal' })
    );
    expect(screen.queryByText('Copied ✓')).not.toBeInTheDocument();
  });

  it('is hidden while there is no answer yet', () => {
    renderPanel({ viewed: makeEntry({ answer: '' }) });
    expect(screen.queryByRole('button', { name: 'Copy' })).not.toBeInTheDocument();
  });
});

describe('frame-coalesced streaming', () => {
  it('renders at most one update per animation frame and lands on the latest text', async () => {
    const entry = makeEntry({ answer: 'Hello' });
    const { rerenderPanel } = renderPanel({ viewed: entry, streaming: true });
    await frame(); // settle the mount-scheduled frame
    expect(screen.getByText('Hello')).toBeInTheDocument();

    rerenderPanel({ viewed: { ...entry, answer: 'Hello there' } });
    rerenderPanel({ viewed: { ...entry, answer: 'Hello there world' } });
    // Both deltas arrived inside one frame: the DOM must still show the old
    // text, not an intermediate paint per delta.
    expect(screen.getByText('Hello')).toBeInTheDocument();
    expect(screen.queryByText('Hello there world')).not.toBeInTheDocument();

    await frame();
    expect(screen.getByText('Hello there world')).toBeInTheDocument();
  });

  it('renders immediately once the stream is over', () => {
    const entry = makeEntry({ answer: 'partial' });
    const { rerenderPanel } = renderPanel({ viewed: entry, streaming: true });
    rerenderPanel({ viewed: { ...entry, answer: 'final text' }, streaming: false });
    // No frame flush: completion must not lag behind by a frame.
    expect(screen.getByText('final text')).toBeInTheDocument();
  });
});

describe('autoscroll', () => {
  it('sticks to the bottom when the reader is within the threshold', async () => {
    const entry = makeEntry({ answer: 'line' });
    const { body, rerenderPanel } = renderPanel({ viewed: entry, streaming: true });
    await frame();
    mockGeometry(body, 500, 100);
    body.scrollTop = 380; // 500 - 380 - 100 = 20px from the bottom — following
    fireEvent.scroll(body);

    rerenderPanel({ viewed: { ...entry, answer: 'line plus more' } });
    await frame();
    expect(body.scrollTop).toBe(400);
  });

  it('leaves a reader who scrolled up alone', async () => {
    const entry = makeEntry({ answer: 'line' });
    const { body, rerenderPanel } = renderPanel({ viewed: entry, streaming: true });
    await frame();
    mockGeometry(body, 500, 100);
    body.scrollTop = 100; // 300px above the bottom — re-reading
    fireEvent.scroll(body);

    rerenderPanel({ viewed: { ...entry, answer: 'line plus more' } });
    await frame();
    // Not yanked down by the new tokens.
    expect(body.scrollTop).toBe(100);
  });

  it('resets to the top when switching history entries', async () => {
    const first = makeEntry({ key: 'a', answer: 'first answer' });
    const { body, rerenderPanel } = renderPanel({ viewed: first });
    mockGeometry(body, 500, 100);
    body.scrollTop = 400;
    fireEvent.scroll(body);

    rerenderPanel({ viewed: makeEntry({ key: 'b', answer: 'second answer' }) });
    expect(body.scrollTop).toBe(0);
    expect(screen.getByText('second answer')).toBeInTheDocument();
  });

  /**
   * Geometry that tracks the COMMITTED content the way real layout would.
   * The guard under test is "never measure the previous entry's DOM": static
   * mocked numbers could not tell a measurement of the old document from a
   * measurement of the new one.
   */
  function mockContentGeometry(el: HTMLElement, clientHeight: number, heightOf: (text: string) => number) {
    Object.defineProperty(el, 'clientHeight', { configurable: true, value: clientHeight });
    Object.defineProperty(el, 'scrollHeight', {
      configurable: true,
      get: () => heightOf(el.textContent ?? ''),
    });
  }

  it('resets to top on entry switch even when the previous entry fit entirely', () => {
    const shortEntry = makeEntry({ key: 'a', answer: 'short' });
    const longEntry = makeEntry({ key: 'b', answer: 'a much longer answer' });
    const { body, rerenderPanel } = renderPanel({ viewed: shortEntry });
    // Short content fits (80 < 100) so "following" is true; the long entry
    // overflows to 500.
    mockContentGeometry(body, 100, (text) => (text.includes('longer') ? 500 : 80));

    rerenderPanel({ viewed: longEntry });
    expect(screen.getByText('a much longer answer')).toBeInTheDocument();
    // §9: "Switching history entries resets scroll to top". Deriving
    // nearBottom from the OLD (fitting) DOM used to bottom-stick the long
    // entry to 400 instead.
    expect(body.scrollTop).toBe(0);
  });

  it('re-engages stick-to-bottom for a new stream after viewing a long answer', async () => {
    const longEntry = makeEntry({ key: 'a', answer: 'a much longer answer' });
    const { body, rerenderPanel } = renderPanel({ viewed: longEntry });
    mockContentGeometry(body, 100, (text) =>
      text.includes('longer') ? 500 : text.includes('streamed') ? 150 : 60
    );
    // The reader parked at the top of the long answer — not following.
    body.scrollTop = 0;
    fireEvent.scroll(body);

    // A new session pushes a fresh, still-empty live entry…
    const liveEntry = makeEntry({ key: 'b', question: 'next', answer: '' });
    rerenderPanel({ viewed: liveEntry, streaming: true });
    // …and the first tokens stream in.
    rerenderPanel({ viewed: { ...liveEntry, answer: 'streamed tokens' } });
    await frame();
    // nearBottom measured against the OLD long DOM stayed false forever, so
    // the new answer never followed; re-derived against the fresh (empty)
    // entry it sticks: 150 - 100.
    expect(body.scrollTop).toBe(50);
  });
});

describe('history navigation in the head', () => {
  it('is hidden below two entries', () => {
    renderPanel({ historyLength: 1 });
    expect(screen.queryByRole('navigation', { name: 'Answer history' })).not.toBeInTheDocument();
  });

  // §9: prev/next sit in the answer panel's head, next to what they page,
  // before Regenerate/Copy.
  it('renders inside the panel head, before Regenerate and Copy, and fires the callbacks', async () => {
    const user = userEvent.setup();
    const { container, props } = renderPanel({ historyLength: 3, viewIndex: 1, canRegenerate: true });
    const head = container.querySelector('.answer-panel .panel-head') as HTMLElement;
    const nav = within(head).getByRole('navigation', { name: 'Answer history' });
    expect(within(nav).getByText('2/3')).toBeInTheDocument();
    const buttons = within(head).getAllByRole('button').map((b) => b.getAttribute('aria-label') ?? b.textContent);
    expect(buttons).toEqual(['Previous answer', 'Next answer', 'Clear', 'Regenerate', 'Copy']);
    await user.click(within(nav).getByRole('button', { name: 'Previous answer' }));
    await user.click(within(nav).getByRole('button', { name: 'Next answer' }));
    await user.click(within(nav).getByRole('button', { name: 'Clear' }));
    expect(props.onPrev).toHaveBeenCalledTimes(1);
    expect(props.onNext).toHaveBeenCalledTimes(1);
    expect(props.onClear).toHaveBeenCalledTimes(1);
  });

  it('Clear is enabled only while idle', () => {
    renderPanel({ historyLength: 2, idle: false });
    expect(screen.getByRole('button', { name: 'Clear' })).toBeDisabled();
  });
});

describe('stream follow', () => {
  // `tail` (the pinned default) seeds "following" at mount so the very first
  // stream carries a reader who never scrolled; `top` seeds it false so the
  // same stream leaves them at the opening sentence.
  it('tail carries a fresh reader to the bottom of the first stream', async () => {
    const entry = makeEntry({ answer: 'line' });
    const { body, rerenderPanel } = renderPanel({ viewed: entry, streaming: true, streamFollow: 'tail' });
    await frame();
    mockGeometry(body, 500, 100);
    rerenderPanel({ viewed: { ...entry, answer: 'line plus more' } });
    await frame();
    expect(body.scrollTop).toBe(400);
  });

  it('top parks a fresh reader at the opening sentence while the stream lands', async () => {
    const entry = makeEntry({ answer: 'line' });
    const { body, rerenderPanel } = renderPanel({ viewed: entry, streaming: true, streamFollow: 'top' });
    await frame();
    mockGeometry(body, 500, 100);
    rerenderPanel({ viewed: { ...entry, answer: 'line plus more' } });
    await frame();
    expect(body.scrollTop).toBe(0);
  });

  it('top does not re-engage sticking on an entry switch, even when the new entry fits', async () => {
    const longEntry = makeEntry({ key: 'a', answer: 'a much longer answer' });
    const { body, rerenderPanel } = renderPanel({ viewed: longEntry, streamFollow: 'top' });
    Object.defineProperty(body, 'clientHeight', { configurable: true, value: 100 });
    Object.defineProperty(body, 'scrollHeight', {
      configurable: true,
      get: () => ((body.textContent ?? '').includes('longer') ? 500 : (body.textContent ?? '').includes('streamed') ? 150 : 60),
    });
    const liveEntry = makeEntry({ key: 'b', question: 'next', answer: '' });
    rerenderPanel({ viewed: liveEntry, streaming: true });
    rerenderPanel({ viewed: { ...liveEntry, answer: 'streamed tokens' } });
    await frame();
    // The tail counterpart of this scenario lands at 50 (see autoscroll).
    expect(body.scrollTop).toBe(0);
  });

  it('top still resets to the top when switching history entries', () => {
    const first = makeEntry({ key: 'a', answer: 'first answer' });
    const { body, rerenderPanel } = renderPanel({ viewed: first, streamFollow: 'top' });
    mockGeometry(body, 500, 100);
    body.scrollTop = 400;
    fireEvent.scroll(body);
    rerenderPanel({ viewed: makeEntry({ key: 'b', answer: 'second answer' }) });
    expect(body.scrollTop).toBe(0);
  });
});

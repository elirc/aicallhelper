/**
 * MainView driven with a hand-built SessionApi so each state is pinned
 * directly; assertions are on rendered DOM, not on hook internals.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createRef } from 'react';
import type { AnswerStyle, Envelope, SettingsView as Settings } from '../types';
import type { SessionApi } from '../state/useSession';
import { formatDuration, formatHotkey } from '../format';
import type { HotkeyStatus } from '../components/hotkey';
import { MainView } from './MainView';
import { baseSettings, err, makeEntry, makeSession, ok } from './testUtils';

const HK: HotkeyStatus = { accelerator: 'CommandOrControl+Shift+Space', registered: true };
const HK_TEXT = formatHotkey(HK.accelerator);

interface RenderOpts {
  settings?: Settings | null;
  hotkey?: HotkeyStatus | null;
  onSelectStyle?: (style: AnswerStyle) => Promise<Envelope<Settings>>;
}

function renderMain(sessionOverrides: Partial<SessionApi> = {}, opts: RenderOpts = {}) {
  const session = makeSession(sessionOverrides);
  const gearRef = createRef<HTMLButtonElement>();
  const onOpenSettings = vi.fn();
  const onSelectStyle = vi.fn(opts.onSelectStyle ?? (async () => ok(baseSettings)));
  const settings = opts.settings === undefined ? baseSettings : opts.settings;
  const hotkey = opts.hotkey === undefined ? HK : opts.hotkey;
  const view = (s: SessionApi) => (
    <MainView
      session={s}
      settings={settings}
      hotkey={hotkey}
      onOpenSettings={onOpenSettings}
      onSelectStyle={onSelectStyle}
      gearRef={gearRef}
    />
  );
  const utils = render(view(session));
  return {
    ...utils,
    session,
    onOpenSettings,
    onSelectStyle,
    rerenderSession: (o: Partial<SessionApi>) => {
      const next = makeSession(o);
      utils.rerender(view(next));
      return next;
    },
  };
}

describe('status line', () => {
  it('idle without a registered hotkey', () => {
    renderMain({}, { hotkey: null });
    const line = screen.getByText('Ready — press Record while the other person is speaking');
    expect(line).toHaveAttribute('role', 'status');
  });

  it('idle appends the formatted hotkey when registered', () => {
    renderMain();
    expect(
      screen.getByText(`Ready — press Record while the other person is speaking or ${HK_TEXT}`)
    ).toBeInTheDocument();
  });

  it.each([
    ['starting', 'Opening the microphone feed…'],
    ['recording', 'Recording call audio…'],
    ['finalizing', 'Finalizing transcript…'],
    ['answering', 'Generating answer…'],
  ] as const)('%s', (state, text) => {
    renderMain({ state });
    expect(screen.getByText(text)).toBeInTheDocument();
  });

  it('explains the auto-stop when the recording cap was hit', () => {
    renderMain({ state: 'answering', hitRecordingCap: true });
    expect(screen.getByText('Reached the 120s limit — answering now')).toBeInTheDocument();
  });

  it('shows the first-run nudge when the Deepgram key is missing', () => {
    renderMain({}, { settings: { ...baseSettings, hasDeepgramKey: false } });
    expect(screen.getByText('First run: open Settings (gear icon) and add your API keys')).toBeInTheDocument();
  });

  it('first-run keys off the SELECTED provider', () => {
    renderMain({}, { settings: { ...baseSettings, llmProvider: 'groq', hasGroqKey: false } });
    expect(screen.getByText('First run: open Settings (gear icon) and add your API keys')).toBeInTheDocument();
  });

  it('a missing key for the UNSELECTED provider is not first-run', () => {
    // Anthropic selected and present; only the Groq key is missing.
    renderMain({}, { settings: { ...baseSettings, hasGroqKey: false } });
    expect(screen.getByText(/^Ready — press Record/)).toBeInTheDocument();
  });

  it('shows Done after an answer completes cleanly', () => {
    const { rerenderSession } = renderMain({ state: 'answering' });
    rerenderSession({ state: 'idle' });
    expect(screen.getByText('Done — press Record for the next question')).toBeInTheDocument();
  });

  it('does not claim Done when the session ended in an error', () => {
    const { rerenderSession } = renderMain({ state: 'answering' });
    rerenderSession({ state: 'idle', error: { code: 'llm_timeout', message: 'timed out' } });
    expect(screen.queryByText('Done — press Record for the next question')).not.toBeInTheDocument();
  });
});

describe('record button', () => {
  it.each([
    ['idle', 'Record'],
    ['starting', 'Starting…'],
    ['recording', 'Stop & Answer'],
    ['finalizing', 'Record'],
    ['answering', 'Record'],
  ] as const)('labels %s as %s', (state, label) => {
    renderMain({ state }, { hotkey: null });
    expect(screen.getByRole('button', { name: label })).toBeInTheDocument();
  });

  it('is disabled only while finalizing', () => {
    const { rerenderSession } = renderMain({ state: 'finalizing' }, { hotkey: null });
    expect(screen.getByRole('button', { name: 'Record' })).toBeDisabled();
    rerenderSession({ state: 'starting' });
    expect(screen.getByRole('button', { name: 'Starting…' })).toBeEnabled();
  });

  it('shows the formatted hotkey chip when registered', () => {
    renderMain();
    expect(screen.getByText(HK_TEXT)).toBeInTheDocument();
  });

  it('drops the chip when the hotkey is not registered', () => {
    renderMain({}, { hotkey: { ...HK, registered: false } });
    // Accessible name is exactly "Record" once the chip is gone.
    expect(screen.getByRole('button', { name: 'Record' })).toBeInTheDocument();
  });

  it('click toggles the session', async () => {
    const user = userEvent.setup();
    const { session } = renderMain();
    await user.click(screen.getByRole('button', { name: `Record ${HK_TEXT}` }));
    expect(session.toggleRecord).toHaveBeenCalledTimes(1);
  });
});

describe('hotkey taken notice', () => {
  it('uses the exact wording', () => {
    renderMain({}, { hotkey: { ...HK, registered: false } });
    expect(
      screen.getByText(
        `${HK_TEXT} is already taken by another app, so the shortcut is off — record from this window, or pick a different one in Settings.`
      )
    ).toBeInTheDocument();
  });

  it('is absent while the hotkey is registered', () => {
    renderMain();
    expect(screen.queryByText(/already taken by another app/)).not.toBeInTheDocument();
  });

  // §8: an empty accelerator means the user turned the shortcut OFF. The
  // bridge still reports registered:false, which used to trip the "taken by
  // another app" notice with a blank key name — a false accusation for a
  // deliberate setting.
  it.each([
    ['empty', ''],
    ['whitespace-only', '   '],
  ])('renders no notice, no chip, no status suffix for a disabled (%s) hotkey', (_label, accelerator) => {
    renderMain({}, { hotkey: { accelerator, registered: false } });
    expect(screen.queryByText(/already taken by another app/)).not.toBeInTheDocument();
    // Accessible name is exactly "Record": no hotkey chip inside the button.
    expect(screen.getByRole('button', { name: 'Record' })).toBeInTheDocument();
    // And the idle status line carries no "or <hotkey>" suffix.
    expect(screen.getByText('Ready — press Record while the other person is speaking')).toBeInTheDocument();
  });
});

describe('recording row', () => {
  it('shows the level meter and mm:ss timer while recording', () => {
    renderMain({ state: 'recording', rms: 0.5, elapsedMs: 65_000 });
    expect(screen.getByRole('meter', { name: 'Microphone level' })).toHaveAttribute('aria-valuenow', '50');
    expect(screen.getByText(formatDuration(65_000))).toBeInTheDocument();
  });

  it('is absent when idle', () => {
    renderMain();
    expect(screen.queryByRole('meter')).not.toBeInTheDocument();
  });
});

describe('question heard panel', () => {
  it('shows the placeholder when nothing was ever recorded', () => {
    renderMain();
    expect(screen.getByText('The live transcript will appear here while you record.')).toBeInTheDocument();
  });

  it('shows Listening… and the live tag while recording with nothing heard', () => {
    const entry = makeEntry({ question: '', answer: '' });
    renderMain({ state: 'recording', history: [entry], viewed: entry });
    expect(screen.getByText('Listening…')).toBeInTheDocument();
    expect(screen.getByText('live')).toBeInTheDocument();
  });

  it('shows the live transcript text while recording', () => {
    const entry = makeEntry({ question: 'Tell me about a hard bug', answer: '' });
    renderMain({ state: 'recording', history: [entry], viewed: entry });
    expect(screen.getByText('Tell me about a hard bug')).toBeInTheDocument();
  });

  it('drops the live tag once recording ends', () => {
    const entry = makeEntry();
    renderMain({ state: 'idle', history: [entry], viewed: entry });
    expect(screen.queryByText('live')).not.toBeInTheDocument();
  });

  // The tag belongs to the entry the STT is feeding — the newest one. An
  // older entry viewed mid-recording is finished text; a pulsing "live" tag
  // on it claims the visible words are still moving while the real live
  // transcript updates out of view.
  it('hides the live tag when viewing an OLDER entry mid-recording', () => {
    const older = makeEntry({ key: 'a', question: 'Old question' });
    const newest = makeEntry({ key: 'b', question: 'heard so far', answer: '' });
    renderMain({ state: 'recording', history: [older, newest], viewIndex: 0, viewed: older });
    expect(screen.getByText('Old question')).toBeInTheDocument();
    expect(screen.queryByText('live')).not.toBeInTheDocument();
  });

  it('keeps the live tag when viewing the LIVE entry mid-recording', () => {
    const older = makeEntry({ key: 'a', question: 'Old question' });
    const newest = makeEntry({ key: 'b', question: 'heard so far', answer: '' });
    renderMain({ state: 'recording', history: [older, newest], viewIndex: 1, viewed: newest });
    expect(screen.getByText('live')).toBeInTheDocument();
  });
});

describe('ask form', () => {
  it.each(['starting', 'recording', 'finalizing'] as const)('is disabled while %s', (state) => {
    renderMain({ state });
    expect(screen.getByLabelText('Type a question')).toBeDisabled();
  });

  it.each(['idle', 'answering'] as const)('stays enabled while %s', (state) => {
    renderMain({ state });
    expect(screen.getByLabelText('Type a question')).toBeEnabled();
  });

  it('clears the input only when the ask is accepted', async () => {
    const user = userEvent.setup();
    const { session } = renderMain({ submitAsk: vi.fn(async () => true) });
    const input = screen.getByLabelText('Type a question');
    await user.type(input, 'How would you scale this?');
    await user.click(screen.getByRole('button', { name: 'Ask' }));
    expect(session.submitAsk).toHaveBeenCalledWith('How would you scale this?');
    expect(input).toHaveValue('');
  });

  it('keeps the input when the ask is refused so the user can retry', async () => {
    const user = userEvent.setup();
    renderMain({ submitAsk: vi.fn(async () => false) });
    const input = screen.getByLabelText('Type a question');
    await user.type(input, 'How would you scale this?');
    await user.click(screen.getByRole('button', { name: 'Ask' }));
    expect(input).toHaveValue('How would you scale this?');
  });
});

describe('style chips', () => {
  it('pressed state mirrors the persisted style, not the clicked chip', async () => {
    const user = userEvent.setup();
    // The save resolves but the settings prop never changes (as after a failed
    // or coerced save) — the clicked chip must NOT light up.
    const { onSelectStyle } = renderMain({}, { settings: { ...baseSettings, answerStyle: 'detailed' } });
    expect(screen.getByRole('button', { name: 'Detailed' })).toHaveAttribute('aria-pressed', 'true');
    await user.click(screen.getByRole('button', { name: 'Brief' }));
    expect(onSelectStyle).toHaveBeenCalledWith('brief');
    expect(screen.getByRole('button', { name: 'Brief' })).toHaveAttribute('aria-pressed', 'false');
    expect(screen.getByRole('button', { name: 'Detailed' })).toHaveAttribute('aria-pressed', 'true');
  });

  it('surfaces a failed style save in the error box', async () => {
    const user = userEvent.setup();
    const { session } = renderMain(
      {},
      { onSelectStyle: vi.fn(async () => err<Settings>('internal', 'save failed')) }
    );
    await user.click(screen.getByRole('button', { name: 'Brief' }));
    expect(session.setError).toHaveBeenCalledWith({ code: 'internal', message: 'save failed' });
  });
});

describe('error box', () => {
  it('renders real errors with role alert', () => {
    renderMain({ error: { code: 'llm_rate_limit', message: 'Slow down please' } });
    expect(within(screen.getByRole('alert')).getByText('Slow down please')).toBeInTheDocument();
  });

  it('renders nothing for aborted', () => {
    renderMain({ error: { code: 'aborted', message: 'cancelled' } });
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
});

describe('history bar', () => {
  const two = [makeEntry({ key: 'a', question: 'Q1' }), makeEntry({ key: 'b', question: 'Q2' })];

  it('is hidden with fewer than two entries', () => {
    const one = makeEntry();
    renderMain({ history: [one], viewed: one });
    expect(screen.queryByRole('navigation', { name: 'Answer history' })).not.toBeInTheDocument();
  });

  it('appears at two entries with an n/m label and nav arrows', async () => {
    const user = userEvent.setup();
    const { session } = renderMain({ history: two, viewIndex: 1, viewed: two[1] ?? null });
    expect(screen.getByText('2/2')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Next answer' })).toBeDisabled();
    await user.click(screen.getByRole('button', { name: 'Previous answer' }));
    expect(session.viewPrev).toHaveBeenCalledTimes(1);
  });

  it('disables prev at the oldest entry', () => {
    renderMain({ history: two, viewIndex: 0, viewed: two[0] ?? null });
    expect(screen.getByRole('button', { name: 'Previous answer' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Next answer' })).toBeEnabled();
  });

  it('disables Clear unless idle', () => {
    renderMain({ state: 'answering', history: two, viewIndex: 1, viewed: two[1] ?? null });
    expect(screen.getByRole('button', { name: 'Clear' })).toBeDisabled();
  });

  it('Clear clears, announces, and moves focus to Record', async () => {
    const user = userEvent.setup();
    const { session } = renderMain(
      { history: two, viewIndex: 1, viewed: two[1] ?? null },
      { hotkey: null }
    );
    await user.click(screen.getByRole('button', { name: 'Clear' }));
    expect(session.clearHistory).toHaveBeenCalledTimes(1);
    expect(screen.getByText('History cleared')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Record' })).toHaveFocus();
  });

  // The announcement text must be WIPED between clears: a second identical
  // string is a state update React bails out of — no DOM change reaches the
  // live region and the screen reader hears nothing (§9 pins the
  // announcement, for every Clear).
  it('announces History cleared again on a second Clear', async () => {
    const user = userEvent.setup();
    renderMain({ history: two, viewIndex: 1, viewed: two[1] ?? null }, { hotkey: null });
    await user.click(screen.getByRole('button', { name: 'Clear' }));
    expect(screen.getByText('History cleared')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText('History cleared')).not.toBeInTheDocument(), {
      timeout: 3000,
    });
    await user.click(screen.getByRole('button', { name: 'Clear' }));
    expect(screen.getByText('History cleared')).toBeInTheDocument();
  });
});

describe('header', () => {
  it('opens settings from the gear', async () => {
    const user = userEvent.setup();
    const { onOpenSettings } = renderMain();
    await user.click(screen.getByRole('button', { name: 'Settings' }));
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
  });

  it('keeps the gear disabled until settings load', () => {
    renderMain({}, { settings: null });
    expect(screen.getByRole('button', { name: 'Settings' })).toBeDisabled();
  });
});

describe('regenerate visibility', () => {
  it('is shown when the viewed entry has a question and state is idle', () => {
    const entry = makeEntry();
    renderMain({ history: [entry], viewed: entry });
    expect(screen.getByRole('button', { name: 'Regenerate' })).toBeInTheDocument();
  });

  it('is shown during answering', () => {
    const entry = makeEntry({ answer: 'partial' });
    renderMain({ state: 'answering', history: [entry], viewed: entry });
    expect(screen.getByRole('button', { name: 'Regenerate' })).toBeInTheDocument();
  });

  it('is hidden while recording', () => {
    const entry = makeEntry({ question: 'heard so far', answer: '' });
    renderMain({ state: 'recording', history: [entry], viewed: entry });
    expect(screen.queryByRole('button', { name: 'Regenerate' })).not.toBeInTheDocument();
  });

  it('is hidden when the viewed entry has no question', () => {
    const entry = makeEntry({ question: '', answer: '' });
    renderMain({ history: [entry], viewed: entry });
    expect(screen.queryByRole('button', { name: 'Regenerate' })).not.toBeInTheDocument();
  });
});

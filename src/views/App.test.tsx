// Compile optional views during test setup, before timed UI interactions.
import './SettingsView';
/**
 * Integration: the REAL useSession hook and real views, driven through a
 * FakeBridge installed with setBridge. Core events are emitted exactly as the
 * Rust side would emit them.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import App from '../App';
import { setBridge } from '../bridge';
import { formatLatency, latencyTitle } from '../format';
import type { Metrics } from '../types';
import { FakeBridge, err, ok } from './testUtils';

let bridge: FakeBridge;

beforeEach(() => {
  bridge = new FakeBridge();
  setBridge(bridge);
});

/**
 * Flush the animation frame the answer panel batches on. jsdom's rAF runs on
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

async function renderApp() {
  const utils = render(<App />);
  // Settings load async; the gear enabling marks the app fully hydrated.
  await waitFor(() => expect(screen.getByRole('button', { name: 'Settings' })).toBeEnabled());
  return utils;
}

describe('record → stop → answer', () => {
  it('walks the full pipeline against real state', async () => {
    const user = userEvent.setup();
    await renderApp();

    await user.click(screen.getByRole('button', { name: /^Record/ }));
    await screen.findByText('Recording call audio…');
    expect(bridge.startSession).toHaveBeenCalledTimes(1);

    act(() => bridge.emit('stt:partial', { sessionId: 1, text: 'What is REST?', isFinal: false }));
    expect(screen.getByText('What is REST?')).toBeInTheDocument();
    expect(screen.getByText('live')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /^Stop & Answer/ }));
    await screen.findByText('Finalizing transcript…');
    expect(bridge.stopSession).toHaveBeenCalledWith(1);

    act(() => bridge.emit('llm:delta', { sessionId: 1, delta: 'Use the ' }));
    act(() => bridge.emit('llm:delta', { sessionId: 1, delta: 'HTTP verbs.' }));
    expect(screen.getByText('Generating answer…')).toBeInTheDocument();
    await frame();
    expect(screen.getByText('Use the HTTP verbs.')).toBeInTheDocument();

    const metrics: Metrics = { sttFinalizeMs: 120, firstTokenMs: 900, totalMs: 1600 };
    act(() =>
      bridge.emit('llm:done', {
        sessionId: 1,
        transcript: 'What is REST?',
        answer: 'Use the HTTP verbs.',
        metrics,
      })
    );
    await screen.findByText('Done — press Record for the next question');
    const chip = screen.getByText(`${formatLatency(900)} to first word`);
    expect(chip).toHaveAttribute('title', latencyTitle(metrics));
  });

  it('copies the markdown source and confirms', async () => {
    const user = userEvent.setup();
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    await renderApp();

    await user.click(screen.getByRole('button', { name: /^Record/ }));
    await screen.findByText('Recording call audio…');
    await user.click(screen.getByRole('button', { name: /^Stop & Answer/ }));
    const metrics: Metrics = { sttFinalizeMs: 100, firstTokenMs: 800, totalMs: 1500 };
    act(() =>
      bridge.emit('llm:done', {
        sessionId: 1,
        transcript: 'Q',
        answer: '- first\n- second',
        metrics,
      })
    );
    await screen.findByText('Done — press Record for the next question');

    await user.click(screen.getByRole('button', { name: 'Copy' }));
    // The SOURCE, dashes and all — not the rendered list text.
    expect(writeText).toHaveBeenCalledWith('- first\n- second');
    expect(await screen.findByText('Copied ✓')).toBeInTheDocument();
  });

  it('surfaces a session error, and aborted renders nothing', async () => {
    const user = userEvent.setup();
    await renderApp();

    await user.click(screen.getByRole('button', { name: /^Record/ }));
    await screen.findByText('Recording call audio…');
    act(() =>
      bridge.emit('session:error', { sessionId: 1, error: { code: 'aborted', message: 'cancelled' } })
    );
    await screen.findByText(/^Ready — press Record/);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /^Record/ }));
    await screen.findByText('Recording call audio…');
    act(() =>
      bridge.emit('session:error', {
        sessionId: 2,
        error: { code: 'stt_timeout', message: 'Deepgram went quiet' },
      })
    );
    expect(await screen.findByRole('alert')).toHaveTextContent('Deepgram went quiet');
  });
});

describe('typed ask', () => {
  it('sends the text, clears the input, and streams the answer', async () => {
    const user = userEvent.setup();
    await renderApp();
    const input = screen.getByLabelText('Type a question');
    await user.type(input, 'Tell me about yourself');
    await user.click(screen.getByRole('button', { name: 'Ask' }));
    expect(bridge.ask).toHaveBeenCalledWith('Tell me about yourself');
    await screen.findByText('Generating answer…');
    expect(input).toHaveValue('');
    // Ask stays usable DURING answering (only starting/recording/finalizing lock it).
    expect(input).toBeEnabled();
  });

  it('keeps the input and shows the error when the ask is rejected', async () => {
    const user = userEvent.setup();
    bridge.ask.mockResolvedValueOnce(err('llm_auth', 'The key was rejected'));
    await renderApp();
    const input = screen.getByLabelText('Type a question');
    await user.type(input, 'Tell me about yourself');
    await user.click(screen.getByRole('button', { name: 'Ask' }));
    await screen.findByRole('alert');
    expect(screen.getByRole('alert')).toHaveTextContent('The key was rejected');
    expect(input).toHaveValue('Tell me about yourself');
  });

  it('is disabled while recording', async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.click(screen.getByRole('button', { name: /^Record/ }));
    await screen.findByText('Recording call audio…');
    expect(screen.getByLabelText('Type a question')).toBeDisabled();
  });
});

describe('global hotkey', () => {
  it('toggles recording like the Record button', async () => {
    await renderApp();
    act(() => bridge.emit('hotkey:toggle', null));
    await screen.findByText('Recording call audio…');
    expect(bridge.startSession).toHaveBeenCalledTimes(1);
    act(() => bridge.emit('hotkey:toggle', null));
    await screen.findByText('Finalizing transcript…');
    expect(bridge.stopSession).toHaveBeenCalledTimes(1);
  });

  it('is ignored while Settings is open, and works again after close', async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await screen.findByRole('heading', { name: 'Settings' });

    // The user may be typing the accelerator itself into the hotkey field.
    act(() => bridge.emit('hotkey:toggle', null));
    expect(bridge.startSession).not.toHaveBeenCalled();

    await user.keyboard('{Escape}');
    // Focus returns to the gear that opened Settings.
    await waitFor(() => expect(screen.getByRole('button', { name: 'Settings' })).toHaveFocus());

    act(() => bridge.emit('hotkey:toggle', null));
    await waitFor(() => expect(bridge.startSession).toHaveBeenCalledTimes(1));
  });
});

describe('style chips', () => {
  it('lights the chip the SAVE returned, not the one clicked', async () => {
    const user = userEvent.setup();
    // Core coerces the value: the user clicks Brief but detailed persists.
    bridge.setSettings.mockResolvedValue(ok({ ...bridge.settings, answerStyle: 'detailed' }));
    await renderApp();
    const statusCallsBefore = bridge.hotkeyStatus.mock.calls.length;
    await user.click(screen.getByRole('button', { name: 'Brief' }));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Detailed' })).toHaveAttribute('aria-pressed', 'true')
    );
    expect(screen.getByRole('button', { name: 'Brief' })).toHaveAttribute('aria-pressed', 'false');
    expect(bridge.hotkeyStatus.mock.calls.length).toBe(statusCallsBefore);
  });
});

describe('settings round trip through App', () => {
  it('sends only the touched key field and re-checks the hotkey', async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.type(await screen.findByLabelText('Deepgram API key'), 'dg_new');
    const statusCallsBefore = bridge.hotkeyStatus.mock.calls.length;
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText('Saved ✓');

    const patch = bridge.setSettings.mock.calls[0]?.[0];
    expect(patch).toBeDefined();
    expect(patch?.deepgramKey).toBe('dg_new');
    expect(patch !== undefined && 'anthropicKey' in patch).toBe(false);
    expect(patch !== undefined && 'groqKey' in patch).toBe(false);
    // The rest of the form always travels whole.
    expect(patch?.resume).toBe(bridge.settings.resume);
    expect(patch?.hotkey).toBe(bridge.settings.hotkey);
    // Saving may re-register the shortcut; the status must be re-fetched.
    expect(bridge.hotkeyStatus.mock.calls.length).toBeGreaterThan(statusCallsBefore);
  });
});

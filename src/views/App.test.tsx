// Compile optional views during test setup, before timed UI interactions.
import './SettingsView';
/**
 * Integration: the REAL useSession hook and real views, driven through a
 * FakeBridge installed with setBridge. Core events are emitted exactly as the
 * Rust side would emit them.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import App from '../App';
import { setBridge } from '../bridge';
import { formatLatency, latencyTitle } from '../format';
import type { Envelope, Metrics, SettingsView } from '../types';
import { FakeBridge, err, makeBudget, makeProfile, ok } from './testUtils';

/**
 * Counts loads of the PracticeLibrary chunk (the factory runs once, on the
 * first import). SettingsView is imported at the top of this file so its
 * tests do not wait on a cold compile, so only this chunk can prove the
 * idle-time preload — and only in the FIRST test, before any other render
 * has been mounted long enough for the preload timer to fire.
 */
const practiceLoads = vi.hoisted(() => ({ count: 0 }));
vi.mock('../components/PracticeLibrary', async (importOriginal) => {
  practiceLoads.count += 1;
  return importOriginal<typeof import('../components/PracticeLibrary')>();
});

let bridge: FakeBridge;

beforeEach(() => {
  bridge = new FakeBridge();
  setBridge(bridge);
});

async function renderApp() {
  const utils = render(<App />);
  // Settings load async; the gear enabling marks the app fully hydrated. The
  // default 1 s can expire on a contended worker (the app now warms its lazy
  // chunks in the background, and the full suite runs sixteen jsdoms at once).
  await waitFor(() => expect(screen.getByRole('button', { name: 'Settings' })).toBeEnabled(), {
    timeout: 10_000,
  });
  return utils;
}

describe('chunk preloading (FE-3)', () => {
  // Real timers on purpose: the chunk compiles in real time, and faking the
  // clock around a React tree that other tests in this file go on to render
  // is how a whole file turns flaky.
  it('warms the lazy chunks 1.5 s after mount, and an unmount cancels the warm-up', async () => {
    const first = render(<App />);
    // Synchronously after mount: the deadline is a timer, so it cannot have fired.
    expect(practiceLoads.count).toBe(0);
    first.unmount();
    await new Promise((resolve) => setTimeout(resolve, 1800));
    // The timer went with the tree.
    expect(practiceLoads.count).toBe(0);

    render(<App />);
    await waitFor(() => expect(practiceLoads.count).toBe(1), { timeout: 10_000 });
  });
});

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
    // The first delta paints synchronously; the second is coalesced behind a
    // 16 ms timer and then a paint frame (FE-1), so poll rather than flush.
    expect(screen.getByText('Generating answer…')).toBeInTheDocument();
    expect(await screen.findByText('Use the HTTP verbs.')).toBeInTheDocument();

    const metrics: Metrics = { sttFinalizeMs: 120, firstTokenMs: 900, totalMs: 1600 };
    act(() =>
      bridge.emit('llm:done', {
        sessionId: 1,
        transcript: 'What is REST?',
        answer: 'Use the HTTP verbs.',
        metrics,
        stopReason: 'complete',
      })
    );
    await screen.findByText('Done — press Record for the next question');
    const chip = screen.getByText(`${formatLatency(900)} to first word`);
    expect(chip).toHaveAttribute('title', latencyTitle(metrics));
    // The transcript strip gave its height back: collapsed to a caption.
    expect(screen.getByRole('button', { name: /^Question heard/ })).toHaveAttribute('aria-expanded', 'false');
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
        stopReason: 'complete',
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

describe('header actions', () => {
  it('dock button calls the bridge and surfaces a refusal', async () => {
    const user = userEvent.setup();
    await renderApp();
    bridge.dockToCamera.mockResolvedValueOnce(err('internal', 'Could not find the display this window is on.'));
    await user.click(screen.getByRole('button', { name: 'Dock to camera' }));
    expect(bridge.dockToCamera).toHaveBeenCalledTimes(1);
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not find the display this window is on.');
  });

  it('a successful dock shows no error', async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.click(screen.getByRole('button', { name: 'Dock to camera' }));
    await waitFor(() => expect(bridge.dockToCamera).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('focus mode toggles from the header and back', async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.click(screen.getByRole('button', { name: 'Focus mode' }));
    expect(screen.getByRole('button', { name: 'Focus mode' })).toHaveAttribute('aria-pressed', 'true');
    expect(screen.queryByRole('textbox', { name: 'Type a question' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^Record/ })).toBeVisible();
    await user.click(screen.getByRole('button', { name: 'Focus mode' }));
    expect(screen.getByRole('textbox', { name: 'Type a question' })).toBeVisible();
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

describe('a refused hotkey (ADR 016)', () => {
  it('is re-checked after any save while it is refused, since every save retries the registration', async () => {
    const user = userEvent.setup();
    bridge.hotkey = { accelerator: 'Ctrl+Shift+Space', registered: false };
    await renderApp();
    expect(await screen.findByText(/is already taken by another app/)).toBeInTheDocument();
    // The other app let go; this chip save's effect step registered it.
    bridge.hotkey = { accelerator: 'Ctrl+Shift+Space', registered: true };
    await user.click(screen.getByRole('button', { name: 'Brief' }));
    await waitFor(() => expect(screen.queryByText(/is already taken by another app/)).not.toBeInTheDocument());
  });
});

describe('profile chips', () => {
  const P_A = makeProfile({ id: 'a', name: 'Backend' });
  const P_B = makeProfile({ id: 'b', name: 'Rust / systems' });

  it('a profile switch sends ONLY activeProfileId and lights the chip the save returned', async () => {
    const user = userEvent.setup();
    bridge = new FakeBridge({ profiles: [P_A, P_B], activeProfileId: 'a' });
    setBridge(bridge);
    await renderApp();
    await user.click(screen.getByRole('button', { name: 'Rust / systems' }));
    // §8: a switch never rewrites profile text — the id travels alone.
    expect(bridge.setSettings).toHaveBeenLastCalledWith({ activeProfileId: 'b' });
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Rust / systems' })).toHaveAttribute('aria-pressed', 'true')
    );
    expect(screen.getByRole('button', { name: 'Backend' })).toHaveAttribute('aria-pressed', 'false');
  });

  it('a refused switch leaves the old chip lit and shows the error', async () => {
    const user = userEvent.setup();
    bridge = new FakeBridge({ profiles: [P_A, P_B], activeProfileId: 'a' });
    setBridge(bridge);
    bridge.setSettings.mockResolvedValueOnce(err('internal', 'Could not write settings'));
    await renderApp();
    await user.click(screen.getByRole('button', { name: 'Rust / systems' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not write settings');
    expect(screen.getByRole('button', { name: 'Backend' })).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('button', { name: 'Rust / systems' })).toHaveAttribute('aria-pressed', 'false');
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
    expect(patch?.profiles).toEqual(bridge.settings.profiles);
    expect(patch?.activeProfileId).toBe(bridge.settings.activeProfileId);
    expect(patch?.hotkey).toBe(bridge.settings.hotkey);
    // Saving may re-register the shortcut; the status must be re-fetched.
    expect(bridge.hotkeyStatus.mock.calls.length).toBeGreaterThan(statusCallsBefore);
  });
});

describe('settings revisions through App (R5)', () => {
  const P_A = makeProfile({ id: 'a', name: 'Backend' });
  const P_B = makeProfile({ id: 'b', name: 'Rust / systems' });

  it('responses delivered in reverse commit order never reinstall the older view', async () => {
    const user = userEvent.setup();
    bridge = new FakeBridge({ profiles: [P_A, P_B], activeProfileId: 'a' });
    setBridge(bridge);
    const held: Array<(v: Envelope<SettingsView>) => void> = [];
    bridge.setSettings.mockImplementation(
      () => new Promise<Envelope<SettingsView>>((resolve) => held.push(resolve))
    );
    await renderApp();
    // Style commits first (revision 2), the profile switch second (3).
    await user.click(screen.getByRole('button', { name: 'Brief' }));
    await user.click(screen.getByRole('button', { name: 'Rust / systems' }));
    expect(held).toHaveLength(2);
    const committed2 = { ...bridge.settings, revision: 2, answerStyle: 'brief' as const };
    const committed3 = { ...committed2, revision: 3, activeProfileId: 'b' };
    await act(async () => held[1]?.(ok(committed3)));
    await act(async () => held[0]?.(ok(committed2)));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Rust / systems' })).toHaveAttribute('aria-pressed', 'true')
    );
    expect(screen.getByRole('button', { name: 'Backend' })).toHaveAttribute('aria-pressed', 'false');
    expect(screen.getByRole('button', { name: 'Brief' })).toHaveAttribute('aria-pressed', 'true');
  });

  it('a form made stale by a save it never saw gets the conflict, reloads, and saves both changes', async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.click(screen.getByRole('button', { name: 'Settings' }));
    const focus = await screen.findByLabelText('Focus (optional)');
    // Another commit lands in the core that this App never received.
    bridge.settings = { ...bridge.settings, revision: 2, answerStyle: 'detailed' };
    await user.type(focus, 'tokio');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    const banner = await screen.findByRole('alert');
    expect(banner).toHaveTextContent('Settings changed elsewhere — reload');
    await user.click(within(banner).getByRole('button', { name: 'Reload' }));
    await screen.findByText(/Reloaded\. Your unsaved edits were kept/);
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText('Saved ✓');
    const patch = bridge.setSettings.mock.calls.at(-1)?.[0];
    expect(patch?.expectedRevision).toBe(2);
    expect(patch?.answerStyle).toBe('detailed');
    expect(patch?.profiles?.[0]?.focus).toBe('tokio');
    expect(bridge.settings.revision).toBe(3);
  });

  it('a startup storage warning is shown on the main view', async () => {
    bridge = new FakeBridge({ storageWarning: 'Your settings file could not be read (os error 32).' });
    setBridge(bridge);
    await renderApp();
    expect(await screen.findByRole('alert')).toHaveTextContent('could not be read (os error 32)');
  });
});

describe('local typed Ask budget (R4)', () => {
  it('refuses a question over the limit before sending anything, and keeps the text', async () => {
    const user = userEvent.setup();
    bridge = new FakeBridge({ llmProvider: 'local' });
    setBridge(bridge);
    bridge.localPromptBudget.mockResolvedValue(ok(makeBudget({ remainingBytes: -12, status: 'over' })));
    await renderApp();
    const input = screen.getByLabelText('Type a question');
    await user.type(input, 'A very long question');
    await user.click(screen.getByRole('button', { name: 'Ask' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('This question is 12 bytes too long for free local mode');
    expect(bridge.ask).not.toHaveBeenCalled();
    expect(input).toHaveValue('A very long question');
    const [profile, style, question] = bridge.localPromptBudget.mock.calls.at(-1) ?? [];
    expect(profile?.id).toBe(bridge.settings.activeProfileId);
    expect(style).toBe('balanced');
    expect(question).toBe('A very long question');

    // A question that fits goes through.
    bridge.localPromptBudget.mockResolvedValue(ok(makeBudget({ remainingBytes: 3, status: 'tight' })));
    await user.click(screen.getByRole('button', { name: 'Ask' }));
    await waitFor(() => expect(bridge.ask).toHaveBeenCalledWith('A very long question'));
  });

  it('cloud providers never ask for a budget', async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.type(screen.getByLabelText('Type a question'), 'Hi');
    await user.click(screen.getByRole('button', { name: 'Ask' }));
    await waitFor(() => expect(bridge.ask).toHaveBeenCalledWith('Hi'));
    expect(bridge.localPromptBudget).not.toHaveBeenCalled();
  });
});

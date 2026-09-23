import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { DEFAULT_HOTKEY, MAX_PROFILES } from '../types';
import type { Envelope, LocalPromptBudget, SettingsPatch, SettingsView as Settings } from '../types';
import { setBridge } from '../bridge';
import { SettingsView } from './SettingsView';
import { FakeBridge, baseSettings, err, makeBudget, makeProfile, ok } from './testUtils';

let bridge: FakeBridge;

// The local-voice panel and the budget preview go through the bridge; never
// let a test reach the real one.
beforeEach(() => {
  bridge = new FakeBridge();
  setBridge(bridge);
});

function renderSettings(overrides: Partial<Settings> = {}, saveResult?: Envelope<Settings>) {
  const settings = { ...baseSettings, ...overrides };
  const onSave = vi.fn(async (_patch: SettingsPatch) => saveResult ?? ok(settings));
  const onReload = vi.fn(async () => ok(settings));
  const onBack = vi.fn();
  const utils = render(<SettingsView settings={settings} onSave={onSave} onReload={onReload} onBack={onBack} />);
  return { ...utils, onSave, onReload, onBack, settings };
}

/** A promise the test settles by hand: no timing assumptions (R3). */
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function lastPatch(onSave: ReturnType<typeof vi.fn>): SettingsPatch {
  const call = onSave.mock.calls[onSave.mock.calls.length - 1];
  expect(call).toBeDefined();
  return call?.[0] as SettingsPatch;
}

const P_A = makeProfile({ id: 'a', name: 'Backend', focus: 'Go, ' });
const P_B = makeProfile({ id: 'b', name: 'Rust / systems', focus: 'Rust, ' });

describe('focus and dismissal', () => {
  it('moves focus into the heading on open', () => {
    renderSettings();
    expect(screen.getByRole('heading', { name: 'Settings' })).toHaveFocus();
  });

  it('Escape closes', async () => {
    const user = userEvent.setup();
    const { onBack } = renderSettings();
    await user.keyboard('{Escape}');
    expect(onBack).toHaveBeenCalledTimes(1);
  });

  it('Back closes', async () => {
    const user = userEvent.setup();
    const { onBack } = renderSettings();
    await user.click(screen.getByRole('button', { name: 'Back' }));
    expect(onBack).toHaveBeenCalledTimes(1);
  });
});

describe('dirty guard', () => {
  it('a dirty form blocks Escape and Discard then closes', async () => {
    const user = userEvent.setup();
    const { onBack } = renderSettings();
    await user.type(screen.getByLabelText('Focus (optional)'), 'tokio');
    await user.keyboard('{Escape}');
    expect(onBack).not.toHaveBeenCalled();
    const dialog = screen.getByRole('alertdialog', { name: 'Unsaved changes' });
    expect(within(dialog).getByText('Discard unsaved changes?')).toBeInTheDocument();
    // Focus lands on a button, so the question must be the dialog's
    // description or it is never announced.
    expect(dialog).toHaveAccessibleDescription('Discard unsaved changes?');
    // The least destructive action holds focus.
    expect(within(dialog).getByRole('button', { name: 'Keep editing' })).toHaveFocus();
    await user.click(within(dialog).getByRole('button', { name: 'Discard' }));
    expect(onBack).toHaveBeenCalledTimes(1);
  });

  it('Back on a dirty form asks too, and Keep editing keeps the draft', async () => {
    const user = userEvent.setup();
    const { onBack } = renderSettings();
    const focus = screen.getByLabelText('Focus (optional)');
    await user.type(focus, 'tokio');
    await user.click(screen.getByRole('button', { name: 'Back' }));
    expect(onBack).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Keep editing' }));
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    expect(focus).toHaveValue('tokio');
    expect(onBack).not.toHaveBeenCalled();
  });

  // A second Escape must never be the destructive answer.
  it('Escape on the guard means keep editing', async () => {
    const user = userEvent.setup();
    const { onBack } = renderSettings();
    await user.type(screen.getByLabelText('Deepgram API key'), 'dg');
    await user.keyboard('{Escape}');
    expect(screen.getByRole('alertdialog')).toBeInTheDocument();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    expect(onBack).not.toHaveBeenCalled();
  });

  it('a typed-then-reverted edit is clean again', async () => {
    const user = userEvent.setup();
    const { onBack } = renderSettings();
    const name = screen.getByLabelText('Profile name');
    await user.type(name, 'X');
    await user.keyboard('{Backspace}');
    await user.keyboard('{Escape}');
    expect(onBack).toHaveBeenCalledTimes(1);
  });

  it('a successful save makes the form clean', async () => {
    const user = userEvent.setup();
    const { onBack } = renderSettings();
    await user.type(screen.getByLabelText('Focus (optional)'), 'tokio');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText('Saved ✓');
    await user.keyboard('{Escape}');
    expect(onBack).toHaveBeenCalledTimes(1);
  });
});

describe('key fields', () => {
  it('are always empty, with a replace placeholder only where a key exists', () => {
    renderSettings({ hasDeepgramKey: true, hasAnthropicKey: true, hasGroqKey: false });
    const deepgram = screen.getByLabelText('Deepgram API key');
    expect(deepgram).toHaveValue('');
    expect(deepgram).toHaveAttribute('placeholder', 'saved — type to replace');
    expect(screen.getByLabelText('Anthropic API key')).toHaveAttribute(
      'placeholder',
      'saved — type to replace'
    );
    // No Groq key stored — promising "saved" would be a lie.
    expect(screen.getByLabelText(/Groq API key/)).not.toHaveAttribute(
      'placeholder',
      'saved — type to replace'
    );
  });

  it('are password inputs so keys never show on screen shares', () => {
    renderSettings();
    expect(screen.getByLabelText('Deepgram API key')).toHaveAttribute('type', 'password');
    expect(screen.getByLabelText('Anthropic API key')).toHaveAttribute('type', 'password');
    expect(screen.getByLabelText(/Groq API key/)).toHaveAttribute('type', 'password');
  });

  const wrapper = (label: string | RegExp) => screen.getByLabelText(label).closest('.field');

  it('only the fields the provider needs are shown; local hides all three', async () => {
    const user = userEvent.setup();
    renderSettings();
    // Anthropic: Deepgram + Anthropic, not Groq.
    expect(wrapper('Deepgram API key')).not.toHaveAttribute('hidden');
    expect(wrapper('Anthropic API key')).not.toHaveAttribute('hidden');
    expect(wrapper(/Groq API key/)).toHaveAttribute('hidden');

    await user.selectOptions(screen.getByLabelText('Answer model'), 'local');
    expect(wrapper('Deepgram API key')).toHaveAttribute('hidden');
    expect(wrapper('Anthropic API key')).toHaveAttribute('hidden');
    expect(wrapper(/Groq API key/)).toHaveAttribute('hidden');
    expect(screen.getByRole('region', { name: 'Free local voice setup' })).toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText('Answer model'), 'groq');
    expect(wrapper('Deepgram API key')).not.toHaveAttribute('hidden');
    expect(wrapper('Anthropic API key')).toHaveAttribute('hidden');
    expect(wrapper(/Groq API key/)).not.toHaveAttribute('hidden');
    expect(screen.queryByRole('region', { name: 'Free local voice setup' })).not.toBeInTheDocument();
  });

  it('lists the providers in PROVIDER_ORDER with their catalogue labels', () => {
    renderSettings();
    const options = within(screen.getByLabelText('Answer model')).getAllByRole('option');
    expect(options.map((o) => o.textContent)).toEqual([
      'Claude Haiku 4.5 (recommended)',
      'Groq GPT-OSS 120B (fastest)',
      'Free local voice (Qwen3.5 2B + Moonshine)',
    ]);
  });
});

describe('saving', () => {
  it('omits untouched key fields but always sends the rest of the form', async () => {
    const user = userEvent.setup();
    const { onSave } = renderSettings();
    await user.click(screen.getByRole('button', { name: 'Save' }));
    const patch = lastPatch(onSave);
    expect('deepgramKey' in patch).toBe(false);
    expect('anthropicKey' in patch).toBe(false);
    expect('groqKey' in patch).toBe(false);
    expect(patch.profiles).toEqual(baseSettings.profiles);
    expect(patch.activeProfileId).toBe(baseSettings.activeProfileId);
    expect(patch.alwaysOnTop).toBe(baseSettings.alwaysOnTop);
    expect(patch.llmProvider).toBe(baseSettings.llmProvider);
    expect(patch.answerStyle).toBe(baseSettings.answerStyle);
    expect(patch.hotkey).toBe(baseSettings.hotkey);
    expect(patch.launchPlacement).toBe(baseSettings.launchPlacement);
    expect(patch.streamFollow).toBe(baseSettings.streamFollow);
  });

  it('sends a typed key, and only that key', async () => {
    const user = userEvent.setup();
    const { onSave } = renderSettings();
    await user.type(screen.getByLabelText('Anthropic API key'), 'sk-ant-123');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    const patch = lastPatch(onSave);
    expect(patch.anthropicKey).toBe('sk-ant-123');
    expect('deepgramKey' in patch).toBe(false);
    expect('groqKey' in patch).toBe(false);
  });

  it('a typed-then-cleared key sends "" to wipe the stored key', async () => {
    const user = userEvent.setup();
    const { onSave } = renderSettings();
    const input = screen.getByLabelText('Deepgram API key');
    await user.type(input, 'oops');
    await user.clear(input);
    await user.click(screen.getByRole('button', { name: 'Save' }));
    expect(lastPatch(onSave).deepgramKey).toBe('');
  });

  it('a successful save resets key fields to untouched', async () => {
    const user = userEvent.setup();
    const { onSave } = renderSettings();
    await user.type(screen.getByLabelText('Deepgram API key'), 'dg_1');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText('Saved ✓');
    // A second save must not silently re-send (and re-store) the same key.
    await user.click(screen.getByRole('button', { name: 'Save' }));
    expect('deepgramKey' in lastPatch(onSave)).toBe(false);
  });

  it('sends changed provider, style, hotkey, and always-on-top values', async () => {
    const user = userEvent.setup();
    const { onSave } = renderSettings();
    await user.selectOptions(screen.getByLabelText('Answer model'), 'groq');
    await user.selectOptions(screen.getByLabelText('Answer style'), 'detailed');
    const hotkeyInput = screen.getByLabelText('Global shortcut');
    await user.clear(hotkeyInput);
    await user.type(hotkeyInput, 'Ctrl+Alt+K');
    await user.click(screen.getByLabelText('Keep this window always on top'));
    await user.click(screen.getByRole('button', { name: 'Save' }));
    const patch = lastPatch(onSave);
    expect(patch.llmProvider).toBe('groq');
    expect(patch.answerStyle).toBe('detailed');
    expect(patch.hotkey).toBe('Ctrl+Alt+K');
    expect(patch.alwaysOnTop).toBe(true);
  });

  it('sends a changed launch placement and stream follow', async () => {
    const user = userEvent.setup();
    const { onSave } = renderSettings({ launchPlacement: 'remembered', streamFollow: 'tail' });
    await user.selectOptions(screen.getByLabelText('Window position at launch'), 'camera');
    await user.selectOptions(screen.getByLabelText('While an answer streams'), 'top');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    const patch = lastPatch(onSave);
    expect(patch.launchPlacement).toBe('camera');
    expect(patch.streamFollow).toBe('top');
  });

  it('the hotkey placeholder is the core default accelerator', () => {
    renderSettings({ hotkey: '' });
    expect(screen.getByLabelText('Global shortcut')).toHaveAttribute('placeholder', DEFAULT_HOTKEY);
  });

  it('shows Saved ✓ briefly after a successful save', async () => {
    const user = userEvent.setup();
    renderSettings();
    await user.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByText('Saved ✓')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText('Saved ✓')).not.toBeInTheDocument(), {
      timeout: 3000,
    });
  });

  it('reports a failed save in a settings-local error box', async () => {
    const user = userEvent.setup();
    renderSettings({}, err<Settings>('internal', 'Vault sealed shut'));
    await user.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Vault sealed shut');
    expect(screen.queryByText('Saved ✓')).not.toBeInTheDocument();
  });

  // The core may repair ids and trim names; the form must show what was
  // stored, not what it proposed.
  it('re-seeds every field from the returned view', async () => {
    const user = userEvent.setup();
    const repaired: Settings = {
      ...baseSettings,
      profiles: [makeProfile({ id: 'p1', name: 'Untitled' })],
      activeProfileId: 'p1',
      hotkey: 'Ctrl+Alt+K',
    };
    renderSettings({}, ok(repaired));
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText('Saved ✓');
    expect(screen.getByLabelText('Profile name')).toHaveValue('Untitled');
    expect(screen.getByLabelText('Profile')).toHaveValue('p1');
    expect(screen.getByLabelText('Global shortcut')).toHaveValue('Ctrl+Alt+K');
  });
});

describe('profiles', () => {
  it('edits only the selected profile and sends the whole array plus its id as active', async () => {
    const user = userEvent.setup();
    const { onSave } = renderSettings({ profiles: [P_A, P_B], activeProfileId: 'a' });
    await user.selectOptions(screen.getByLabelText('Profile'), 'b');
    await user.type(screen.getByLabelText('Focus (optional)'), 'tokio');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    const patch = lastPatch(onSave);
    expect(patch.activeProfileId).toBe('b');
    expect(patch.profiles?.[0]).toEqual(P_A);
    expect(patch.profiles?.[1]?.focus).toBe(`${P_B.focus}tokio`);
    expect(patch.profiles).toHaveLength(2);
  });

  it('New adds a blank interview profile, selects it, and is disabled at the cap', async () => {
    const user = userEvent.setup();
    const { onSave } = renderSettings({ profiles: [P_A, P_B], activeProfileId: 'a' });
    await user.click(screen.getByRole('button', { name: 'New' }));
    expect(screen.getByLabelText('Profile name')).toHaveValue('New profile');
    expect(screen.getByLabelText('Call type')).toHaveValue('interview');
    expect(screen.getByLabelText('Resume')).toHaveValue('');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    const patch = lastPatch(onSave);
    expect(patch.profiles).toHaveLength(3);
    const added = patch.profiles?.[2];
    expect(added?.name).toBe('New profile');
    expect(added?.id).toMatch(/^[A-Za-z0-9_-]{1,40}$/);
    expect(patch.activeProfileId).toBe(added?.id);
  });

  it('New and Duplicate are disabled at MAX_PROFILES; Delete with one profile', () => {
    const eight = Array.from({ length: MAX_PROFILES }, (_, i) => makeProfile({ id: `p${i}`, name: `P${i}` }));
    const { unmount } = renderSettings({ profiles: eight, activeProfileId: 'p0' });
    expect(screen.getByRole('button', { name: 'New' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Duplicate' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Delete' })).toBeEnabled();
    unmount();
    renderSettings();
    expect(screen.getByRole('button', { name: 'New' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Delete' })).toBeDisabled();
  });

  it('Duplicate copies every field under a new id with a " copy" name', async () => {
    const user = userEvent.setup();
    const source = makeProfile({
      id: 'a',
      name: 'Backend',
      callType: 'sales',
      resume: 'R',
      jobDescription: 'J',
      focus: 'F',
      extraInstructions: 'E',
    });
    const { onSave } = renderSettings({ profiles: [source], activeProfileId: 'a' });
    await user.click(screen.getByRole('button', { name: 'Duplicate' }));
    expect(screen.getByLabelText('Profile name')).toHaveValue('Backend copy');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    const patch = lastPatch(onSave);
    const copy = patch.profiles?.[1];
    expect(copy).toMatchObject({
      name: 'Backend copy',
      callType: 'sales',
      resume: 'R',
      jobDescription: 'J',
      focus: 'F',
      extraInstructions: 'E',
    });
    expect(copy?.id).not.toBe('a');
    expect(patch.profiles?.[0]).toEqual(source);
    expect(patch.activeProfileId).toBe(copy?.id);
  });

  it('Delete removes the selected profile and selects the previous neighbour', async () => {
    const user = userEvent.setup();
    const P_C = makeProfile({ id: 'c', name: 'Support' });
    const { onSave } = renderSettings({ profiles: [P_A, P_B, P_C], activeProfileId: 'c' });
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    expect(screen.getByLabelText('Profile')).toHaveValue('b');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    const patch = lastPatch(onSave);
    expect(patch.profiles?.map((p) => p.id)).toEqual(['a', 'b']);
    expect(patch.activeProfileId).toBe('b');
  });

  it('labels switch with the call type', async () => {
    const user = userEvent.setup();
    renderSettings();
    expect(screen.getByLabelText('Resume')).toBeInTheDocument();
    expect(screen.getByLabelText('Job description')).toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText('Call type'), 'sales');
    expect(screen.getByLabelText('About you (optional)')).toHaveValue(baseSettings.profiles[0]?.resume);
    expect(screen.getByLabelText('Call context (account, product, agenda)')).toHaveValue(
      baseSettings.profiles[0]?.jobDescription
    );
    expect(screen.queryByLabelText('Resume')).not.toBeInTheDocument();
  });

  it('shows a character counter per text field', () => {
    renderSettings();
    expect(screen.getByLabelText('Resume')).toHaveAccessibleDescription(
      `${baseSettings.profiles[0]?.resume.length} / 200,000 characters`
    );
    expect(screen.getByLabelText('Job description')).toHaveAccessibleDescription(
      `${baseSettings.profiles[0]?.jobDescription.length} / 200,000 characters`
    );
    expect(screen.getByLabelText('Focus (optional)')).toHaveAccessibleDescription('0 / 2,000 characters');
    expect(screen.getByLabelText('Extra instructions (optional)')).toHaveAccessibleDescription('0 / 2,000 characters');
  });

  it('the local budget line is gone for cloud providers and never asks the core', async () => {
    renderSettings();
    await new Promise((r) => setTimeout(r, 400));
    expect(bridge.localPromptBudget).not.toHaveBeenCalled();
    expect(screen.queryByText(/free local mode/)).not.toBeInTheDocument();
  });
});

describe('save lock (R3)', () => {
  it('locks every editing and navigation control while a save is in flight, then unlocks', async () => {
    const user = userEvent.setup();
    const pending = deferred<Envelope<Settings>>();
    const settings = { ...baseSettings, profiles: [P_A, P_B], activeProfileId: 'a' };
    const onSave = vi.fn(() => pending.promise);
    render(<SettingsView settings={settings} onSave={onSave} onReload={vi.fn()} onBack={vi.fn()} />);
    await user.type(screen.getByLabelText('Focus (optional)'), 'X');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    expect(screen.getByRole('button', { name: 'Saving…' })).toBeDisabled();
    expect(screen.getByRole('status')).toHaveTextContent('Saving…');
    for (const label of ['Resume', 'Job description', 'Focus (optional)', 'Profile name', 'Call type', 'Profile',
      'Answer model', 'Answer style', 'Deepgram API key', 'Global shortcut']) {
      expect(screen.getByLabelText(label)).toBeDisabled();
    }
    for (const name of ['New', 'Duplicate', 'Delete', 'Back']) {
      expect(screen.getByRole('button', { name })).toBeDisabled();
    }
    // Typing into the locked form does nothing, so nothing can be lost.
    await user.type(screen.getByLabelText('Resume'), 'lost?');
    expect(screen.getByLabelText('Resume')).toHaveValue(P_A.resume);

    await act(async () => pending.resolve(ok({ ...settings, revision: 2 })));
    expect(await screen.findByText('Saved ✓')).toBeInTheDocument();
    expect(screen.getByLabelText('Resume')).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Save' })).toBeEnabled();
    expect(onSave).toHaveBeenCalledTimes(1);
  });

  it('refuses Escape and Back while saving, even on a dirty form', async () => {
    const user = userEvent.setup();
    const pending = deferred<Envelope<Settings>>();
    const onBack = vi.fn();
    render(<SettingsView settings={baseSettings} onSave={() => pending.promise} onReload={vi.fn()} onBack={onBack} />);
    await user.type(screen.getByLabelText('Focus (optional)'), 'tokio');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await user.keyboard('{Escape}');
    await user.click(screen.getByRole('button', { name: 'Back' }));
    expect(onBack).not.toHaveBeenCalled();
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    await act(async () => pending.resolve(ok({ ...baseSettings, revision: 2 })));
    await screen.findByText('Saved ✓');
    await user.keyboard('{Escape}');
    expect(onBack).toHaveBeenCalledTimes(1);
  });

  it('gives focus back to Save when the lock lifts, if the lock dropped it', async () => {
    const user = userEvent.setup();
    const pending = deferred<Envelope<Settings>>();
    render(<SettingsView settings={baseSettings} onSave={() => pending.promise} onReload={vi.fn()} onBack={vi.fn()} />);
    await user.type(screen.getByLabelText('Focus (optional)'), 'tokio');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    // The HTML focus-fixup rule the webview applies when the focused button
    // becomes disabled (jsdom does not): focus falls back to <body>.
    act(() => {
      const probe = document.createElement('input');
      document.body.append(probe);
      probe.focus();
      probe.remove();
    });
    expect(document.activeElement).toBe(document.body);
    await act(async () => pending.resolve(ok({ ...baseSettings, revision: 2 })));
    await screen.findByText('Saved ✓');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Save' }));
  });

  it('a failed save restores interactivity and keeps the draft and the typed key', async () => {
    const user = userEvent.setup();
    const pending = deferred<Envelope<Settings>>();
    render(<SettingsView settings={baseSettings} onSave={() => pending.promise} onReload={vi.fn()} onBack={vi.fn()} />);
    await user.type(screen.getByLabelText('Focus (optional)'), 'tokio');
    await user.type(screen.getByLabelText('Deepgram API key'), 'dg_typed');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await act(async () => pending.resolve(err('internal', 'Could not save settings: disk full')));
    expect(await screen.findByRole('alert')).toHaveTextContent('disk full');
    expect(screen.getByLabelText('Focus (optional)')).toBeEnabled();
    expect(screen.getByLabelText('Focus (optional)')).toHaveValue('tokio');
    expect(screen.getByLabelText('Deepgram API key')).toHaveValue('dg_typed');
    expect(screen.getByRole('button', { name: 'Save' })).toBeEnabled();
  });
});

describe('revisions (R5)', () => {
  it('a save sends the revision the form was seeded from, then the one its response committed', async () => {
    const user = userEvent.setup();
    const onSave = vi.fn(async (_p: SettingsPatch) => ok({ ...baseSettings, revision: 8 }));
    render(<SettingsView settings={{ ...baseSettings, revision: 7 }} onSave={onSave} onReload={vi.fn()} onBack={vi.fn()} />);
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText('Saved ✓');
    expect(lastPatch(onSave).expectedRevision).toBe(7);
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(onSave).toHaveBeenCalledTimes(2));
    expect(lastPatch(onSave).expectedRevision).toBe(8);
  });

  it('a conflict keeps the draft and blocks Save; Reload rebases the edits onto the newer view', async () => {
    const user = userEvent.setup();
    const seeded = { ...baseSettings, revision: 1, profiles: [P_A, P_B], activeProfileId: 'a' };
    // Committed elsewhere meanwhile: a style change and an edit to profile B.
    const latest: Settings = {
      ...seeded,
      revision: 3,
      answerStyle: 'detailed',
      profiles: [P_A, { ...P_B, focus: 'Edited elsewhere' }],
    };
    const onSave = vi
      .fn<(p: SettingsPatch) => Promise<Envelope<Settings>>>()
      .mockResolvedValueOnce(err('settings_conflict', 'Settings changed while this form was open. Reload it.'))
      .mockResolvedValueOnce(ok({ ...latest, revision: 4 }));
    const onReload = vi.fn(async () => ok(latest));
    render(<SettingsView settings={seeded} onSave={onSave} onReload={onReload} onBack={vi.fn()} />);
    await user.type(screen.getByLabelText('Resume'), ' + typed');
    await user.type(screen.getByLabelText('Anthropic API key'), 'sk-typed');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    const banner = await screen.findByRole('alert');
    expect(banner).toHaveTextContent('Settings changed elsewhere — reload');
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    expect(screen.getByLabelText('Resume')).toHaveValue(`${P_A.resume} + typed`);

    await user.click(within(banner).getByRole('button', { name: 'Reload' }));
    expect(await screen.findByText(/Reloaded\. Your unsaved edits were kept/)).toBeInTheDocument();
    expect(screen.queryByText(/Settings changed elsewhere/)).not.toBeInTheDocument();
    // Kept: the typed resume and key. Taken: the untouched style and profile B.
    expect(screen.getByLabelText('Resume')).toHaveValue(`${P_A.resume} + typed`);
    expect(screen.getByLabelText('Anthropic API key')).toHaveValue('sk-typed');
    expect(screen.getByLabelText('Answer style')).toHaveValue('detailed');

    await user.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText('Saved ✓');
    const patch = lastPatch(onSave);
    expect(patch.expectedRevision).toBe(3);
    expect(patch.answerStyle).toBe('detailed');
    expect(patch.anthropicKey).toBe('sk-typed');
    expect(patch.profiles?.[0]?.resume).toBe(`${P_A.resume} + typed`);
    expect(patch.profiles?.[1]?.focus).toBe('Edited elsewhere');
  });

  it('an untouched open form silently follows a newer committed view', async () => {
    const onSave = vi.fn(async (_p: SettingsPatch) => ok({ ...baseSettings, revision: 3 }));
    const { rerender } = render(
      <SettingsView settings={baseSettings} onSave={onSave} onReload={vi.fn()} onBack={vi.fn()} />
    );
    const newer = { ...baseSettings, revision: 2, answerStyle: 'brief' as const };
    rerender(<SettingsView settings={newer} onSave={onSave} onReload={vi.fn()} onBack={vi.fn()} />);
    await waitFor(() => expect(screen.getByLabelText('Answer style')).toHaveValue('brief'));
    expect(screen.queryByText(/Settings changed elsewhere/)).not.toBeInTheDocument();
    await userEvent.setup().click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(onSave).toHaveBeenCalled());
    expect(lastPatch(onSave).expectedRevision).toBe(2);
  });

  it('a dirty open form is told at once when a newer view arrives, and keeps its edits', async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <SettingsView settings={baseSettings} onSave={vi.fn()} onReload={vi.fn()} onBack={vi.fn()} />
    );
    await user.type(screen.getByLabelText('Focus (optional)'), 'tokio');
    const newer = { ...baseSettings, revision: 2, answerStyle: 'brief' as const };
    rerender(<SettingsView settings={newer} onSave={vi.fn()} onReload={vi.fn()} onBack={vi.fn()} />);
    expect(await screen.findByText(/Settings changed elsewhere — reload/)).toBeInTheDocument();
    expect(screen.getByLabelText('Focus (optional)')).toHaveValue('tokio');
    expect(screen.getByLabelText('Answer style')).toHaveValue('balanced');
  });

  it('undoing the edits that raised the banner follows the newer view and clears the banner', async () => {
    const user = userEvent.setup();
    const onSave = vi.fn(async (_p: SettingsPatch) => ok({ ...baseSettings, revision: 3 }));
    const { rerender } = render(
      <SettingsView settings={baseSettings} onSave={onSave} onReload={vi.fn()} onBack={vi.fn()} />
    );
    await user.type(screen.getByLabelText('Focus (optional)'), 'x');
    const newer = { ...baseSettings, revision: 2, answerStyle: 'brief' as const };
    rerender(<SettingsView settings={newer} onSave={onSave} onReload={vi.fn()} onBack={vi.fn()} />);
    expect(await screen.findByText(/Settings changed elsewhere — reload/)).toBeInTheDocument();
    await user.type(screen.getByLabelText('Focus (optional)'), '{Backspace}');
    await waitFor(() => expect(screen.queryByText(/Settings changed elsewhere/)).not.toBeInTheDocument());
    expect(screen.getByLabelText('Answer style')).toHaveValue('brief');
    expect(screen.getByRole('button', { name: 'Save' })).toBeEnabled();
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1));
    expect(lastPatch(onSave).expectedRevision).toBe(2);
  });

  it('shows the startup storage warning', () => {
    renderSettings({ storageWarning: 'Your settings file was damaged and could not be read.' });
    expect(screen.getByRole('note')).toHaveTextContent('Your settings file was damaged');
  });
});

describe('local budget (R4)', () => {
  function budgetAnswers(...answers: LocalPromptBudget[]) {
    for (const b of answers) bridge.localPromptBudget.mockResolvedValueOnce(ok(b));
  }

  it('previews the UNSAVED draft and shows the remaining bytes', async () => {
    const user = userEvent.setup();
    renderSettings({ llmProvider: 'local' });
    expect(await screen.findByText(/6,000 bytes left for the question/)).toBeInTheDocument();
    bridge.localPromptBudget.mockResolvedValue(ok(makeBudget({ remainingBytes: 5_990, usedBytes: 1_010 })));
    await user.type(screen.getByLabelText('Focus (optional)'), 'Rust');
    expect(await screen.findByText(/5,990 bytes left for the question/)).toBeInTheDocument();
    const [profile, style, question] = bridge.localPromptBudget.mock.calls.at(-1) ?? [];
    expect(profile?.focus).toBe('Rust');
    expect(style).toBe('balanced');
    expect(question).toBe('');
  });

  it('separates little room (a warning) from over the limit (blocking local use), and Save stays enabled', async () => {
    const user = userEvent.setup();
    budgetAnswers(makeBudget({ remainingBytes: 150, status: 'tight' }));
    const { onSave } = renderSettings({ llmProvider: 'local' });
    const tight = await screen.findByText(/Only 150 bytes left for the question/);
    expect(tight).toHaveClass('field-help--warn');

    bridge.localPromptBudget.mockResolvedValue(ok(makeBudget({ usedBytes: 7_274, remainingBytes: -274, status: 'over' })));
    await user.type(screen.getByLabelText('Focus (optional)'), 'x');
    const over = await screen.findByText(/Too long for free local mode/);
    expect(over).toHaveClass('field-help--error');
    expect(over).toHaveTextContent('7,274 of 7,000');
    expect(over).toHaveTextContent('you can still save it for a cloud model');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1));
  });

  it('discards an obsolete answer when the style changes again before it lands', async () => {
    const user = userEvent.setup();
    const slow = deferred<Envelope<LocalPromptBudget>>();
    bridge.localPromptBudget.mockImplementationOnce(() => slow.promise);
    renderSettings({ llmProvider: 'local' });
    await waitFor(() => expect(bridge.localPromptBudget).toHaveBeenCalledTimes(1));
    bridge.localPromptBudget.mockResolvedValue(ok(makeBudget({ remainingBytes: 4_321 })));
    await user.selectOptions(screen.getByLabelText('Answer style'), 'detailed');
    expect(await screen.findByText(/4,321 bytes left/)).toBeInTheDocument();
    expect(bridge.localPromptBudget.mock.calls.at(-1)?.[1]).toBe('detailed');
    // The first request, for Balanced, finally answers: it must be ignored.
    await act(async () => slow.resolve(ok(makeBudget({ remainingBytes: 1 }))));
    expect(screen.getByText(/4,321 bytes left/)).toBeInTheDocument();
    expect(screen.queryByText(/^1 bytes left/)).not.toBeInTheDocument();
  });

  it('says so when the size check itself fails, instead of "Checking…" forever', async () => {
    bridge.localPromptBudget.mockResolvedValue(err('internal', 'invoke failed'));
    renderSettings({ llmProvider: 'local' });
    expect(await screen.findByText(/Could not check the free local mode size/)).toBeInTheDocument();
    expect(screen.queryByText(/Checking the free local mode size/)).not.toBeInTheDocument();
  });

  it('switching the edited profile asks for that profile and never shows the previous one\'s figure', async () => {
    const user = userEvent.setup();
    bridge.localPromptBudget.mockImplementation(async (profile) =>
      ok(makeBudget({ remainingBytes: profile.id === 'a' ? 1_111 : 2_222 }))
    );
    renderSettings({ llmProvider: 'local', profiles: [P_A, P_B], activeProfileId: 'a' });
    expect(await screen.findByText(/1,111 bytes left/)).toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText('Profile'), 'b');
    expect(screen.queryByText(/1,111 bytes left/)).not.toBeInTheDocument();
    expect(await screen.findByText(/2,222 bytes left/)).toBeInTheDocument();
  });
});

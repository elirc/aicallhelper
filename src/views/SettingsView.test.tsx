import { describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { Envelope, SettingsPatch, SettingsView as Settings } from '../types';
import { SettingsView } from './SettingsView';
import { baseSettings, err, ok } from './testUtils';

function renderSettings(overrides: Partial<Settings> = {}, saveResult?: Envelope<Settings>) {
  const settings = { ...baseSettings, ...overrides };
  const onSave = vi.fn(async (_patch: SettingsPatch) => saveResult ?? ok(settings));
  const onBack = vi.fn();
  const utils = render(<SettingsView settings={settings} onSave={onSave} onBack={onBack} />);
  return { ...utils, onSave, onBack, settings };
}

function lastPatch(onSave: ReturnType<typeof vi.fn>): SettingsPatch {
  const call = onSave.mock.calls[onSave.mock.calls.length - 1];
  expect(call).toBeDefined();
  return call?.[0] as SettingsPatch;
}

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
    expect(patch.resume).toBe(baseSettings.resume);
    expect(patch.jobDescription).toBe(baseSettings.jobDescription);
    expect(patch.alwaysOnTop).toBe(baseSettings.alwaysOnTop);
    expect(patch.llmProvider).toBe(baseSettings.llmProvider);
    expect(patch.answerStyle).toBe(baseSettings.answerStyle);
    expect(patch.hotkey).toBe(baseSettings.hotkey);
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
});

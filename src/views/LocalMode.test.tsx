import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { createRef } from 'react';
import * as bridge from '../bridge';
import { SettingsView } from './SettingsView';
import { MainView } from './MainView';
import { baseSettings, makeSession, ok, err } from './testUtils';

beforeEach(() => {
  vi.spyOn(bridge, 'checkLocalVoice').mockResolvedValue(ok({
    ollamaRunning: false, modelAvailable: false, speechReady: false,
  }));
  vi.spyOn(bridge, 'prepareLocalVoice').mockResolvedValue(ok({
    ollamaRunning: true, modelAvailable: true, speechReady: true,
  }));
});
afterEach(() => vi.restoreAllMocks());

test('free mode saves with no cloud keys and preserves shared profile and controls', async () => {
  const onSave = vi.fn(async () => ok({ ...baseSettings, llmProvider: 'local' as const }));
  render(<SettingsView settings={baseSettings} onSave={onSave} onBack={vi.fn()} />);
  fireEvent.change(screen.getByLabelText('Answer model'), { target: { value: 'local' } });
  expect(await screen.findByRole('region', { name: 'Free local voice setup' })).toBeVisible();
  fireEvent.click(screen.getByRole('button', { name: 'Save' }));
  await waitFor(() => expect(onSave).toHaveBeenCalledOnce());
  const patch = onSave.mock.calls[0] as unknown as [Record<string, unknown>];
  expect(patch[0]).toMatchObject({ llmProvider: 'local', resume: baseSettings.resume, answerStyle: 'balanced' });
  expect(patch[0]).not.toHaveProperty('deepgramKey');
  expect(patch[0]).not.toHaveProperty('anthropicKey');
  expect(patch[0]).not.toHaveProperty('groqKey');
});

test('warm-up updates readiness and exposes actionable setup failures', async () => {
  render(<SettingsView settings={{ ...baseSettings, llmProvider: 'local' }} onSave={vi.fn()} onBack={vi.fn()} />);
  const warm = await screen.findByRole('button', { name: 'Start and warm free mode' });
  await waitFor(() => expect(warm).toBeEnabled());
  fireEvent.click(warm);
  expect(await screen.findByText(/Ready. Save this mode/)).toBeVisible();
  expect(screen.getByText('Installed')).toBeVisible();
  vi.mocked(bridge.prepareLocalVoice).mockResolvedValueOnce(err('internal', 'Free disk space, then rerun setup.'));
  fireEvent.click(warm);
  expect(await screen.findByText('Free disk space, then rerun setup.')).toBeVisible();
  expect(warm).toBeEnabled();
});

test('main local mode records without the missing cloud-key nudge', () => {
  const session = makeSession();
  render(<MainView
    settings={{ ...baseSettings, llmProvider: 'local', hasDeepgramKey: false, hasAnthropicKey: false, hasGroqKey: false }}
    session={session} hotkey={null} onOpenSettings={vi.fn()}
    onSelectStyle={vi.fn()} gearRef={createRef<HTMLButtonElement>()}
  />);
  expect(screen.getByText('Free local voice · no API fees')).toBeVisible();
  expect(screen.queryByText(/add your API keys/i)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: /Record/ }));
  expect(session.toggleRecord).toHaveBeenCalledOnce();
});

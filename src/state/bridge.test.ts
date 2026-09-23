/**
 * The real bridge's whole job is "one error path": anything invoke throws must
 * come back as an envelope, never as a rejection the UI has to catch.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getBridge, setBridge, type Bridge } from '../bridge';
import type { Envelope, SessionId } from '../types';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

const ok = <T,>(value: T): Envelope<T> => ({ ok: true, value });

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(() => undefined);
});

describe('envelope discipline', () => {
  it('converts a rejected invoke into an internal-code error envelope', async () => {
    mockInvoke.mockRejectedValueOnce(new Error('ipc exploded'));
    const env = await getBridge().startSession();
    expect(env).toEqual({ ok: false, error: { code: 'internal', message: 'ipc exploded' } });
  });

  it('stringifies non-Error throwables so the message is never "[object Object]" surprise-free', async () => {
    mockInvoke.mockRejectedValueOnce('plain string failure');
    const env = await getBridge().getSettings();
    expect(env).toEqual({ ok: false, error: { code: 'internal', message: 'plain string failure' } });
  });

  it('passes a resolved envelope through untouched', async () => {
    mockInvoke.mockResolvedValueOnce(ok<SessionId>(41));
    const env = await getBridge().startSession();
    expect(env).toEqual(ok(41));
  });

  it('never rejects: every command method resolves even when invoke throws', async () => {
    mockInvoke.mockRejectedValue(new Error('down'));
    const b = getBridge();
    await expect(b.getSettings()).resolves.toMatchObject({ ok: false });
    await expect(b.setSettings({})).resolves.toMatchObject({ ok: false });
    await expect(b.startSession()).resolves.toMatchObject({ ok: false });
    await expect(b.stopSession(1)).resolves.toMatchObject({ ok: false });
    await expect(b.ask('q')).resolves.toMatchObject({ ok: false });
    await expect(b.sessionOutcome(1)).resolves.toMatchObject({ ok: false });
    await expect(b.hotkeyStatus()).resolves.toMatchObject({ ok: false });
    await expect(b.dockToCamera()).resolves.toMatchObject({ ok: false });
    await expect(b.localVoiceStatus()).resolves.toMatchObject({ ok: false });
    await expect(b.prepareLocalVoice()).resolves.toMatchObject({ ok: false });
    const profile = { id: 'a', name: 'A', callType: 'interview' as const, resume: '', jobDescription: '', focus: '', extraInstructions: '' };
    await expect(b.localPromptBudget(profile, 'balanced', '')).resolves.toMatchObject({ ok: false });
  });
});

describe('command wiring', () => {
  it('uses the snake_case command names and argument shapes the core registered', async () => {
    mockInvoke.mockResolvedValue(ok(null));
    const b = getBridge();
    await b.getSettings();
    expect(mockInvoke).toHaveBeenLastCalledWith('get_settings', undefined);
    await b.setSettings({ activeProfileId: 'b' });
    expect(mockInvoke).toHaveBeenLastCalledWith('set_settings', { patch: { activeProfileId: 'b' } });
    await b.startSession();
    expect(mockInvoke).toHaveBeenLastCalledWith('start_session', undefined);
    // Tauri maps the Rust `session_id` parameter to a camelCase `sessionId`
    // argument key — `{ id }` would fail every stop with a missing-key error.
    await b.stopSession(4);
    expect(mockInvoke).toHaveBeenLastCalledWith('stop_session', { sessionId: 4 });
    await b.ask('hello');
    expect(mockInvoke).toHaveBeenLastCalledWith('ask', { text: 'hello' });
    // Same camelCase argument-key rule as stop_session (R1 reconciliation).
    await b.sessionOutcome(4);
    expect(mockInvoke).toHaveBeenLastCalledWith('session_outcome', { sessionId: 4 });
    await b.hotkeyStatus();
    expect(mockInvoke).toHaveBeenLastCalledWith('hotkey_status', undefined);
    await b.dockToCamera();
    expect(mockInvoke).toHaveBeenLastCalledWith('dock_to_camera', undefined);
    await b.localVoiceStatus();
    expect(mockInvoke).toHaveBeenLastCalledWith('local_voice_status', undefined);
    await b.prepareLocalVoice();
    expect(mockInvoke).toHaveBeenLastCalledWith('prepare_local_voice', undefined);
    // R4: Rust's `answer_style` parameter is the camelCase `answerStyle` key.
    const profile = { id: 'a', name: 'A', callType: 'interview' as const, resume: 'R', jobDescription: '', focus: '', extraInstructions: '' };
    await b.localPromptBudget(profile, 'brief', 'Hi?');
    expect(mockInvoke).toHaveBeenLastCalledWith('local_prompt_budget', { profile, answerStyle: 'brief', question: 'Hi?' });
    b.cancelSession(9);
    expect(mockInvoke).toHaveBeenLastCalledWith('cancel_session', { sessionId: 9 });
  });

  it('cancelSession swallows a rejection (cancel races teardown by design)', async () => {
    mockInvoke.mockRejectedValueOnce(new Error('already gone'));
    getBridge().cancelSession(2);
    // A leaked rejection would fail the test run as unhandled.
    await new Promise((r) => setTimeout(r, 0));
    expect(mockInvoke).toHaveBeenCalledWith('cancel_session', { sessionId: 2 });
  });
});

describe('events', () => {
  // Holder object rather than a bare let: TS narrows a closure-assigned let
  // to its initializer and would reject the later call as `never`.
  type TauriCallback = (event: { payload: unknown }) => void;

  function captureListen(result: Promise<() => void>) {
    const holder: { cb: TauriCallback | null } = { cb: null };
    mockListen.mockImplementationOnce((_name, cb) => {
      holder.cb = cb as TauriCallback;
      return result;
    });
    return (payload: unknown): void => {
      if (holder.cb === null) throw new Error('listen was never registered');
      holder.cb({ payload });
    };
  }

  it('delivers listened payloads to the handler and unlistens on unsubscribe', async () => {
    const unlisten = vi.fn();
    const fire = captureListen(Promise.resolve(unlisten));

    const handler = vi.fn();
    const sub = getBridge().on('llm:delta', handler);
    await expect(sub.ready).resolves.toEqual({ ok: true, value: null });
    fire({ sessionId: 1, delta: 'hi' });
    expect(handler).toHaveBeenCalledWith({ sessionId: 1, delta: 'hi' });

    sub.unsubscribe();
    expect(unlisten).toHaveBeenCalledTimes(1);
    fire({ sessionId: 1, delta: 'late' });
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it('unsubscribing before listen resolves still detaches', async () => {
    let resolveListen!: (un: () => void) => void;
    const unlisten = vi.fn();
    const handler = vi.fn();
    const fire = captureListen(
      new Promise<() => void>((r) => {
        resolveListen = r;
      }),
    );

    const sub = getBridge().on('llm:delta', handler);
    sub.unsubscribe(); // torn down before the Tauri registration finished
    resolveListen(unlisten);
    await Promise.resolve();
    await Promise.resolve();
    expect(unlisten).toHaveBeenCalledTimes(1);
    fire({ sessionId: 1, delta: 'ghost' });
    expect(handler).not.toHaveBeenCalled();
  });
});

describe('listener readiness (R1)', () => {
  it('a failed registration surfaces as an error envelope instead of being swallowed', async () => {
    // The old bridge caught and dropped this, so the UI started sessions
    // whose events could never arrive and waited forever.
    mockListen.mockRejectedValueOnce(new Error('ipc not ready'));
    const sub = getBridge().on('session:error', vi.fn());
    const ready = await sub.ready;
    expect(ready.ok).toBe(false);
    if (!ready.ok) {
      expect(ready.error.code).toBe('internal');
      expect(ready.error.message).toContain('session:error');
      expect(ready.error.message).toContain('ipc not ready');
    }
    // Unsubscribing a failed registration is harmless.
    expect(() => sub.unsubscribe()).not.toThrow();
  });

  it('ready stays pending until the registration actually completes', async () => {
    let resolveListen!: (un: () => void) => void;
    mockListen.mockImplementationOnce(
      () =>
        new Promise<() => void>((r) => {
          resolveListen = r;
        }),
    );
    const sub = getBridge().on('llm:done', vi.fn());
    let settled = false;
    void sub.ready.then(() => {
      settled = true;
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(settled).toBe(false);
    resolveListen(() => undefined);
    await expect(sub.ready).resolves.toEqual({ ok: true, value: null });
  });
});

describe('test seam', () => {
  it('setBridge replaces what getBridge returns', () => {
    const fake = { marker: true } as unknown as Bridge;
    setBridge(fake);
    expect(getBridge()).toBe(fake);
  });
});

/**
 * Shared fakes for the UI tests: a controllable Bridge (installed via
 * setBridge so the REAL useSession hook runs against it) and a hand-built
 * SessionApi for prop-driven view tests where a specific state must be pinned
 * without walking the whole machine there.
 */
import { vi } from 'vitest';
import type { Bridge } from '../bridge';
import type {
  Envelope,
  ErrorCode,
  EventMap,
  EventName,
  SessionId,
  SettingsPatch,
  SettingsView,
} from '../types';
import type { HistoryEntry, SessionApi } from '../state/useSession';

export function ok<T>(value: T): Envelope<T> {
  return { ok: true, value };
}

export function err<T>(code: ErrorCode, message: string): Envelope<T> {
  return { ok: false, error: { code, message } };
}

export const baseSettings: SettingsView = {
  resume: 'Senior engineer, 8 years.',
  jobDescription: 'Backend role at Initech.',
  alwaysOnTop: false,
  llmProvider: 'anthropic',
  answerStyle: 'balanced',
  hotkey: 'CommandOrControl+Shift+Space',
  hasDeepgramKey: true,
  hasAnthropicKey: true,
  hasGroqKey: false,
};

type AnyHandler = (payload: never) => void;

export class FakeBridge implements Bridge {
  settings: SettingsView;
  hotkey = { accelerator: 'CommandOrControl+Shift+Space', registered: true };
  private handlers = new Map<EventName, Set<AnyHandler>>();
  private nextId = 1;

  constructor(settings: Partial<SettingsView> = {}) {
    this.settings = { ...baseSettings, ...settings };
  }

  getSettings = vi.fn(async (): Promise<Envelope<SettingsView>> => ok({ ...this.settings }));

  setSettings = vi.fn(async (patch: SettingsPatch): Promise<Envelope<SettingsView>> => {
    const { deepgramKey, anthropicKey, groqKey, ...rest } = patch;
    this.settings = { ...this.settings, ...rest };
    if (deepgramKey !== undefined) this.settings.hasDeepgramKey = deepgramKey !== '';
    if (anthropicKey !== undefined) this.settings.hasAnthropicKey = anthropicKey !== '';
    if (groqKey !== undefined) this.settings.hasGroqKey = groqKey !== '';
    return ok({ ...this.settings });
  });

  startSession = vi.fn(async (): Promise<Envelope<SessionId>> => ok(this.nextId++));

  stopSession = vi.fn(async (_id: SessionId): Promise<Envelope<null>> => ok(null));

  ask = vi.fn(async (_text: string): Promise<Envelope<SessionId>> => ok(this.nextId++));

  cancelSession = vi.fn((_id: SessionId): void => undefined);

  hotkeyStatus = vi.fn(
    async (): Promise<Envelope<{ accelerator: string; registered: boolean }>> => ok({ ...this.hotkey })
  );

  on = <K extends EventName>(name: K, handler: (payload: EventMap[K]) => void): (() => void) => {
    let set = this.handlers.get(name);
    if (set === undefined) {
      set = new Set();
      this.handlers.set(name, set);
    }
    set.add(handler as AnyHandler);
    return () => {
      set.delete(handler as AnyHandler);
    };
  };

  /** Push a core event to every subscriber. Wrap calls in act(). */
  emit<K extends EventName>(name: K, payload: EventMap[K]): void {
    const set = this.handlers.get(name);
    if (set === undefined) return;
    for (const handler of [...set]) (handler as (payload: EventMap[K]) => void)(payload);
  }
}

export function makeEntry(overrides: Partial<HistoryEntry> = {}): HistoryEntry {
  return {
    key: 'attempt-1',
    sessionId: 1,
    question: 'What is REST?',
    answer: 'Use the HTTP verbs.',
    metrics: null,
    ...overrides,
  };
}

export function makeSession(overrides: Partial<SessionApi> = {}): SessionApi {
  return {
    state: 'idle',
    error: null,
    rms: 0,
    elapsedMs: 0,
    hitRecordingCap: false,
    history: [],
    viewIndex: 0,
    viewed: null,
    toggleRecord: vi.fn(),
    submitAsk: vi.fn(async () => true),
    regenerate: vi.fn(),
    clearHistory: vi.fn(),
    viewPrev: vi.fn(),
    viewNext: vi.fn(),
    setError: vi.fn(),
    ...overrides,
  };
}

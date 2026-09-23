/**
 * Shared fakes for the UI tests: a controllable Bridge (installed via
 * setBridge so the REAL useSession hook runs against it) and a hand-built
 * SessionApi for prop-driven view tests where a specific state must be pinned
 * without walking the whole machine there.
 */
import { vi } from 'vitest';
import type { Bridge, Subscription } from '../bridge';
import type {
  AnswerStyle,
  CallProfile,
  Envelope,
  ErrorCode,
  EventMap,
  EventName,
  HotkeyStatus,
  LocalPromptBudget,
  LocalVoiceStatus,
  SessionId,
  SessionOutcome,
  SessionStart,
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

/** A complete profile in the wire shape; override what a test pins. */
export function makeProfile(overrides: Partial<CallProfile> = {}): CallProfile {
  return {
    id: 'default',
    name: 'Default',
    callType: 'interview',
    resume: 'Senior engineer, 8 years.',
    jobDescription: 'Backend role at Initech.',
    focus: '',
    extraInstructions: '',
    ...overrides,
  };
}

export const baseSettings: SettingsView = {
  revision: 1,
  storageWarning: null,
  profiles: [makeProfile()],
  activeProfileId: 'default',
  alwaysOnTop: false,
  llmProvider: 'anthropic',
  answerStyle: 'balanced',
  hotkey: 'Ctrl+Shift+Space',
  launchPlacement: 'camera',
  streamFollow: 'tail',
  hasDeepgramKey: true,
  hasAnthropicKey: true,
  hasGroqKey: false,
};

type AnyHandler = (payload: never) => void;

export class FakeBridge implements Bridge {
  settings: SettingsView;
  hotkey: HotkeyStatus = { accelerator: 'Ctrl+Shift+Space', registered: true };
  private handlers = new Map<EventName, Set<AnyHandler>>();
  private nextId = 1;

  constructor(settings: Partial<SettingsView> = {}) {
    this.settings = { ...baseSettings, ...settings };
  }

  getSettings = vi.fn(async (): Promise<Envelope<SettingsView>> => ok({ ...this.settings }));

  /**
   * Mirrors the core's revision rule (R5, ADR 016): a stale
   * `expectedRevision` is a `settings_conflict` that changes nothing; any
   * committed patch bumps the revision.
   */
  setSettings = vi.fn(async (patch: SettingsPatch): Promise<Envelope<SettingsView>> => {
    const { deepgramKey, anthropicKey, groqKey, expectedRevision, ...rest } = patch;
    if (expectedRevision !== undefined && expectedRevision !== this.settings.revision) {
      return err('settings_conflict', 'Settings changed while this form was open. Reload it.');
    }
    this.settings = { ...this.settings, ...rest, revision: this.settings.revision + 1 };
    if (deepgramKey !== undefined) this.settings.hasDeepgramKey = deepgramKey !== '';
    if (anthropicKey !== undefined) this.settings.hasAnthropicKey = anthropicKey !== '';
    if (groqKey !== undefined) this.settings.hasGroqKey = groqKey !== '';
    return ok({ ...this.settings });
  });

  startSession = vi.fn(
    async (): Promise<Envelope<SessionStart>> => ok({ sessionId: this.nextId++, outcome: { status: 'active' } })
  );

  stopSession = vi.fn(async (_id: SessionId): Promise<Envelope<null>> => ok(null));

  ask = vi.fn(
    async (_text: string): Promise<Envelope<SessionStart>> =>
      ok({ sessionId: this.nextId++, outcome: { status: 'active' } })
  );

  /** Default: still running — events decide. Tests override per case (R1). */
  sessionOutcome = vi.fn(async (_id: SessionId): Promise<Envelope<SessionOutcome>> => ok({ status: 'active' }));

  /**
   * What each subscription's `ready` resolves with (R1). Default: registered
   * at once. A test swaps it to delay or fail listener registration.
   */
  listenReady: (name: EventName) => Promise<Envelope<null>> = async () => ok(null);

  cancelSession = vi.fn((_id: SessionId): void => undefined);

  hotkeyStatus = vi.fn(async (): Promise<Envelope<HotkeyStatus>> => ok({ ...this.hotkey }));

  dockToCamera = vi.fn(async (): Promise<Envelope<null>> => ok(null));

  localVoiceStatus = vi.fn(
    async (): Promise<Envelope<LocalVoiceStatus>> =>
      ok({ ollamaRunning: false, modelAvailable: false, speechReady: false })
  );

  prepareLocalVoice = vi.fn(
    async (): Promise<Envelope<LocalVoiceStatus>> =>
      ok({ ollamaRunning: true, modelAvailable: true, speechReady: true })
  );

  /** A roomy local budget by default; tests override per case (R4). */
  localPromptBudget = vi.fn(
    async (_profile: CallProfile, _style: AnswerStyle, question: string): Promise<Envelope<LocalPromptBudget>> =>
      ok(makeBudget({ questionBytes: question.length }))
  );

  on = <K extends EventName>(name: K, handler: (payload: EventMap[K]) => void): Subscription => {
    let set = this.handlers.get(name);
    if (set === undefined) {
      set = new Set();
      this.handlers.set(name, set);
    }
    set.add(handler as AnyHandler);
    return {
      ready: this.listenReady(name),
      unsubscribe: () => {
        set.delete(handler as AnyHandler);
      },
    };
  };

  /** Push a core event to every subscriber. Wrap calls in act(). */
  emit<K extends EventName>(name: K, payload: EventMap[K]): void {
    const set = this.handlers.get(name);
    if (set === undefined) return;
    for (const handler of [...set]) (handler as (payload: EventMap[K]) => void)(payload);
  }
}

/** A local budget in the wire shape; override what a test pins (R4). */
export function makeBudget(overrides: Partial<LocalPromptBudget> = {}): LocalPromptBudget {
  return {
    usedBytes: 1000,
    limitBytes: 7000,
    remainingBytes: 6000,
    fixedBytes: 900,
    profileBytes: 100,
    questionBytes: 0,
    reserveBytes: 200,
    status: 'ok',
    ...overrides,
  };
}

export function makeEntry(overrides: Partial<HistoryEntry> = {}): HistoryEntry {
  return {
    key: 'attempt-1',
    sessionId: 1,
    question: 'What is REST?',
    answer: 'Use the HTTP verbs.',
    metrics: null,
    status: 'completed',
    reason: null,
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

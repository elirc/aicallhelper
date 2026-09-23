/**
 * Hook tests: the reducer rules are proven in reducer.test.ts; these prove
 * the WIRING — bridge calls issued at the right moments, resolutions and
 * events landing (or being dropped) correctly, and the fake-timer-driven
 * recording cap.
 */
import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { setBridge, type Bridge, type Subscription } from '../bridge';
import {
  MAX_RECORDING_SECONDS,
  type AppError,
  type Envelope,
  type EventMap,
  type EventName,
  type Metrics,
  type SessionId,
  type SessionOutcome,
  type SessionStart,
  type SettingsView,
} from '../types';
import { LISTEN_TIMEOUT_ERROR, LISTEN_TIMEOUT_MS, useSession } from './useSession';

const ok = <T,>(value: T): Envelope<T> => ({ ok: true, value });
// Returns the error branch only so it is assignable to any Envelope<T>.
const fail = (code: AppError['code'], message = 'boom'): { ok: false; error: AppError } => ({
  ok: false,
  error: { code, message },
});
const metrics: Metrics = { sttFinalizeMs: 480, firstTokenMs: 950, totalMs: 3210 };
/** A start/ask envelope for a session that is still running (R1). */
const started = (sessionId: SessionId, outcome: SessionOutcome = { status: 'active' }): Envelope<SessionStart> =>
  ok({ sessionId, outcome });

const settings: SettingsView = {
  revision: 1,
  storageWarning: null,
  profiles: [
    { id: 'default', name: 'Default', callType: 'interview', resume: '', jobDescription: '', focus: '', extraInstructions: '' },
  ],
  activeProfileId: 'default',
  alwaysOnTop: true,
  llmProvider: 'anthropic',
  answerStyle: 'balanced',
  hotkey: 'Ctrl+Shift+Space',
  launchPlacement: 'camera',
  streamFollow: 'tail',
  hasDeepgramKey: true,
  hasAnthropicKey: true,
  hasGroqKey: false,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function makeFakeBridge() {
  const handlers = new Map<EventName, Set<(payload: never) => void>>();
  const bridge = {
    getSettings: vi.fn(() => Promise.resolve(ok(settings))),
    setSettings: vi.fn(() => Promise.resolve(ok(settings))),
    startSession: vi.fn(() => Promise.resolve(started(1))),
    stopSession: vi.fn((_id: SessionId) => Promise.resolve(ok<null>(null))),
    ask: vi.fn((_text: string) => Promise.resolve(started(1))),
    sessionOutcome: vi.fn((_id: SessionId) => Promise.resolve(ok<SessionOutcome>({ status: 'active' }))),
    cancelSession: vi.fn((_id: SessionId) => undefined),
    hotkeyStatus: vi.fn(() => Promise.resolve(ok({ accelerator: 'Ctrl+Shift+Space', registered: true }))),
    dockToCamera: vi.fn(() => Promise.resolve(ok<null>(null))),
    localVoiceStatus: vi.fn(() =>
      Promise.resolve(ok({ ollamaRunning: false, modelAvailable: false, speechReady: false }))
    ),
    prepareLocalVoice: vi.fn(() =>
      Promise.resolve(ok({ ollamaRunning: true, modelAvailable: true, speechReady: true }))
    ),
    // Never reached by the hook; present because the Bridge requires it (R4).
    localPromptBudget: vi.fn(() => Promise.resolve(fail('internal', 'not used by useSession'))),
    /** What each subscription's `ready` resolves with; tests delay or fail it (R1). */
    listenReady: (_name: EventName): Promise<Envelope<null>> => Promise.resolve(ok<null>(null)),
    on: <K extends EventName>(name: K, handler: (payload: EventMap[K]) => void): Subscription => {
      let set = handlers.get(name);
      if (set === undefined) {
        set = new Set();
        handlers.set(name, set);
      }
      const h = handler as (payload: never) => void;
      set.add(h);
      return {
        ready: bridge.listenReady(name),
        unsubscribe: () => {
          handlers.get(name)?.delete(h);
        },
      };
    },
  };
  const emit = <K extends EventName>(name: K, payload: EventMap[K]): void => {
    const set = handlers.get(name);
    if (set !== undefined) {
      for (const h of [...set]) (h as (payload: EventMap[K]) => void)(payload);
    }
  };
  return { bridge, emit, handlers };
}

type Fake = ReturnType<typeof makeFakeBridge>;
let fake: Fake;

beforeEach(() => {
  fake = makeFakeBridge();
  setBridge(fake.bridge as Bridge);
});

afterEach(() => {
  // Restore the performance.now spy BEFORE uninstalling fake timers: sinon's
  // uninstall reassigns the true original, and restoring a spy afterwards
  // would put a dead fake back onto the global for later tests.
  vi.restoreAllMocks();
  vi.useRealTimers();
});

const flush = () => act(async () => {});

/**
 * The recording clock reads performance.now() deltas (WebView2 throttles
 * background timers, so fire counts lie about wall time — §3). Tests pin the
 * wall clock themselves so fake-timer config can't change what a "fire" means.
 */
function mockPerfNow() {
  let now = 0;
  vi.spyOn(performance, 'now').mockImplementation(() => now);
  return {
    advance: (ms: number): void => {
      now += ms;
    },
  };
}

/** Drives the hook to mid-recording with the given adopted session id. */
async function startRecording(result: { current: ReturnType<typeof useSession> }, id: SessionId) {
  fake.bridge.startSession.mockResolvedValueOnce(started(id));
  act(() => {
    result.current.toggleRecord();
  });
  await flush();
  expect(result.current.state).toBe('recording');
}

describe('recording round trip', () => {
  it('idle -> starting -> recording -> finalizing -> answering -> idle', async () => {
    const { result } = renderHook(() => useSession());
    fake.bridge.startSession.mockResolvedValueOnce(started(11));

    act(() => {
      result.current.toggleRecord();
    });
    expect(result.current.state).toBe('starting');
    expect(result.current.history).toHaveLength(1);
    expect(result.current.viewIndex).toBe(0);

    await flush();
    expect(result.current.state).toBe('recording');

    act(() => {
      fake.emit('audio:level', { sessionId: 11, rms: 0.42 });
      fake.emit('stt:partial', { sessionId: 11, text: 'how do I', isFinal: false });
    });
    expect(result.current.rms).toBe(0.42);
    expect(result.current.viewed?.question).toBe('how do I');

    act(() => {
      result.current.toggleRecord();
    });
    expect(result.current.state).toBe('finalizing');
    await flush();
    expect(fake.bridge.stopSession).toHaveBeenCalledWith(11);

    act(() => {
      fake.emit('llm:delta', { sessionId: 11, delta: 'Use ' });
    });
    expect(result.current.state).toBe('answering');
    act(() => {
      fake.emit('llm:delta', { sessionId: 11, delta: 'refs.' });
      fake.emit('llm:done', { sessionId: 11, transcript: 'how do I test hooks', answer: 'Use refs.', metrics, stopReason: 'complete' });
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.viewed).toMatchObject({
      sessionId: 11,
      question: 'how do I test hooks',
      answer: 'Use refs.',
      metrics,
    });
  });

  it('the hotkey event toggles exactly like the button', async () => {
    const { result } = renderHook(() => useSession());
    act(() => {
      fake.emit('hotkey:toggle', null);
    });
    expect(result.current.state).toBe('starting');
    await flush();
    expect(result.current.state).toBe('recording');
    act(() => {
      fake.emit('hotkey:toggle', null);
    });
    expect(result.current.state).toBe('finalizing');
  });
});

describe('hotkey gate', () => {
  // §9: the shortcut is IGNORED while Settings is open (the user may be
  // typing the accelerator itself). The hook consults the option per event,
  // so the caller's closure can flip without re-subscribing.
  it('drops hotkey:toggle while hotkeyEnabled() is false and honors it once true', async () => {
    let enabled = false;
    const { result } = renderHook(() => useSession({ hotkeyEnabled: () => enabled }));
    act(() => {
      fake.emit('hotkey:toggle', null);
    });
    await flush();
    expect(fake.bridge.startSession).not.toHaveBeenCalled();
    expect(result.current.state).toBe('idle');

    enabled = true;
    act(() => {
      fake.emit('hotkey:toggle', null);
    });
    await flush();
    expect(fake.bridge.startSession).toHaveBeenCalledTimes(1);
    expect(result.current.state).toBe('recording');
  });

  it('is enabled by default', async () => {
    const { result } = renderHook(() => useSession());
    act(() => {
      fake.emit('hotkey:toggle', null);
    });
    await flush();
    expect(result.current.state).toBe('recording');
  });
});

describe('stale events', () => {
  it("holds events arriving before the start call resolved, then replays only the adopted id's", async () => {
    // R1 changed this contract on purpose. The old rule dropped every
    // pre-adoption event, which is how an early error or answer vanished.
    // Events are now HELD for the pending attempt and replayed on adoption —
    // but only those tagged with the adopted id, so a superseded session
    // still cannot write into the new one. Audio levels are never held.
    const { result } = renderHook(() => useSession());
    const start = deferred<Envelope<SessionStart>>();
    fake.bridge.startSession.mockReturnValueOnce(start.promise);

    await act(async () => {
      result.current.toggleRecord();
    });
    act(() => {
      fake.emit('audio:level', { sessionId: 5, rms: 0.9 });
      fake.emit('stt:partial', { sessionId: 4, text: 'ghost of a superseded session', isFinal: false });
      fake.emit('stt:partial', { sessionId: 5, text: 'early words', isFinal: false });
    });
    // Nothing is dispatched while the id is unknown.
    expect(result.current.rms).toBe(0);
    expect(result.current.viewed?.question).toBe('');

    start.resolve(started(5));
    await flush();
    expect(result.current.state).toBe('recording');
    expect(result.current.viewed?.question).toBe('early words');
    act(() => {
      fake.emit('audio:level', { sessionId: 5, rms: 0.7 });
    });
    expect(result.current.rms).toBe(0.7);
  });

  it('drops events carrying a different sessionId', async () => {
    const { result } = renderHook(() => useSession());
    await startRecording(result, 5);
    act(() => {
      fake.emit('stt:partial', { sessionId: 999, text: 'not mine', isFinal: false });
      fake.emit('audio:level', { sessionId: 999, rms: 1 });
      fake.emit('session:error', { sessionId: 999, error: { code: 'internal', message: 'x' } });
    });
    expect(result.current.state).toBe('recording');
    expect(result.current.viewed?.question).toBe('');
    expect(result.current.rms).toBe(0);
    expect(result.current.error).toBeNull();
  });
});

describe('abort while starting', () => {
  it('returns to idle silently and cancels the session if it starts anyway', async () => {
    const { result } = renderHook(() => useSession());
    // Let listener readiness settle first (R1): an abort during the readiness
    // wait never reaches startSession at all, so there would be no orphan.
    await flush();
    const start = deferred<Envelope<SessionStart>>();
    fake.bridge.startSession.mockReturnValueOnce(start.promise);

    act(() => {
      result.current.toggleRecord();
    });
    expect(result.current.state).toBe('starting');
    act(() => {
      result.current.toggleRecord();
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toBeNull();
    expect(result.current.history).toHaveLength(0);

    start.resolve(started(7));
    await flush();
    // The session the core opened after the abort is an orphan.
    expect(fake.bridge.cancelSession).toHaveBeenCalledWith(7);
    expect(result.current.state).toBe('idle');
  });

  it('a failed start discards the empty attempt and surfaces the error', async () => {
    const { result } = renderHook(() => useSession());
    fake.bridge.startSession.mockResolvedValueOnce(fail('no_stt_key', 'add a key'));
    act(() => {
      result.current.toggleRecord();
    });
    await flush();
    expect(result.current.state).toBe('idle');
    expect(result.current.history).toHaveLength(0);
    expect(result.current.error).toEqual({ code: 'no_stt_key', message: 'add a key' });
  });
});

describe('stop not taken', () => {
  it('returns to idle immediately instead of hanging in finalizing; transcript retired', async () => {
    const { result } = renderHook(() => useSession());
    await startRecording(result, 3);
    act(() => {
      fake.emit('stt:partial', { sessionId: 3, text: 'my question', isFinal: false });
    });
    fake.bridge.stopSession.mockResolvedValueOnce(fail('internal', 'core gone'));
    act(() => {
      result.current.toggleRecord();
    });
    expect(result.current.state).toBe('finalizing');
    await flush();
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toEqual({ code: 'internal', message: 'core gone' });
    expect(result.current.history.map((e) => e.question)).toEqual(['my question']);
  });

  it('discards the attempt when nothing was captured', async () => {
    const { result } = renderHook(() => useSession());
    await startRecording(result, 3);
    fake.bridge.stopSession.mockResolvedValueOnce(fail('no_speech'));
    act(() => {
      result.current.toggleRecord();
    });
    await flush();
    expect(result.current.state).toBe('idle');
    expect(result.current.history).toHaveLength(0);
  });
});

describe('aborted is never surfaced', () => {
  it('a session:error with code aborted tears down silently, keeping captured work', async () => {
    const { result } = renderHook(() => useSession());
    await startRecording(result, 4);
    act(() => {
      fake.emit('stt:partial', { sessionId: 4, text: 'kept words', isFinal: false });
      fake.emit('session:error', { sessionId: 4, error: { code: 'aborted', message: 'cancelled' } });
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toBeNull();
    expect(result.current.history.map((e) => e.question)).toEqual(['kept words']);
  });
});

describe('submitAsk', () => {
  it('refuses while starting, recording, and finalizing without touching the live session', async () => {
    const { result } = renderHook(() => useSession());
    const start = deferred<Envelope<SessionStart>>();
    fake.bridge.startSession.mockReturnValueOnce(start.promise);
    act(() => {
      result.current.toggleRecord();
    });
    await expect(result.current.submitAsk('question')).resolves.toBe(false); // starting

    start.resolve(started(6));
    await flush();
    act(() => {
      fake.emit('stt:partial', { sessionId: 6, text: 'live words', isFinal: false });
    });
    await expect(result.current.submitAsk('question')).resolves.toBe(false); // recording
    expect(result.current.state).toBe('recording');
    expect(result.current.viewed?.question).toBe('live words');

    const stop = deferred<Envelope<null>>();
    fake.bridge.stopSession.mockReturnValueOnce(stop.promise);
    act(() => {
      result.current.toggleRecord();
    });
    await expect(result.current.submitAsk('question')).resolves.toBe(false); // finalizing
    expect(fake.bridge.ask).not.toHaveBeenCalled();
    expect(result.current.history).toHaveLength(1);
  });

  it('refuses empty and whitespace-only text', async () => {
    const { result } = renderHook(() => useSession());
    await expect(result.current.submitAsk('')).resolves.toBe(false);
    await expect(result.current.submitAsk('   \n\t ')).resolves.toBe(false);
    expect(fake.bridge.ask).not.toHaveBeenCalled();
    expect(result.current.history).toHaveLength(0);
  });

  it('resolves true only when the core accepted it', async () => {
    const { result } = renderHook(() => useSession());
    fake.bridge.ask.mockResolvedValueOnce(started(21));
    let accepted = false;
    await act(async () => {
      accepted = await result.current.submitAsk('what is x?');
    });
    expect(accepted).toBe(true);
    expect(fake.bridge.ask).toHaveBeenCalledWith('what is x?');
    expect(result.current.state).toBe('answering');
    expect(result.current.viewed).toMatchObject({ sessionId: 21, question: 'what is x?' });
  });

  it('a rejected ask resolves false, retires the typed question, surfaces the error', async () => {
    const { result } = renderHook(() => useSession());
    fake.bridge.ask.mockResolvedValueOnce(fail('llm_rate_limit', 'slow down'));
    let accepted = true;
    await act(async () => {
      accepted = await result.current.submitAsk('my question');
    });
    expect(accepted).toBe(false);
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toEqual({ code: 'llm_rate_limit', message: 'slow down' });
    expect(result.current.history.map((e) => e.question)).toEqual(['my question']);
  });

  it('asking during a streaming answer supersedes it: old session cancelled, its events dropped', async () => {
    const { result } = renderHook(() => useSession());
    fake.bridge.ask.mockResolvedValueOnce(started(1));
    await act(async () => {
      await result.current.submitAsk('first');
    });
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'partial ' });
    });
    expect(result.current.state).toBe('answering');

    fake.bridge.ask.mockResolvedValueOnce(started(2));
    let accepted = false;
    await act(async () => {
      accepted = await result.current.submitAsk('second');
    });
    expect(accepted).toBe(true);
    expect(fake.bridge.cancelSession).toHaveBeenCalledWith(1);
    // The superseded partial is retired, not lost.
    expect(result.current.history.map((e) => e.question)).toEqual(['first', 'second']);
    expect(result.current.history[0]?.answer).toBe('partial ');

    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'zombie' }); // stale now
      fake.emit('llm:done', { sessionId: 2, transcript: 'second', answer: 'fresh answer', metrics, stopReason: 'complete' });
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.history[0]?.answer).toBe('partial ');
    expect(result.current.history[1]?.answer).toBe('fresh answer');
  });

  it('toggleRecord during a streaming answer also supersedes it', async () => {
    const { result } = renderHook(() => useSession());
    fake.bridge.ask.mockResolvedValueOnce(started(1));
    await act(async () => {
      await result.current.submitAsk('first');
    });
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'part' });
    });
    fake.bridge.startSession.mockResolvedValueOnce(started(2));
    act(() => {
      result.current.toggleRecord();
    });
    expect(fake.bridge.cancelSession).toHaveBeenCalledWith(1);
    expect(result.current.state).toBe('starting');
    await flush();
    expect(result.current.state).toBe('recording');
    expect(result.current.history).toHaveLength(2);
    expect(result.current.history[0]?.answer).toBe('part');
  });
});

describe('timer and the recording cap', () => {
  it('elapsedMs tracks wall-clock time between fires', async () => {
    vi.useFakeTimers();
    const perf = mockPerfNow();
    const { result } = renderHook(() => useSession());
    await startRecording(result, 8);
    act(() => {
      perf.advance(1000);
      vi.advanceTimersByTime(250);
    });
    expect(result.current.elapsedMs).toBe(1000);
  });

  it('caps locally at 120 s: NO stop issued, the core auto-stop answer lands intact', async () => {
    vi.useFakeTimers();
    const perf = mockPerfNow();
    const { result } = renderHook(() => useSession());
    await startRecording(result, 8);

    await act(async () => {
      perf.advance(MAX_RECORDING_SECONDS * 1000);
      vi.advanceTimersByTime(250);
    });
    expect(result.current.state).toBe('finalizing');
    expect(result.current.hitRecordingCap).toBe(true);
    expect(result.current.elapsedMs).toBe(MAX_RECORDING_SECONDS * 1000);
    // The core's cap timer is armed before ours and it emits no event when
    // it auto-stops (§3), so a stop_session here would be refused "not
    // taken" — and the teardown for that refusal is the double-stop race
    // that dropped every capped recording's answer and showed a spurious
    // internal error.
    expect(fake.bridge.stopSession).not.toHaveBeenCalled();

    act(() => {
      fake.emit('llm:delta', { sessionId: 8, delta: 'Auto ' });
    });
    expect(result.current.state).toBe('answering');
    act(() => {
      fake.emit('llm:done', { sessionId: 8, transcript: 'long q', answer: 'Auto answer.', metrics, stopReason: 'complete' });
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toBeNull();
    expect(result.current.viewed).toMatchObject({
      question: 'long q',
      answer: 'Auto answer.',
      metrics,
    });
    expect(fake.bridge.stopSession).not.toHaveBeenCalled();

    // The flag stays up through the answer so the UI can explain the
    // auto-stop (§9 "Reached the 120s limit"), then clears on the next start.
    expect(result.current.hitRecordingCap).toBe(true);
    await startRecording(result, 9);
    expect(result.current.hitRecordingCap).toBe(false);
    expect(result.current.elapsedMs).toBe(0);
  });

  it('caps at true wall-clock 120 s even when timer fires are throttled and lumpy', async () => {
    vi.useFakeTimers();
    const perf = mockPerfNow();
    const { result } = renderHook(() => useSession());
    await startRecording(result, 8);

    // WebView2 background throttling: fires arrive rarely and late. Counted
    // as fixed 250 ms deltas these three fires would read as 0.75 s; keyed
    // off performance.now they are two minutes and change.
    for (const lumpMs of [45_000, 45_000, 31_000]) {
      await act(async () => {
        perf.advance(lumpMs);
        vi.advanceTimersByTime(250); // a single (late) fire per lump
      });
    }
    expect(result.current.elapsedMs).toBe(MAX_RECORDING_SECONDS * 1000);
    expect(result.current.hitRecordingCap).toBe(true);
    expect(result.current.state).toBe('finalizing');
    expect(fake.bridge.stopSession).not.toHaveBeenCalled();
  });

  it('adopts an answer delta arriving while still "recording" (core capped first)', async () => {
    // Belt and braces for the same race: even before any tick crosses the
    // cap, answer events for the current session mean the recording ended.
    const { result } = renderHook(() => useSession());
    await startRecording(result, 8);
    act(() => {
      fake.emit('llm:delta', { sessionId: 8, delta: 'Answer' });
    });
    expect(result.current.state).toBe('answering');
    expect(result.current.viewed?.answer).toBe('Answer');
    act(() => {
      fake.emit('llm:done', { sessionId: 8, transcript: 'q', answer: 'Answer.', metrics, stopReason: 'complete' });
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toBeNull();
    expect(fake.bridge.stopSession).not.toHaveBeenCalled();
  });
});

describe('history', () => {
  async function completeAsk(result: { current: ReturnType<typeof useSession> }, i: number) {
    fake.bridge.ask.mockResolvedValueOnce(started(100 + i));
    await act(async () => {
      await result.current.submitAsk(`q${i}`);
    });
    act(() => {
      fake.emit('llm:done', { sessionId: 100 + i, transcript: `q${i}`, answer: `a${i}`, metrics, stopReason: 'complete' });
    });
  }

  it('keeps the last 6, trims the oldest, and the in-flight entry survives the trim', async () => {
    const { result } = renderHook(() => useSession());
    for (let i = 1; i <= 6; i += 1) await completeAsk(result, i);
    expect(result.current.history).toHaveLength(6);

    // The 7th attempt is live while the trim happens.
    await startRecording(result, 50);
    expect(result.current.history).toHaveLength(6);
    expect(result.current.history.map((e) => e.question)).toEqual(['q2', 'q3', 'q4', 'q5', 'q6', '']);
    expect(result.current.history[5]?.sessionId).toBe(50);
    expect(result.current.viewIndex).toBe(5);
    expect(result.current.viewed?.sessionId).toBe(50);
  });

  it('regenerate re-asks the viewed question as a new entry', async () => {
    const { result } = renderHook(() => useSession());
    await completeAsk(result, 1);
    fake.bridge.ask.mockResolvedValueOnce(started(200));
    await act(async () => {
      result.current.regenerate();
    });
    expect(fake.bridge.ask).toHaveBeenLastCalledWith('q1');
    expect(result.current.history).toHaveLength(2);
    expect(result.current.history[1]).toMatchObject({ question: 'q1', answer: '', sessionId: 200 });
    expect(result.current.viewIndex).toBe(1);
  });

  it('regenerate is a no-op when the viewed entry has no question', async () => {
    const { result } = renderHook(() => useSession());
    act(() => {
      result.current.regenerate();
    });
    expect(fake.bridge.ask).not.toHaveBeenCalled();
  });

  it('clearHistory wipes when idle and is refused otherwise', async () => {
    const { result } = renderHook(() => useSession());
    await completeAsk(result, 1);

    fake.bridge.ask.mockResolvedValueOnce(started(300));
    await act(async () => {
      await result.current.submitAsk('live one');
    });
    act(() => {
      result.current.clearHistory();
    });
    expect(result.current.history).toHaveLength(2); // refused while answering

    act(() => {
      fake.emit('llm:done', { sessionId: 300, transcript: 'live one', answer: 'a', metrics, stopReason: 'complete' });
    });
    act(() => {
      result.current.clearHistory();
    });
    expect(result.current.history).toHaveLength(0);
    expect(result.current.viewed).toBeNull();
  });

  it('viewPrev/viewNext clamp at the ends', async () => {
    const { result } = renderHook(() => useSession());
    for (let i = 1; i <= 3; i += 1) await completeAsk(result, i);
    expect(result.current.viewIndex).toBe(2);
    act(() => {
      result.current.viewNext();
    });
    expect(result.current.viewIndex).toBe(2);
    act(() => {
      result.current.viewPrev();
      result.current.viewPrev();
      result.current.viewPrev();
      result.current.viewPrev();
    });
    expect(result.current.viewIndex).toBe(0);
    expect(result.current.viewed?.question).toBe('q1');
  });
});

describe('error during streaming', () => {
  it('keeps the partial answer, surfaces the error, returns to idle', async () => {
    const { result } = renderHook(() => useSession());
    fake.bridge.ask.mockResolvedValueOnce(started(31));
    await act(async () => {
      await result.current.submitAsk('question');
    });
    act(() => {
      fake.emit('llm:delta', { sessionId: 31, delta: 'half an ' });
      fake.emit('session:error', { sessionId: 31, error: { code: 'llm_timeout', message: 'stalled' } });
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toEqual({ code: 'llm_timeout', message: 'stalled' });
    expect(result.current.viewed).toMatchObject({ question: 'question', answer: 'half an ' });
  });

  it('setError clears a surfaced error', async () => {
    const { result } = renderHook(() => useSession());
    act(() => {
      result.current.setError({ code: 'internal', message: 'x' });
    });
    expect(result.current.error).toEqual({ code: 'internal', message: 'x' });
    act(() => {
      result.current.setError(null);
    });
    expect(result.current.error).toBeNull();
  });
});

describe('teardown', () => {
  it('unmount unsubscribes every bridge listener', () => {
    const { unmount } = renderHook(() => useSession());
    // Five session events + the hotkey: `sessionOutcome` is a command, not a
    // subscription, so R1 added no listener here.
    const subscribed = [...fake.handlers.values()].reduce((n, set) => n + set.size, 0);
    expect(subscribed).toBe(6);
    unmount();
    const remaining = [...fake.handlers.values()].reduce((n, set) => n + set.size, 0);
    expect(remaining).toBe(0);
  });
});

describe('delta coalescing (FE-1)', () => {
  // A fast provider lands several tokens a millisecond, and each one used to
  // be a reducer pass plus a render of the whole main view. The first delta
  // of a burst paints at once; the rest of a 16 ms window is merged into one
  // dispatch, flushed by a timer — or earlier, by the next non-delta event,
  // so ORDER is never violated.
  async function startAsk(result: { current: ReturnType<typeof useSession> }, id: SessionId) {
    fake.bridge.ask.mockResolvedValueOnce(started(id));
    await act(async () => {
      await result.current.submitAsk('q');
    });
    expect(result.current.state).toBe('answering');
  }

  it('the first delta of a burst paints immediately', async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useSession());
    await startAsk(result, 1);
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'Hello' });
    });
    expect(result.current.viewed?.answer).toBe('Hello');
  });

  it('later deltas inside the window are merged and flushed by the timer, not rendered per token', async () => {
    vi.useFakeTimers();
    let renders = 0;
    const { result } = renderHook(() => {
      renders += 1;
      return useSession();
    });
    await startAsk(result, 1);
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'a' });
    });
    const after = renders;
    // Each in its own act: without coalescing every one would be a render.
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'b' });
    });
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'c' });
    });
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'd' });
    });
    expect(renders).toBe(after);
    expect(result.current.viewed?.answer).toBe('a');
    act(() => {
      vi.advanceTimersByTime(16);
    });
    expect(result.current.viewed?.answer).toBe('abcd');
    expect(renders).toBe(after + 1);
  });

  it('a steady stream renders once per window; a quiet window closes it', async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useSession());
    await startAsk(result, 1);
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: '1' }); // t=0, leading
    });
    act(() => {
      vi.advanceTimersByTime(8);
      fake.emit('llm:delta', { sessionId: 1, delta: '2' }); // t=8, buffered
    });
    act(() => {
      vi.advanceTimersByTime(8); // t=16: flushed, and the window rolls on
    });
    expect(result.current.viewed?.answer).toBe('12');
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: '3' }); // t=16, still inside a window
    });
    expect(result.current.viewed?.answer).toBe('12');
    act(() => {
      vi.advanceTimersByTime(16); // t=32
    });
    expect(result.current.viewed?.answer).toBe('123');
    act(() => {
      vi.advanceTimersByTime(16); // t=48: nothing pending, window closed
    });
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: '4' }); // paints at once again
    });
    expect(result.current.viewed?.answer).toBe('1234');
  });

  it('a session:error after a buffered delta keeps the whole partial answer', async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useSession());
    await startAsk(result, 1);
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'half an ' });
      fake.emit('llm:delta', { sessionId: 1, delta: 'answer' });
      fake.emit('session:error', { sessionId: 1, error: { code: 'llm_timeout', message: 'stalled' } });
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toEqual({ code: 'llm_timeout', message: 'stalled' });
    // Flushed BEFORE the teardown; a delta arriving after it would be
    // dropped as stale and the reader would lose the tail.
    expect(result.current.viewed?.answer).toBe('half an answer');
  });

  it('llm:done after a buffered delta yields the full answer, and nothing fires late', async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useSession());
    await startAsk(result, 1);
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'Use ' });
      fake.emit('llm:delta', { sessionId: 1, delta: 'refs.' });
      fake.emit('llm:done', { sessionId: 1, transcript: 'q', answer: 'Use refs.', metrics, stopReason: 'complete' });
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.viewed).toMatchObject({ question: 'q', answer: 'Use refs.', metrics });
    act(() => {
      vi.advanceTimersByTime(50);
    });
    expect(result.current.viewed?.answer).toBe('Use refs.');
    expect(result.current.state).toBe('idle');
  });

  it('a hotkey start after a buffered delta retires the old entry with its last token', async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useSession());
    await startAsk(result, 1);
    fake.bridge.startSession.mockResolvedValueOnce(started(2));
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'partial ' });
      fake.emit('llm:delta', { sessionId: 1, delta: 'text' });
      fake.emit('hotkey:toggle', null);
    });
    expect(result.current.state).toBe('starting');
    expect(fake.bridge.cancelSession).toHaveBeenCalledWith(1);
    expect(result.current.history[0]?.answer).toBe('partial text');
    // Let the start resolve inside act so the adoption is not an orphan update.
    await flush();
    expect(result.current.state).toBe('recording');
    expect(result.current.history[1]?.sessionId).toBe(2);
  });

  it('a Record click after a buffered delta retires the old entry with its last token', async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useSession());
    await startAsk(result, 1);
    fake.bridge.startSession.mockResolvedValueOnce(started(2));
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'partial ' });
      fake.emit('llm:delta', { sessionId: 1, delta: 'text' });
      // The button path, not the hotkey event: it has to drain the coalescer
      // itself before record/start settles the streaming entry.
      result.current.toggleRecord();
    });
    expect(result.current.state).toBe('starting');
    expect(fake.bridge.cancelSession).toHaveBeenCalledWith(1);
    expect(result.current.history[0]?.answer).toBe('partial text');
    await flush();
    expect(result.current.state).toBe('recording');
    expect(result.current.history[1]?.sessionId).toBe(2);
    // The window's timer was cleared with the flush: nothing lands late.
    act(() => {
      vi.advanceTimersByTime(50);
    });
    expect(result.current.history[0]?.answer).toBe('partial text');
  });

  it('a typed ask after a buffered delta retires the old entry with its last token', async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useSession());
    await startAsk(result, 1);
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'partial ' });
      fake.emit('llm:delta', { sessionId: 1, delta: 'text' });
    });
    // Precondition: the second token is still sitting in the window.
    expect(result.current.viewed?.answer).toBe('partial ');
    fake.bridge.ask.mockResolvedValueOnce(started(2));
    await act(async () => {
      await result.current.submitAsk('second');
    });
    expect(fake.bridge.cancelSession).toHaveBeenCalledWith(1);
    expect(result.current.history.map((e) => e.question)).toEqual(['q', 'second']);
    expect(result.current.history[0]?.answer).toBe('partial text');
    expect(result.current.history[1]?.sessionId).toBe(2);
    act(() => {
      vi.advanceTimersByTime(50);
    });
    expect(result.current.history[0]?.answer).toBe('partial text');
    expect(result.current.history[1]?.answer).toBe('');
  });

  it("never merges a stale session's token into the live answer", async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useSession());
    await startAsk(result, 2);
    act(() => {
      fake.emit('llm:delta', { sessionId: 2, delta: 'live ' }); // leading
      fake.emit('llm:delta', { sessionId: 1, delta: 'ZOMBIE' }); // buffered, then dropped by the reducer
      fake.emit('llm:delta', { sessionId: 2, delta: 'answer' }); // must not concatenate onto the zombie
    });
    act(() => {
      vi.advanceTimersByTime(16);
    });
    expect(result.current.viewed?.answer).toBe('live answer');
  });

  it('unmount leaves no coalescing timer behind', async () => {
    vi.useFakeTimers();
    const { result, unmount } = renderHook(() => useSession());
    await startAsk(result, 1);
    const baseline = vi.getTimerCount();
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'a' });
      fake.emit('llm:delta', { sessionId: 1, delta: 'b' });
    });
    expect(vi.getTimerCount()).toBe(baseline + 1);
    unmount();
    expect(vi.getTimerCount()).toBe(baseline);
  });
});

describe('adoption and reconciliation across the command/event boundary (R1)', () => {
  const done = (sessionId: SessionId, answer: string) => ({
    sessionId,
    transcript: 'q',
    answer,
    metrics,
    stopReason: 'complete' as const,
  });

  it('early deltas before adoption are shown exactly once, then live deltas append', async () => {
    const { result } = renderHook(() => useSession());
    const ask = deferred<Envelope<SessionStart>>();
    fake.bridge.ask.mockReturnValueOnce(ask.promise);
    let accepted: Promise<boolean> = Promise.resolve(false);
    await act(async () => {
      accepted = result.current.submitAsk('q');
    });
    act(() => {
      fake.emit('llm:delta', { sessionId: 3, delta: 'Early ' });
      fake.emit('llm:delta', { sessionId: 3, delta: 'text' });
    });
    expect(result.current.viewed?.answer).toBe('');
    await act(async () => {
      ask.resolve(started(3));
      await accepted;
    });
    expect(result.current.viewed?.answer).toBe('Early text');
    act(() => {
      fake.emit('llm:delta', { sessionId: 3, delta: ' and more' });
      fake.emit('llm:done', done(3, 'Early text and more'));
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.viewed).toMatchObject({ answer: 'Early text and more', status: 'completed' });
  });

  it('an immediate failure in the start envelope settles the attempt with its error and text', async () => {
    // The local oversize rejection fires microseconds after the ask task
    // spawns; the envelope now carries it instead of a bare id.
    const { result } = renderHook(() => useSession());
    const oversize = { code: 'llm_http' as const, message: 'Free local mode supports about 7 KB…' };
    fake.bridge.ask.mockResolvedValueOnce(
      started(9, { status: 'failed', error: oversize, transcript: 'q', partial: '' }),
    );
    await act(async () => {
      await result.current.submitAsk('q');
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toEqual(oversize);
    expect(result.current.viewed).toMatchObject({ question: 'q', status: 'incomplete', reason: oversize.message });
    // A settled envelope needs no follow-up lookup.
    expect(fake.bridge.sessionOutcome).not.toHaveBeenCalled();
  });

  it('an immediate success in the start envelope completes the answer, never an error', async () => {
    const { result } = renderHook(() => useSession());
    fake.bridge.ask.mockResolvedValueOnce(
      started(4, { status: 'completed', transcript: 'q', answer: 'Instant.', metrics, stopReason: 'complete' }),
    );
    await act(async () => {
      await result.current.submitAsk('q');
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toBeNull();
    expect(result.current.viewed).toMatchObject({ answer: 'Instant.', metrics, status: 'completed' });
  });

  it('a delayed command response: the terminal event that raced ahead is replayed, not lost', async () => {
    // The exact R1 reproduction: start -> error for id 1 -> invoke resolves.
    const { result } = renderHook(() => useSession());
    const start = deferred<Envelope<SessionStart>>();
    fake.bridge.startSession.mockReturnValueOnce(start.promise);
    await act(async () => {
      result.current.toggleRecord();
    });
    act(() => {
      fake.emit('session:error', { sessionId: 1, error: { code: 'stt_connect', message: 'no network' } });
    });
    expect(result.current.state).toBe('starting');
    await act(async () => {
      start.resolve(started(1));
    });
    await flush();
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toEqual({ code: 'stt_connect', message: 'no network' });
  });

  it('the adoption lookup settles a session whose terminal event never reached the UI', async () => {
    const { result } = renderHook(() => useSession());
    fake.bridge.sessionOutcome.mockResolvedValueOnce(
      ok<SessionOutcome>({ status: 'completed', transcript: 'q', answer: 'Recovered.', metrics, stopReason: 'token_limit' }),
    );
    fake.bridge.ask.mockResolvedValueOnce(started(12));
    await act(async () => {
      await result.current.submitAsk('q');
    });
    await flush();
    expect(fake.bridge.sessionOutcome).toHaveBeenCalledWith(12);
    expect(result.current.state).toBe('idle');
    expect(result.current.viewed).toMatchObject({ answer: 'Recovered.', status: 'limited' });
  });

  it('duplicate terminal delivery (event, repeat, and lookup) settles exactly once', async () => {
    const { result } = renderHook(() => useSession());
    const lookup = deferred<Envelope<SessionOutcome>>();
    fake.bridge.sessionOutcome.mockReturnValueOnce(lookup.promise);
    fake.bridge.ask.mockResolvedValueOnce(started(6));
    await act(async () => {
      await result.current.submitAsk('q');
    });
    act(() => {
      fake.emit('llm:done', done(6, 'Once.'));
      fake.emit('llm:done', done(6, 'Once.'));
    });
    await act(async () => {
      lookup.resolve(ok<SessionOutcome>({ status: 'failed', error: { code: 'internal', message: 'late' }, transcript: 'q', partial: '' }));
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toBeNull();
    expect(result.current.history).toHaveLength(1);
    expect(result.current.viewed).toMatchObject({ answer: 'Once.', status: 'completed' });
  });

  it('an older lookup never settles the newer attempt', async () => {
    const { result } = renderHook(() => useSession());
    const oldLookup = deferred<Envelope<SessionOutcome>>();
    fake.bridge.sessionOutcome.mockReturnValueOnce(oldLookup.promise);
    fake.bridge.ask.mockResolvedValueOnce(started(1));
    await act(async () => {
      await result.current.submitAsk('first');
    });
    fake.bridge.ask.mockResolvedValueOnce(started(2));
    await act(async () => {
      await result.current.submitAsk('second');
    });
    await act(async () => {
      oldLookup.resolve(ok<SessionOutcome>({ status: 'failed', error: { code: 'internal', message: 'old' }, transcript: 'first', partial: '' }));
    });
    expect(result.current.state).toBe('answering');
    expect(result.current.error).toBeNull();
    expect(result.current.viewed).toMatchObject({ question: 'second', status: 'pending' });
  });

  it('cancellation before adoption: held events are discarded and the orphan is cancelled once', async () => {
    const { result } = renderHook(() => useSession());
    const start = deferred<Envelope<SessionStart>>();
    fake.bridge.startSession.mockReturnValueOnce(start.promise);
    await act(async () => {
      result.current.toggleRecord();
    });
    act(() => {
      fake.emit('stt:partial', { sessionId: 8, text: 'said before abort', isFinal: false });
      result.current.toggleRecord(); // abort while starting
    });
    expect(result.current.state).toBe('idle');
    await act(async () => {
      start.resolve(started(8));
    });
    expect(fake.bridge.cancelSession).toHaveBeenCalledTimes(1);
    expect(fake.bridge.cancelSession).toHaveBeenCalledWith(8);
    expect(result.current.history).toHaveLength(0);
    expect(fake.bridge.sessionOutcome).not.toHaveBeenCalled();
  });

  it("rapid supersession: the first attempt is cancelled and its early text never reaches the second", async () => {
    const { result } = renderHook(() => useSession());
    const first = deferred<Envelope<SessionStart>>();
    fake.bridge.ask.mockReturnValueOnce(first.promise);
    await act(async () => {
      void result.current.submitAsk('first');
    });
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'stale ' });
    });
    fake.bridge.ask.mockResolvedValueOnce(started(2));
    await act(async () => {
      await result.current.submitAsk('second');
    });
    await act(async () => {
      first.resolve(started(1));
    });
    expect(fake.bridge.cancelSession).toHaveBeenCalledTimes(1);
    expect(fake.bridge.cancelSession).toHaveBeenCalledWith(1);
    act(() => {
      fake.emit('llm:delta', { sessionId: 2, delta: 'fresh' });
      fake.emit('llm:done', { ...done(2, 'fresh'), transcript: 'second' });
    });
    expect(result.current.viewed).toMatchObject({ question: 'second', answer: 'fresh', status: 'completed' });
  });

  it('a delayed listener registration holds the start until the subscriptions are live', async () => {
    const ready = deferred<Envelope<null>>();
    fake.bridge.listenReady = () => ready.promise;
    const { result } = renderHook(() => useSession());
    await act(async () => {
      result.current.toggleRecord();
    });
    expect(result.current.state).toBe('starting');
    expect(fake.bridge.startSession).not.toHaveBeenCalled();
    await act(async () => {
      ready.resolve(ok<null>(null));
    });
    await flush();
    expect(fake.bridge.startSession).toHaveBeenCalledTimes(1);
    expect(result.current.state).toBe('recording');
  });

  it('a failed listener registration is a visible start error, never a silent hang, and is retried', async () => {
    const registrationError = { code: 'internal' as const, message: 'The app could not listen for llm:done events (x). Restart the app.' };
    let failing = true;
    fake.bridge.listenReady = (name) =>
      Promise.resolve(failing && name === 'llm:done' ? fail('internal', registrationError.message) : ok<null>(null));
    const { result } = renderHook(() => useSession());
    await flush();
    await act(async () => {
      await result.current.submitAsk('q');
    });
    expect(fake.bridge.ask).not.toHaveBeenCalled();
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toEqual(registrationError);
    // The next attempt re-registers; once that succeeds the ask goes out.
    failing = false;
    await act(async () => {
      await result.current.submitAsk('q');
    });
    expect(fake.bridge.ask).toHaveBeenCalledTimes(1);
    expect(result.current.state).toBe('answering');
  });

  it('a failed hotkey registration is shown but does not block Record', async () => {
    fake.bridge.listenReady = (name) =>
      Promise.resolve(name === 'hotkey:toggle' ? fail('internal', 'no shortcut') : ok<null>(null));
    const { result } = renderHook(() => useSession());
    await flush();
    expect(result.current.error).toEqual({ code: 'internal', message: 'no shortcut' });
    await startRecording(result, 3);
    expect(result.current.state).toBe('recording');
  });

  it('a listener registration that never settles fails the start after the timeout, and a later attempt retries', async () => {
    // Review F2: a `listen()` that hangs used to leave Record in "starting"
    // forever with no message, and every retry awaited the same dead promise.
    vi.useFakeTimers();
    let hang = true;
    fake.bridge.listenReady = () => (hang ? new Promise<Envelope<null>>(() => undefined) : Promise.resolve(ok<null>(null)));
    const { result } = renderHook(() => useSession());
    await act(async () => {
      result.current.toggleRecord();
    });
    expect(result.current.state).toBe('starting');
    await act(async () => {
      vi.advanceTimersByTime(LISTEN_TIMEOUT_MS);
    });
    await flush();
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toEqual(LISTEN_TIMEOUT_ERROR);
    expect(fake.bridge.startSession).not.toHaveBeenCalled();

    hang = false;
    fake.bridge.startSession.mockResolvedValueOnce(started(4));
    await act(async () => {
      result.current.toggleRecord();
    });
    await flush();
    expect(fake.bridge.startSession).toHaveBeenCalledTimes(1);
    expect(result.current.state).toBe('recording');
  });

  it("a held-buffer overflow evicts other sessions' events first, keeping the adopted id's early text", async () => {
    // Review F4: the cap used to evict the OLDEST event, which could be the
    // adopted session's first delta, leaving a fragment with its start
    // missing. Ids only grow, so the newest id is protected.
    const { result } = renderHook(() => useSession());
    const ask = deferred<Envelope<SessionStart>>();
    fake.bridge.ask.mockReturnValueOnce(ask.promise);
    let accepted: Promise<boolean> = Promise.resolve(false);
    await act(async () => {
      accepted = result.current.submitAsk('q');
    });
    act(() => {
      fake.emit('llm:delta', { sessionId: 99, delta: 'Early' });
      // 70 non-mergeable events of two older ids push the buffer past 64.
      for (let i = 0; i < 70; i += 1) {
        fake.emit('stt:partial', { sessionId: 1 + (i % 2), text: `stale ${i}`, isFinal: false });
      }
    });
    await act(async () => {
      ask.resolve(started(99));
      await accepted;
    });
    expect(result.current.viewed?.answer).toBe('Early');
  });
});

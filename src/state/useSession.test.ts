/**
 * Hook tests: the reducer rules are proven in reducer.test.ts; these prove
 * the WIRING — bridge calls issued at the right moments, resolutions and
 * events landing (or being dropped) correctly, and the fake-timer-driven
 * recording cap.
 */
import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { setBridge, type Bridge } from '../bridge';
import {
  MAX_RECORDING_SECONDS,
  type AppError,
  type Envelope,
  type EventMap,
  type EventName,
  type Metrics,
  type SessionId,
  type SettingsView,
} from '../types';
import { useSession } from './useSession';

const ok = <T,>(value: T): Envelope<T> => ({ ok: true, value });
// Returns the error branch only so it is assignable to any Envelope<T>.
const fail = (code: AppError['code'], message = 'boom'): { ok: false; error: AppError } => ({
  ok: false,
  error: { code, message },
});
const metrics: Metrics = { sttFinalizeMs: 480, firstTokenMs: 950, totalMs: 3210 };

const settings: SettingsView = {
  resume: '',
  jobDescription: '',
  alwaysOnTop: true,
  llmProvider: 'anthropic',
  answerStyle: 'balanced',
  hotkey: 'Ctrl+Shift+Space',
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
    startSession: vi.fn(() => Promise.resolve(ok<SessionId>(1))),
    stopSession: vi.fn((_id: SessionId) => Promise.resolve(ok<null>(null))),
    ask: vi.fn((_text: string) => Promise.resolve(ok<SessionId>(1))),
    cancelSession: vi.fn((_id: SessionId) => undefined),
    hotkeyStatus: vi.fn(() => Promise.resolve(ok({ accelerator: 'Ctrl+Shift+Space', registered: true }))),
    on: <K extends EventName>(name: K, handler: (payload: EventMap[K]) => void): (() => void) => {
      let set = handlers.get(name);
      if (set === undefined) {
        set = new Set();
        handlers.set(name, set);
      }
      const h = handler as (payload: never) => void;
      set.add(h);
      return () => {
        handlers.get(name)?.delete(h);
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
  fake.bridge.startSession.mockResolvedValueOnce(ok(id));
  act(() => {
    result.current.toggleRecord();
  });
  await flush();
  expect(result.current.state).toBe('recording');
}

describe('recording round trip', () => {
  it('idle -> starting -> recording -> finalizing -> answering -> idle', async () => {
    const { result } = renderHook(() => useSession());
    fake.bridge.startSession.mockResolvedValueOnce(ok<SessionId>(11));

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
      fake.emit('llm:done', { sessionId: 11, transcript: 'how do I test hooks', answer: 'Use refs.', metrics });
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

describe('stale events', () => {
  it('drops events arriving before the start call resolved (no buffering)', async () => {
    const { result } = renderHook(() => useSession());
    const start = deferred<Envelope<SessionId>>();
    fake.bridge.startSession.mockReturnValueOnce(start.promise);

    act(() => {
      result.current.toggleRecord();
    });
    act(() => {
      // The core could plausibly emit before the command promise settles;
      // adopting these would let a superseded session write into a new one.
      fake.emit('audio:level', { sessionId: 5, rms: 0.9 });
      fake.emit('stt:partial', { sessionId: 5, text: 'ghost', isFinal: false });
    });
    expect(result.current.rms).toBe(0);
    expect(result.current.viewed?.question).toBe('');

    start.resolve(ok(5));
    await flush();
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
    const start = deferred<Envelope<SessionId>>();
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

    start.resolve(ok(7));
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
    const start = deferred<Envelope<SessionId>>();
    fake.bridge.startSession.mockReturnValueOnce(start.promise);
    act(() => {
      result.current.toggleRecord();
    });
    await expect(result.current.submitAsk('question')).resolves.toBe(false); // starting

    start.resolve(ok(6));
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
    fake.bridge.ask.mockResolvedValueOnce(ok<SessionId>(21));
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
    fake.bridge.ask.mockResolvedValueOnce(ok<SessionId>(1));
    await act(async () => {
      await result.current.submitAsk('first');
    });
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'partial ' });
    });
    expect(result.current.state).toBe('answering');

    fake.bridge.ask.mockResolvedValueOnce(ok<SessionId>(2));
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
      fake.emit('llm:done', { sessionId: 2, transcript: 'second', answer: 'fresh answer', metrics });
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.history[0]?.answer).toBe('partial ');
    expect(result.current.history[1]?.answer).toBe('fresh answer');
  });

  it('toggleRecord during a streaming answer also supersedes it', async () => {
    const { result } = renderHook(() => useSession());
    fake.bridge.ask.mockResolvedValueOnce(ok<SessionId>(1));
    await act(async () => {
      await result.current.submitAsk('first');
    });
    act(() => {
      fake.emit('llm:delta', { sessionId: 1, delta: 'part' });
    });
    fake.bridge.startSession.mockResolvedValueOnce(ok<SessionId>(2));
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
      fake.emit('llm:done', { sessionId: 8, transcript: 'long q', answer: 'Auto answer.', metrics });
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
      fake.emit('llm:done', { sessionId: 8, transcript: 'q', answer: 'Answer.', metrics });
    });
    expect(result.current.state).toBe('idle');
    expect(result.current.error).toBeNull();
    expect(fake.bridge.stopSession).not.toHaveBeenCalled();
  });
});

describe('history', () => {
  async function completeAsk(result: { current: ReturnType<typeof useSession> }, i: number) {
    fake.bridge.ask.mockResolvedValueOnce(ok<SessionId>(100 + i));
    await act(async () => {
      await result.current.submitAsk(`q${i}`);
    });
    act(() => {
      fake.emit('llm:done', { sessionId: 100 + i, transcript: `q${i}`, answer: `a${i}`, metrics });
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
    fake.bridge.ask.mockResolvedValueOnce(ok<SessionId>(200));
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

    fake.bridge.ask.mockResolvedValueOnce(ok<SessionId>(300));
    await act(async () => {
      await result.current.submitAsk('live one');
    });
    act(() => {
      result.current.clearHistory();
    });
    expect(result.current.history).toHaveLength(2); // refused while answering

    act(() => {
      fake.emit('llm:done', { sessionId: 300, transcript: 'live one', answer: 'a', metrics });
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
    fake.bridge.ask.mockResolvedValueOnce(ok<SessionId>(31));
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
    const subscribed = [...fake.handlers.values()].reduce((n, set) => n + set.size, 0);
    expect(subscribed).toBe(6);
    unmount();
    const remaining = [...fake.handlers.values()].reduce((n, set) => n + set.size, 0);
    expect(remaining).toBe(0);
  });
});

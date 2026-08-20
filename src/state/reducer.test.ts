/**
 * Exhaustive pure tests for the §9 hard rules. Dropped inputs are asserted by
 * object identity (toBe) — the reducer contract is that an ignored action
 * returns the SAME state, so any accidental clone shows up here.
 */
import { describe, expect, it } from 'vitest';
import { HISTORY_LIMIT, MAX_RECORDING_SECONDS, type AppError, type Metrics } from '../types';
import {
  initialState,
  sessionReducer,
  type HistoryEntry,
  type SessionAction,
  type SessionState,
} from './reducer';

const CAP_MS = MAX_RECORDING_SECONDS * 1000;

const err = (code: AppError['code'], message = 'boom'): AppError => ({ code, message });
const metrics: Metrics = { sttFinalizeMs: 480, firstTokenMs: 950, totalMs: 3210 };

function entry(over: Partial<HistoryEntry> = {}): HistoryEntry {
  return { key: 'k', sessionId: null, question: '', answer: '', metrics: null, ...over };
}

function st(over: Partial<SessionState> = {}): SessionState {
  return { ...initialState, ...over };
}

function reduce(state: SessionState, ...actions: SessionAction[]): SessionState {
  return actions.reduce(sessionReducer, state);
}

/** A state mid-recording with an adopted session id. */
function recording(over: Partial<SessionState> = {}): SessionState {
  return st({
    ui: 'recording',
    activeId: 7,
    liveKey: 'live',
    history: [entry({ key: 'live', sessionId: 7 })],
    viewIndex: 0,
    ...over,
  });
}

describe('record/start', () => {
  it('idle -> starting pushes a live entry, jumps the view, resets per-attempt fields', () => {
    const prev = st({
      error: err('llm_http'),
      rms: 0.5,
      elapsedMs: 4000,
      hitRecordingCap: true,
      history: [entry({ key: 'old', question: 'q', answer: 'a', metrics })],
      viewIndex: 0,
    });
    const next = sessionReducer(prev, { type: 'record/start', key: 'n' });
    expect(next.ui).toBe('starting');
    expect(next.liveKey).toBe('n');
    expect(next.activeId).toBeNull();
    expect(next.error).toBeNull();
    expect(next.rms).toBe(0);
    expect(next.elapsedMs).toBe(0);
    expect(next.hitRecordingCap).toBe(false);
    expect(next.history.map((e) => e.key)).toEqual(['old', 'n']);
    expect(next.viewIndex).toBe(1);
  });

  it('answering -> starting supersedes: a captured streaming entry is retired, not lost', () => {
    const prev = st({
      ui: 'answering',
      activeId: 3,
      liveKey: 'live',
      history: [entry({ key: 'live', sessionId: 3, question: 'q1', answer: 'half an ans' })],
    });
    const next = sessionReducer(prev, { type: 'record/start', key: 'n' });
    expect(next.ui).toBe('starting');
    expect(next.history.map((e) => e.key)).toEqual(['live', 'n']);
    expect(next.history[0]?.answer).toBe('half an ans');
  });

  it('answering -> starting discards a previous live entry that captured nothing', () => {
    const prev = st({
      ui: 'answering',
      activeId: 3,
      liveKey: 'live',
      history: [entry({ key: 'live', sessionId: 3, question: '   ', answer: '' })],
    });
    const next = sessionReducer(prev, { type: 'record/start', key: 'n' });
    expect(next.history.map((e) => e.key)).toEqual(['n']);
    expect(next.viewIndex).toBe(0);
  });

  it('is refused in starting, recording, and finalizing', () => {
    for (const ui of ['starting', 'recording', 'finalizing'] as const) {
      const prev = st({ ui });
      expect(sessionReducer(prev, { type: 'record/start', key: 'n' })).toBe(prev);
    }
  });
});

describe('record/started (adoption)', () => {
  it('starting -> recording adopts the id onto state and the live entry', () => {
    const prev = st({ ui: 'starting', liveKey: 'a', history: [entry({ key: 'a' })] });
    const next = sessionReducer(prev, { type: 'record/started', key: 'a', id: 42 });
    expect(next.ui).toBe('recording');
    expect(next.activeId).toBe(42);
    expect(next.history[0]?.sessionId).toBe(42);
  });

  it('ignores a resolution for a superseded attempt key', () => {
    const prev = st({ ui: 'starting', liveKey: 'b', history: [entry({ key: 'b' })] });
    expect(sessionReducer(prev, { type: 'record/started', key: 'a', id: 42 })).toBe(prev);
  });

  it('ignores adoption when no longer starting (user already aborted)', () => {
    const prev = st({ ui: 'idle', liveKey: null });
    expect(sessionReducer(prev, { type: 'record/started', key: 'a', id: 42 })).toBe(prev);
  });
});

describe('record/startFailed', () => {
  it('discards the empty live entry, returns to idle, surfaces the error', () => {
    const prev = st({ ui: 'starting', liveKey: 'a', history: [entry({ key: 'a' })] });
    const next = sessionReducer(prev, { type: 'record/startFailed', key: 'a', error: err('stt_connect') });
    expect(next.ui).toBe('idle');
    expect(next.history).toEqual([]);
    expect(next.liveKey).toBeNull();
    expect(next.error).toEqual(err('stt_connect'));
  });

  it('never surfaces aborted', () => {
    const prev = st({ ui: 'starting', liveKey: 'a', history: [entry({ key: 'a' })] });
    const next = sessionReducer(prev, { type: 'record/startFailed', key: 'a', error: err('aborted') });
    expect(next.ui).toBe('idle');
    expect(next.error).toBeNull();
  });

  it('ignores a stale key', () => {
    const prev = st({ ui: 'starting', liveKey: 'b', history: [entry({ key: 'b' })] });
    expect(sessionReducer(prev, { type: 'record/startFailed', key: 'a', error: err('internal') })).toBe(prev);
  });
});

describe('record/abortStarting', () => {
  it('silently tears down: idle, no error, empty attempt discarded', () => {
    const prev = st({ ui: 'starting', liveKey: 'a', history: [entry({ key: 'a' })], error: null });
    const next = sessionReducer(prev, { type: 'record/abortStarting' });
    expect(next.ui).toBe('idle');
    expect(next.error).toBeNull();
    expect(next.history).toEqual([]);
  });

  it('is a no-op outside starting', () => {
    const prev = recording();
    expect(sessionReducer(prev, { type: 'record/abortStarting' })).toBe(prev);
  });
});

describe('record/stop and record/stopRejected', () => {
  it('recording -> finalizing', () => {
    const next = sessionReducer(recording({ rms: 0.8 }), { type: 'record/stop' });
    expect(next.ui).toBe('finalizing');
    expect(next.rms).toBe(0);
  });

  it('stop is ignored outside recording', () => {
    for (const ui of ['idle', 'starting', 'finalizing', 'answering'] as const) {
      const prev = st({ ui });
      expect(sessionReducer(prev, { type: 'record/stop' })).toBe(prev);
    }
  });

  it('stopRejected unsticks finalizing: retires a captured transcript and surfaces the error', () => {
    const prev = recording({
      ui: 'finalizing',
      history: [entry({ key: 'live', sessionId: 7, question: 'hello there' })],
    });
    const next = sessionReducer(prev, { type: 'record/stopRejected', key: 'live', error: err('internal') });
    expect(next.ui).toBe('idle');
    expect(next.activeId).toBeNull();
    expect(next.history.map((e) => e.question)).toEqual(['hello there']);
    expect(next.error).toEqual(err('internal'));
  });

  it('stopRejected discards an attempt that captured nothing', () => {
    const prev = recording({ ui: 'finalizing' });
    const next = sessionReducer(prev, { type: 'record/stopRejected', key: 'live', error: err('no_speech') });
    expect(next.ui).toBe('idle');
    expect(next.history).toEqual([]);
    expect(next.error).toEqual(err('no_speech'));
  });

  it('stopRejected with aborted is silent', () => {
    const prev = recording({ ui: 'finalizing' });
    const next = sessionReducer(prev, { type: 'record/stopRejected', key: 'live', error: err('aborted') });
    expect(next.ui).toBe('idle');
    expect(next.error).toBeNull();
  });

  it('stopRejected ignores a stale key or wrong phase', () => {
    const finalizing = recording({ ui: 'finalizing' });
    expect(sessionReducer(finalizing, { type: 'record/stopRejected', key: 'other', error: err('internal') })).toBe(finalizing);
    const rec = recording();
    expect(sessionReducer(rec, { type: 'record/stopRejected', key: 'live', error: err('internal') })).toBe(rec);
  });
});

describe('ask lifecycle', () => {
  it('idle -> answering pushes the question and jumps the view', () => {
    const next = sessionReducer(st(), { type: 'ask/start', key: 'a', question: 'what is x?' });
    expect(next.ui).toBe('answering');
    expect(next.activeId).toBeNull();
    expect(next.liveKey).toBe('a');
    expect(next.history).toEqual([entry({ key: 'a', question: 'what is x?' })]);
    expect(next.viewIndex).toBe(0);
  });

  it('ask over a streaming answer supersedes it and retires the partial', () => {
    const prev = st({
      ui: 'answering',
      activeId: 5,
      liveKey: 'old',
      history: [entry({ key: 'old', sessionId: 5, question: 'q1', answer: 'part' })],
    });
    const next = sessionReducer(prev, { type: 'ask/start', key: 'new', question: 'q2' });
    expect(next.history.map((e) => e.key)).toEqual(['old', 'new']);
    expect(next.liveKey).toBe('new');
    expect(next.activeId).toBeNull();
  });

  it('ask/start is refused mid-recording pipeline', () => {
    for (const ui of ['starting', 'recording', 'finalizing'] as const) {
      const prev = st({ ui });
      expect(sessionReducer(prev, { type: 'ask/start', key: 'a', question: 'q' })).toBe(prev);
    }
  });

  it('ask/accepted adopts the id; stale key ignored', () => {
    const pending = sessionReducer(st(), { type: 'ask/start', key: 'a', question: 'q' });
    const next = sessionReducer(pending, { type: 'ask/accepted', key: 'a', id: 9 });
    expect(next.activeId).toBe(9);
    expect(next.history[0]?.sessionId).toBe(9);
    expect(sessionReducer(pending, { type: 'ask/accepted', key: 'zzz', id: 9 })).toBe(pending);
  });

  it('ask/failed retires the entry (it holds the typed question) and surfaces the error', () => {
    const pending = sessionReducer(st(), { type: 'ask/start', key: 'a', question: 'q' });
    const next = sessionReducer(pending, { type: 'ask/failed', key: 'a', error: err('no_llm_key') });
    expect(next.ui).toBe('idle');
    expect(next.history.map((e) => e.question)).toEqual(['q']);
    expect(next.error).toEqual(err('no_llm_key'));
  });
});

describe('tick and the recording cap', () => {
  it('accumulates elapsed only while recording', () => {
    const next = reduce(recording(), { type: 'tick', deltaMs: 250 }, { type: 'tick', deltaMs: 250 });
    expect(next.elapsedMs).toBe(500);
    expect(next.hitRecordingCap).toBe(false);
    const idle = st();
    expect(sessionReducer(idle, { type: 'tick', deltaMs: 250 })).toBe(idle);
  });

  it('crossing MAX_RECORDING_SECONDS caps locally: finalizing, flag latched, session KEPT', () => {
    const prev = recording({ elapsedMs: CAP_MS - 100, rms: 0.4 });
    const next = sessionReducer(prev, { type: 'tick', deltaMs: 250 });
    expect(next.elapsedMs).toBe(CAP_MS);
    expect(next.hitRecordingCap).toBe(true);
    // The core capped first (its timer is armed before ours — §3) and will
    // stream the answer for this same session; nulling activeId/liveKey here
    // is what used to drop it.
    expect(next.ui).toBe('finalizing');
    expect(next.activeId).toBe(7);
    expect(next.liveKey).toBe('live');
    expect(next.rms).toBe(0);
  });

  it('the cap transition fires exactly once — a follow-up tick is a no-op', () => {
    const capped = sessionReducer(recording({ elapsedMs: CAP_MS - 1 }), { type: 'tick', deltaMs: 250 });
    expect(capped.ui).toBe('finalizing');
    expect(sessionReducer(capped, { type: 'tick', deltaMs: 250 })).toBe(capped);
  });

  it('the flag resets on the next start, not before', () => {
    const capped = sessionReducer(recording({ elapsedMs: CAP_MS - 1 }), { type: 'tick', deltaMs: 250 });
    const done = sessionReducer(capped, {
      type: 'event/llmDone',
      payload: { sessionId: 7, transcript: 't', answer: 'a', metrics },
    });
    expect(done.hitRecordingCap).toBe(true);
    const restarted = sessionReducer(done, { type: 'record/start', key: 'n2' });
    expect(restarted.hitRecordingCap).toBe(false);
  });
});

describe('core auto-stop recovery (answer events while still "recording")', () => {
  // Belt and braces for the §3 cap race: the core auto-stops at 120 s
  // without emitting an event, so its answer can arrive while the frontend
  // clock still says 'recording'. The events themselves are the
  // authoritative "recording ended" signal and must not be dropped.
  it('llm:delta for the current session transitions recording -> answering and appends', () => {
    const next = reduce(
      recording({ rms: 0.5 }),
      { type: 'event/llmDelta', payload: { sessionId: 7, delta: 'Auto ' } },
      { type: 'event/llmDelta', payload: { sessionId: 7, delta: 'answer.' } },
    );
    expect(next.ui).toBe('answering');
    expect(next.rms).toBe(0);
    expect(next.history[0]?.answer).toBe('Auto answer.');
    // hitRecordingCap is purely tick-driven; recovery only prevents data loss.
    expect(next.hitRecordingCap).toBe(false);
  });

  it('llm:done for the current session completes the entry straight from recording', () => {
    const next = sessionReducer(recording(), {
      type: 'event/llmDone',
      payload: { sessionId: 7, transcript: 'capped q', answer: 'full a', metrics },
    });
    expect(next.ui).toBe('idle');
    expect(next.activeId).toBeNull();
    expect(next.liveKey).toBeNull();
    expect(next.history[0]).toEqual(
      entry({ key: 'live', sessionId: 7, question: 'capped q', answer: 'full a', metrics }),
    );
    expect(next.error).toBeNull();
  });

  it('still drops stale ids while recording', () => {
    const prev = recording();
    expect(sessionReducer(prev, { type: 'event/llmDelta', payload: { sessionId: 999, delta: 'x' } })).toBe(prev);
    expect(
      sessionReducer(prev, { type: 'event/llmDone', payload: { sessionId: 999, transcript: 't', answer: 'a', metrics } }),
    ).toBe(prev);
  });

  it('still drops pre-adoption events (starting, or ask not yet accepted)', () => {
    const starting = st({ ui: 'starting', liveKey: 'a', history: [entry({ key: 'a' })] });
    expect(sessionReducer(starting, { type: 'event/llmDelta', payload: { sessionId: 1, delta: 'x' } })).toBe(starting);
    expect(
      sessionReducer(starting, { type: 'event/llmDone', payload: { sessionId: 1, transcript: 't', answer: 'a', metrics } }),
    ).toBe(starting);
    const pendingAsk = sessionReducer(st(), { type: 'ask/start', key: 'a', question: 'q' });
    expect(sessionReducer(pendingAsk, { type: 'event/llmDelta', payload: { sessionId: 1, delta: 'x' } })).toBe(pendingAsk);
    expect(
      sessionReducer(pendingAsk, { type: 'event/llmDone', payload: { sessionId: 1, transcript: 't', answer: 'a', metrics } }),
    ).toBe(pendingAsk);
  });
});

describe('stale events change nothing, ever', () => {
  it('drops events whose sessionId is not the tracked session', () => {
    const prev = recording();
    const wrong = 999;
    expect(sessionReducer(prev, { type: 'event/audioLevel', payload: { sessionId: wrong, rms: 1 } })).toBe(prev);
    expect(sessionReducer(prev, { type: 'event/sttPartial', payload: { sessionId: wrong, text: 'x', isFinal: false } })).toBe(prev);
    expect(sessionReducer(prev, { type: 'event/sessionError', payload: { sessionId: wrong, error: err('internal') } })).toBe(prev);
    const answering = recording({ ui: 'answering' });
    expect(sessionReducer(answering, { type: 'event/llmDelta', payload: { sessionId: wrong, delta: 'x' } })).toBe(answering);
    expect(
      sessionReducer(answering, { type: 'event/llmDone', payload: { sessionId: wrong, transcript: 't', answer: 'a', metrics } }),
    ).toBe(answering);
  });

  it('drops events arriving before the id was adopted (activeId still null)', () => {
    const starting = st({ ui: 'starting', liveKey: 'a', history: [entry({ key: 'a' })] });
    expect(sessionReducer(starting, { type: 'event/audioLevel', payload: { sessionId: 1, rms: 0.9 } })).toBe(starting);
    const pendingAsk = sessionReducer(st(), { type: 'ask/start', key: 'a', question: 'q' });
    expect(sessionReducer(pendingAsk, { type: 'event/llmDelta', payload: { sessionId: 1, delta: 'x' } })).toBe(pendingAsk);
  });
});

describe('event application', () => {
  it('audio:level drives rms while recording', () => {
    const next = sessionReducer(recording(), { type: 'event/audioLevel', payload: { sessionId: 7, rms: 0.63 } });
    expect(next.rms).toBe(0.63);
  });

  it('stt:partial replaces (not appends) the live question in recording and finalizing', () => {
    const a = sessionReducer(recording(), { type: 'event/sttPartial', payload: { sessionId: 7, text: 'how do', isFinal: false } });
    const b = reduce(
      a,
      { type: 'record/stop' },
      { type: 'event/sttPartial', payload: { sessionId: 7, text: 'how do I test hooks', isFinal: true } },
    );
    expect(b.history[0]?.question).toBe('how do I test hooks');
  });

  it('llm:delta moves finalizing -> answering and appends', () => {
    const finalizing = sessionReducer(recording(), { type: 'record/stop' });
    const next = reduce(
      finalizing,
      { type: 'event/llmDelta', payload: { sessionId: 7, delta: 'Use ' } },
      { type: 'event/llmDelta', payload: { sessionId: 7, delta: 'refs.' } },
    );
    expect(next.ui).toBe('answering');
    expect(next.history[0]?.answer).toBe('Use refs.');
  });

  it('llm:done finalizes the entry and returns to idle', () => {
    const answering = reduce(
      recording(),
      { type: 'record/stop' },
      { type: 'event/llmDelta', payload: { sessionId: 7, delta: 'Use ' } },
    );
    const next = sessionReducer(answering, {
      type: 'event/llmDone',
      payload: { sessionId: 7, transcript: 'the question', answer: 'Use refs.', metrics },
    });
    expect(next.ui).toBe('idle');
    expect(next.activeId).toBeNull();
    expect(next.liveKey).toBeNull();
    expect(next.history[0]).toEqual(
      entry({ key: 'live', sessionId: 7, question: 'the question', answer: 'Use refs.', metrics }),
    );
  });

  it('session:error mid-stream keeps the partial answer, surfaces the error, returns to idle', () => {
    const answering = reduce(
      recording(),
      { type: 'record/stop' },
      { type: 'event/sttPartial', payload: { sessionId: 7, text: 'q', isFinal: true } },
      { type: 'event/llmDelta', payload: { sessionId: 7, delta: 'partial ans' } },
    );
    const next = sessionReducer(answering, {
      type: 'event/sessionError',
      payload: { sessionId: 7, error: err('llm_timeout') },
    });
    expect(next.ui).toBe('idle');
    expect(next.error).toEqual(err('llm_timeout'));
    expect(next.history[0]?.answer).toBe('partial ans');
  });

  it('session:error aborted is silent; an empty attempt is discarded', () => {
    const next = sessionReducer(recording(), {
      type: 'event/sessionError',
      payload: { sessionId: 7, error: err('aborted') },
    });
    expect(next.ui).toBe('idle');
    expect(next.error).toBeNull();
    expect(next.history).toEqual([]);
  });

  it('session:error aborted retires an attempt that captured a question', () => {
    const withQ = sessionReducer(recording(), {
      type: 'event/sttPartial',
      payload: { sessionId: 7, text: 'my question', isFinal: false },
    });
    const next = sessionReducer(withQ, {
      type: 'event/sessionError',
      payload: { sessionId: 7, error: err('aborted') },
    });
    expect(next.error).toBeNull();
    expect(next.history.map((e) => e.question)).toEqual(['my question']);
  });
});

describe('history limit', () => {
  const full = () =>
    st({
      history: Array.from({ length: HISTORY_LIMIT }, (_, i) =>
        entry({ key: `c${i + 1}`, question: `q${i + 1}`, answer: `a${i + 1}`, metrics }),
      ),
      viewIndex: HISTORY_LIMIT - 1,
    });

  it('pushing onto a full history trims the oldest and jumps the view to the new entry', () => {
    const next = sessionReducer(full(), { type: 'record/start', key: 'new' });
    expect(next.history).toHaveLength(HISTORY_LIMIT);
    expect(next.history.map((e) => e.key)).toEqual(['c2', 'c3', 'c4', 'c5', 'c6', 'new']);
    expect(next.viewIndex).toBe(HISTORY_LIMIT - 1);
  });

  it('the in-flight entry survives a trim even from a pathological oversized history', () => {
    // Not reachable through normal transitions; guards against a future bug
    // that lets history exceed the limit and then trims the live entry.
    const oversized = st({
      history: Array.from({ length: HISTORY_LIMIT + 2 }, (_, i) =>
        entry({ key: `c${i + 1}`, question: `q${i + 1}` }),
      ),
    });
    const next = sessionReducer(oversized, { type: 'ask/start', key: 'new', question: 'q' });
    expect(next.history).toHaveLength(HISTORY_LIMIT);
    expect(next.history[next.history.length - 1]?.key).toBe('new');
  });

  it('repeated pushes never exceed the limit and always keep the newest live', () => {
    let s = st();
    for (let i = 0; i < 10; i += 1) {
      s = sessionReducer(s, { type: 'ask/start', key: `k${i}`, question: `q${i}` });
      s = sessionReducer(s, { type: 'ask/accepted', key: `k${i}`, id: i });
      s = sessionReducer(s, {
        type: 'event/llmDone',
        payload: { sessionId: i, transcript: `q${i}`, answer: `a${i}`, metrics },
      });
      expect(s.history.length).toBeLessThanOrEqual(HISTORY_LIMIT);
      expect(s.history[s.history.length - 1]?.question).toBe(`q${i}`);
    }
  });
});

describe('view + clear', () => {
  const three = () =>
    st({
      history: [entry({ key: 'a', question: 'qa' }), entry({ key: 'b', question: 'qb' }), entry({ key: 'c', question: 'qc' })],
      viewIndex: 1,
    });

  it('viewPrev/viewNext clamp at the ends', () => {
    let s = three();
    s = reduce(s, { type: 'view/prev' }, { type: 'view/prev' }, { type: 'view/prev' });
    expect(s.viewIndex).toBe(0);
    s = reduce(s, { type: 'view/next' }, { type: 'view/next' }, { type: 'view/next' }, { type: 'view/next' });
    expect(s.viewIndex).toBe(2);
    const empty = st();
    expect(sessionReducer(empty, { type: 'view/next' }).viewIndex).toBe(0);
    expect(sessionReducer(empty, { type: 'view/prev' }).viewIndex).toBe(0);
  });

  it('clearHistory wipes everything, but only when idle', () => {
    const cleared = sessionReducer(three(), { type: 'history/clear' });
    expect(cleared.history).toEqual([]);
    expect(cleared.viewIndex).toBe(0);
    for (const ui of ['starting', 'recording', 'finalizing', 'answering'] as const) {
      const busy = three();
      const prev = { ...busy, ui };
      expect(sessionReducer(prev, { type: 'history/clear' })).toBe(prev);
    }
  });
});

describe('error/set', () => {
  it('sets and clears the surfaced error', () => {
    const withErr = sessionReducer(st(), { type: 'error/set', error: err('llm_rate_limit') });
    expect(withErr.error).toEqual(err('llm_rate_limit'));
    expect(sessionReducer(withErr, { type: 'error/set', error: null }).error).toBeNull();
  });
});

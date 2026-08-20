/**
 * Side-effect shell around the pure reducer: bridge calls, the recording
 * timer, event subscriptions. All decisions live in reducer.ts; this file
 * only decides WHEN to dispatch and which core session to poke.
 */
import { useCallback, useEffect, useReducer, useRef } from 'react';
import { getBridge } from '../bridge';
import type { AppError } from '../types';
import { initialState, sessionReducer } from './reducer';
import type { HistoryEntry, UiState } from './reducer';

export type { HistoryEntry, UiState } from './reducer';

export interface SessionApi {
  state: UiState;
  error: AppError | null;
  rms: number;
  elapsedMs: number;
  hitRecordingCap: boolean;
  history: HistoryEntry[];
  viewIndex: number;
  viewed: HistoryEntry | null;
  toggleRecord(): void;
  submitAsk(text: string): Promise<boolean>;
  regenerate(): void;
  clearHistory(): void;
  viewPrev(): void;
  viewNext(): void;
  setError(e: AppError | null): void;
}

/** Coarse enough to be cheap, fine enough that mm:ss never visibly skips. */
const TICK_MS = 250;

export function useSession(): SessionApi {
  const [state, dispatch] = useReducer(sessionReducer, initialState);

  // Callbacks read the freshest state through a ref so they can stay
  // referentially stable; recreating them per render would churn the bridge
  // subscription and any consumer memoization.
  const stateRef = useRef(state);
  stateRef.current = state;

  const keyCounter = useRef(0);
  // The attempt whose resolution we still care about. A start/ask promise
  // settling with any other key belongs to a superseded or aborted attempt:
  // its session must be put down, never adopted.
  const attemptRef = useRef<string | null>(null);
  // The Stop button and the global hotkey can both fire in the same tick,
  // before React re-renders stateRef out of 'recording'; the core expects
  // exactly one stop.
  const stopIssuedFor = useRef<string | null>(null);

  const startFlow = useCallback(async (): Promise<void> => {
    const bridge = getBridge();
    const s = stateRef.current;
    // Starting over a streaming answer supersedes it — kill the old session
    // so its tokens stop burning; its late events are dropped as stale.
    if (s.ui === 'answering' && s.activeId !== null) bridge.cancelSession(s.activeId);
    keyCounter.current += 1;
    const key = `attempt-${keyCounter.current}`;
    attemptRef.current = key;
    stopIssuedFor.current = null;
    dispatch({ type: 'record/start', key });
    const env = await bridge.startSession();
    if (attemptRef.current !== key) {
      // The user aborted or superseded while this call was in flight; a
      // session that started anyway is an orphan and must be cancelled.
      if (env.ok) bridge.cancelSession(env.value);
      return;
    }
    if (env.ok) {
      dispatch({ type: 'record/started', key, id: env.value });
    } else {
      attemptRef.current = null;
      dispatch({ type: 'record/startFailed', key, error: env.error });
    }
  }, []);

  const issueStop = useCallback(async (): Promise<void> => {
    const bridge = getBridge();
    const s = stateRef.current;
    if (s.ui !== 'recording' || s.activeId === null || s.liveKey === null) return;
    if (stopIssuedFor.current === s.liveKey) return;
    stopIssuedFor.current = s.liveKey;
    const key = s.liveKey;
    const id = s.activeId;
    dispatch({ type: 'record/stop' });
    const env = await bridge.stopSession(id);
    // An error envelope means the stop was not taken: nothing will ever be
    // emitted for this session, so sitting in "Finalizing…" would hang.
    if (!env.ok) dispatch({ type: 'record/stopRejected', key, error: env.error });
  }, []);

  const toggleRecord = useCallback((): void => {
    const s = stateRef.current;
    if (s.ui === 'recording') {
      void issueStop();
      return;
    }
    if (s.ui === 'starting') {
      // Marking the attempt stale is what makes the eventual startSession
      // resolution cancel instead of adopt.
      attemptRef.current = null;
      dispatch({ type: 'record/abortStarting' });
      return;
    }
    if (s.ui === 'idle' || s.ui === 'answering') {
      void startFlow();
    }
    // finalizing: the stop is already committed; toggling again is a no-op.
  }, [issueStop, startFlow]);

  const submitAsk = useCallback(async (text: string): Promise<boolean> => {
    const bridge = getBridge();
    const s = stateRef.current;
    if (s.ui === 'starting' || s.ui === 'recording' || s.ui === 'finalizing') return false;
    if (text.trim() === '') return false;
    if (s.ui === 'answering' && s.activeId !== null) bridge.cancelSession(s.activeId);
    keyCounter.current += 1;
    const key = `attempt-${keyCounter.current}`;
    attemptRef.current = key;
    dispatch({ type: 'ask/start', key, question: text });
    const env = await bridge.ask(text);
    if (attemptRef.current !== key) {
      if (env.ok) bridge.cancelSession(env.value);
      return false;
    }
    if (env.ok) {
      dispatch({ type: 'ask/accepted', key, id: env.value });
      return true;
    }
    attemptRef.current = null;
    dispatch({ type: 'ask/failed', key, error: env.error });
    return false;
  }, []);

  const regenerate = useCallback((): void => {
    const s = stateRef.current;
    const viewed = s.history[s.viewIndex];
    if (viewed === undefined || viewed.question.trim() === '') return;
    void submitAsk(viewed.question);
  }, [submitAsk]);

  const clearHistory = useCallback((): void => {
    dispatch({ type: 'history/clear' });
  }, []);
  const viewPrev = useCallback((): void => {
    dispatch({ type: 'view/prev' });
  }, []);
  const viewNext = useCallback((): void => {
    dispatch({ type: 'view/next' });
  }, []);
  const setError = useCallback((e: AppError | null): void => {
    dispatch({ type: 'error/set', error: e });
  }, []);

  // The hotkey handler must see the current toggleRecord without forcing the
  // subscription effect to re-run.
  const toggleRef = useRef(toggleRecord);
  toggleRef.current = toggleRecord;

  useEffect(() => {
    const bridge = getBridge();
    const unsubs = [
      bridge.on('stt:partial', (payload) => dispatch({ type: 'event/sttPartial', payload })),
      bridge.on('llm:delta', (payload) => dispatch({ type: 'event/llmDelta', payload })),
      bridge.on('llm:done', (payload) => dispatch({ type: 'event/llmDone', payload })),
      bridge.on('session:error', (payload) => dispatch({ type: 'event/sessionError', payload })),
      bridge.on('audio:level', (payload) => dispatch({ type: 'event/audioLevel', payload })),
      bridge.on('hotkey:toggle', () => toggleRef.current()),
    ];
    return () => {
      for (const unsub of unsubs) unsub();
    };
  }, []);

  // The clock must track WALL time, not fire counts: WebView2 throttles
  // timers in background windows (the global-hotkey flow runs minimized), so
  // a fixed 250 ms per fire would run this clock at a fraction of real time
  // and lag the core's 120 s cap (§3) by minutes. Deltas come from
  // performance.now(), which is monotonic — a system clock jump mid-recording
  // still cannot corrupt the timer. The cap itself is a pure reducer
  // transition on 'tick'; no stop is issued here, because the core's own cap
  // timer provably fires first and a stop_session at that point is refused
  // "not taken" — reacting to that refusal is the double-stop race that used
  // to drop every capped recording's answer.
  useEffect(() => {
    if (state.ui !== 'recording') return;
    let last = performance.now();
    const timer = setInterval(() => {
      const now = performance.now();
      dispatch({ type: 'tick', deltaMs: now - last });
      last = now;
    }, TICK_MS);
    return () => clearInterval(timer);
  }, [state.ui]);

  return {
    state: state.ui,
    error: state.error,
    rms: state.rms,
    elapsedMs: state.elapsedMs,
    hitRecordingCap: state.hitRecordingCap,
    history: state.history,
    viewIndex: state.viewIndex,
    viewed: state.history[state.viewIndex] ?? null,
    toggleRecord,
    submitAsk,
    regenerate,
    clearHistory,
    viewPrev,
    viewNext,
    setError,
  };
}

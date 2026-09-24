/**
 * Side-effect shell around the pure reducer: bridge calls, the recording
 * timer, event subscriptions. All decisions live in reducer.ts; this file
 * only decides WHEN to dispatch and which core session to poke.
 */
import { useCallback, useEffect, useReducer, useRef } from 'react';
import { getBridge, type Subscription } from '../bridge';
import type {
  AppError,
  Envelope,
  LlmDeltaEvent,
  LlmDoneEvent,
  SessionErrorEvent,
  SessionId,
  SessionStart,
  SttPartialEvent,
} from '../types';
import { initialState, sessionReducer } from './reducer';
import type { HistoryEntry, SessionAction, UiState } from './reducer';

export type { EntryStatus, HistoryEntry, UiState } from './reducer';

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

export interface UseSessionOptions {
  /**
   * Consulted at the moment a `hotkey:toggle` lands; return false to drop
   * it. §9: the shortcut is IGNORED while Settings is open — the user may be
   * typing the accelerator itself into the hotkey field. Read through a ref
   * so the caller can pass a fresh closure every render without churning the
   * bridge subscription.
   */
  hotkeyEnabled?: () => boolean;
}

/** Coarse enough to be cheap, fine enough that mm:ss never visibly skips. */
const TICK_MS = 250;

/**
 * FE-1: the delta coalescing window. A fast provider lands several tokens a
 * millisecond, and every one used to be a reducer pass plus a render of the
 * whole main view. The FIRST delta of a burst still dispatches synchronously
 * (first paint as early as ever); the rest of the window is merged into one
 * dispatch. setTimeout, not requestAnimationFrame: WebView2 throttles rAF in
 * a minimized window — the global-hotkey flow runs minimized — and a stream
 * parked behind a throttled frame would land in one lump on restore.
 */
const DELTA_COALESCE_MS = 16;

/**
 * R1: events that arrive while a start/ask call is still in flight, kept for
 * the attempt that is waiting to adopt its id. Consecutive deltas of one
 * session merge and consecutive partials of one session replace each other
 * (a partial is the full transcript so far), so a real session needs a
 * handful of slots; the cap only bounds a pathological stream of other
 * sessions' events. Audio levels are never buffered — they are cosmetic.
 */
const PENDING_EVENT_LIMIT = 64;

type BufferedEvent =
  | { kind: 'partial'; payload: SttPartialEvent }
  | { kind: 'delta'; payload: LlmDeltaEvent }
  | { kind: 'done'; payload: LlmDoneEvent }
  | { kind: 'error'; payload: SessionErrorEvent };

interface PendingBuffer {
  /** The attempt this buffer belongs to; any other attempt's adoption ignores it. */
  key: string;
  events: BufferedEvent[];
  /** Ids that lost a held event to the cap (review F4). */
  evicted: Set<SessionId>;
}

/**
 * Which entry to drop at the cap. Session ids only grow, so the newest id in
 * the buffer is the one this attempt will almost certainly adopt: evict the
 * oldest event of any OTHER id first, and only fall back to the oldest event
 * overall (review F4 — evicting the adopted id's first delta used to leave a
 * fragment with its start missing).
 */
function evictionIndex(events: BufferedEvent[]): number {
  let newest = -Infinity;
  for (const ev of events) newest = Math.max(newest, ev.payload.sessionId);
  const other = events.findIndex((ev) => ev.payload.sessionId !== newest);
  return other === -1 ? 0 : other;
}

function bufferEvent(buf: PendingBuffer, ev: BufferedEvent): void {
  const last = buf.events[buf.events.length - 1];
  if (last !== undefined && last.payload.sessionId === ev.payload.sessionId) {
    if (last.kind === 'delta' && ev.kind === 'delta') {
      buf.events[buf.events.length - 1] = {
        kind: 'delta',
        payload: { sessionId: ev.payload.sessionId, delta: last.payload.delta + ev.payload.delta },
      };
      return;
    }
    if (last.kind === 'partial' && ev.kind === 'partial') {
      buf.events[buf.events.length - 1] = ev;
      return;
    }
  }
  buf.events.push(ev);
  if (buf.events.length > PENDING_EVENT_LIMIT) {
    const [dropped] = buf.events.splice(evictionIndex(buf.events), 1);
    if (dropped !== undefined) buf.evicted.add(dropped.payload.sessionId);
  }
}

function actionFor(ev: BufferedEvent): SessionAction {
  switch (ev.kind) {
    case 'partial':
      return { type: 'event/sttPartial', payload: ev.payload };
    case 'delta':
      return { type: 'event/llmDelta', payload: ev.payload };
    case 'done':
      return { type: 'event/llmDone', payload: ev.payload };
    case 'error':
      return { type: 'event/sessionError', payload: ev.payload };
  }
}

/** Listener readiness (R1): whether the session-event subscriptions are live. */
type Readiness =
  | { state: 'pending'; promise: Promise<AppError | null> }
  | { state: 'ok' }
  | { state: 'failed'; error: AppError };

/** Fold every subscription's readiness into the first failure, or null. */
async function firstFailure(subs: Subscription[]): Promise<AppError | null> {
  const results: Envelope<null>[] = await Promise.all(subs.map((sub) => sub.ready));
  for (const r of results) if (!r.ok) return r.error;
  return null;
}

/**
 * How long a start waits for listener registration (review F2). `listen()`
 * normally settles in microseconds; one that never settles (a wedged IPC, a
 * webview mid-reload) used to leave Record in "starting" forever with no
 * message. Past this the wait fails visibly and the next attempt
 * re-subscribes instead of awaiting the same dead promise.
 */
export const LISTEN_TIMEOUT_MS = 3000;

export const LISTEN_TIMEOUT_ERROR: AppError = {
  code: 'internal',
  message: 'The app could not start listening for session events. Restart the app.',
};

function withListenTimeout(p: Promise<AppError | null>): Promise<AppError | null> {
  return new Promise((resolve) => {
    const timer = window.setTimeout(() => resolve(LISTEN_TIMEOUT_ERROR), LISTEN_TIMEOUT_MS);
    void p.then((result) => {
      window.clearTimeout(timer);
      resolve(result);
    });
  });
}

export function useSession(options: UseSessionOptions = {}): SessionApi {
  const [state, dispatch] = useReducer(sessionReducer, initialState);

  const hotkeyEnabledRef = useRef(options.hotkeyEnabled);
  hotkeyEnabledRef.current = options.hotkeyEnabled;

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
  // The delta coalescer lives inside the subscription effect; this is its
  // flush, hoisted so the two UI supersede paths (Record over a streaming
  // answer, Ask/Regenerate over one) can drain a token still buffered in the
  // 16 ms window BEFORE they dispatch record/start or ask/start. Without it
  // the reducer settles the old entry a token short and the timer's late
  // dispatch is dropped as stale — the very failure the hotkey handler's
  // flush already guards against, on the two paths that do the same thing.
  const flushRef = useRef<() => void>(() => {});
  // R1: the in-flight attempt's early events (see PendingBuffer). Non-null
  // exactly while a start/ask call is waiting to be adopted.
  const pendingRef = useRef<PendingBuffer | null>(null);
  // R1: listener readiness, set by the subscription effect. `resubscribe`
  // tears the subscriptions down and registers them again — the recovery a
  // failed registration gets on the next Record/Ask instead of a dead app.
  const readinessRef = useRef<Readiness>({ state: 'ok' });
  const resubscribeRef = useRef<() => void>(() => {});

  /**
   * Resolve once the session-event listeners are live, or with the error
   * that stops them being so. Sync-fast on the common path (already live):
   * the caller skips the await entirely, so a start is not delayed a tick.
   */
  const awaitReady = useCallback(async (): Promise<AppError | null> => {
    let r = readinessRef.current;
    if (r.state === 'failed') {
      resubscribeRef.current();
      r = readinessRef.current;
    }
    if (r.state === 'ok') return null;
    if (r.state === 'failed') return r.error;
    return r.promise;
  }, []);

  /**
   * Adopt the id a start/ask resolved with (R1, ADR 015): mark the attempt
   * live, replay the events it buffered for exactly this id (in order, once —
   * they were never dispatched), then reconcile its outcome. A terminal
   * outcome in the envelope settles it now; otherwise one `session_outcome`
   * lookup covers a session that ended between the envelope and this line.
   * Every step is guarded by attempt key AND id in the reducer, so a live
   * terminal event and the reconciliation can both land harmlessly.
   */
  const adopt = useCallback((key: string, start: SessionStart, kind: 'record' | 'ask'): void => {
    const id = start.sessionId;
    const buffered = pendingRef.current;
    pendingRef.current = null;
    flushRef.current();
    dispatch(kind === 'record' ? { type: 'record/started', key, id } : { type: 'ask/accepted', key, id });
    // An id that lost held events to the cap would replay a fragment with its
    // start missing: skip its held deltas and let the outcome (which carries
    // the full text) settle it — a lookup is forced below (review F4).
    const gap = buffered !== null && buffered.key === key && buffered.evicted.has(id);
    if (buffered !== null && buffered.key === key) {
      for (const ev of buffered.events) {
        if (ev.payload.sessionId !== id) continue;
        if (gap && ev.kind === 'delta') continue;
        dispatch(actionFor(ev));
      }
    }
    if (start.outcome.status !== 'active') {
      dispatch({ type: 'session/outcome', key, id, outcome: start.outcome });
      return;
    }
    void getBridge()
      .sessionOutcome(id)
      .then((env) => {
        if (env.ok && env.value.status !== 'active') {
          dispatch({ type: 'session/outcome', key, id, outcome: env.value });
        }
      });
  }, []);

  /** The attempt ended before adoption: its buffer (if still its own) goes too. */
  const dropPending = (key: string): void => {
    if (pendingRef.current?.key === key) pendingRef.current = null;
  };

  const startFlow = useCallback(async (): Promise<void> => {
    const bridge = getBridge();
    const s = stateRef.current;
    // Starting over a streaming answer supersedes it — kill the old session
    // so its tokens stop burning; its late events are dropped as stale. Drain
    // the coalescer first so a buffered token lands on the old entry before
    // record/start settles it.
    flushRef.current();
    if (s.ui === 'answering' && s.activeId !== null) bridge.cancelSession(s.activeId);
    keyCounter.current += 1;
    const key = `attempt-${keyCounter.current}`;
    attemptRef.current = key;
    stopIssuedFor.current = null;
    dispatch({ type: 'record/start', key });
    pendingRef.current = { key, events: [], evicted: new Set() };
    // Readiness is part of the start contract: never start a session whose
    // events nobody is listening for yet.
    if (readinessRef.current.state !== 'ok') {
      const notReady = await awaitReady();
      if (attemptRef.current !== key) return;
      if (notReady !== null) {
        attemptRef.current = null;
        dropPending(key);
        dispatch({ type: 'record/startFailed', key, error: notReady });
        return;
      }
    }
    const env = await bridge.startSession();
    if (attemptRef.current !== key) {
      // The user aborted or superseded while this call was in flight; a
      // session that started anyway is an orphan and must be cancelled.
      dropPending(key);
      if (env.ok) bridge.cancelSession(env.value.sessionId);
      return;
    }
    if (env.ok) {
      adopt(key, env.value, 'record');
    } else {
      attemptRef.current = null;
      dropPending(key);
      dispatch({ type: 'record/startFailed', key, error: env.error });
    }
  }, [adopt, awaitReady]);

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
      pendingRef.current = null;
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
    // Same supersede as startFlow: drain a buffered delta before ask/start
    // retires the streaming entry.
    flushRef.current();
    keyCounter.current += 1;
    const key = `attempt-${keyCounter.current}`;
    attemptRef.current = key;
    dispatch({ type: 'ask/start', key, question: text });
    pendingRef.current = { key, events: [], evicted: new Set() };
    if (readinessRef.current.state !== 'ok') {
      const notReady = await awaitReady();
      if (attemptRef.current !== key) return false;
      if (notReady !== null) {
        attemptRef.current = null;
        dropPending(key);
        dispatch({ type: 'ask/failed', key, error: notReady });
        return false;
      }
    }
    const env = await bridge.ask(text);
    if (attemptRef.current !== key) {
      dropPending(key);
      if (env.ok) bridge.cancelSession(env.value.sessionId);
      return false;
    }
    if (env.ok) {
      adopt(key, env.value, 'ask');
      return true;
    }
    attemptRef.current = null;
    dropPending(key);
    dispatch({ type: 'ask/failed', key, error: env.error });
    return false;
  }, [adopt, awaitReady]);

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

    // ---------------------------------------------------- delta coalescer
    // Leading + trailing: the first delta of a burst goes straight through,
    // later ones inside the window accumulate here and go out as one.
    let pending: LlmDeltaEvent | null = null;
    let timer: number | null = null;

    const flushPending = (): void => {
      const p = pending;
      pending = null;
      if (p !== null) dispatch({ type: 'event/llmDelta', payload: p });
    };
    // Every OTHER handler calls this first, so event ORDER survives the
    // buffering: a done/error/partial that overtook a buffered delta would
    // leave that delta to be dropped as stale — a session:error mid-stream
    // would then lose the tail of the partial answer the reducer keeps, and
    // a start (hotkey, Record button or a typed Ask) would retire the old
    // entry a token short.
    const flushNow = (): void => {
      if (timer !== null) {
        window.clearTimeout(timer);
        timer = null;
      }
      flushPending();
    };
    flushRef.current = flushNow;
    const onTimer = (): void => {
      timer = null;
      // Nothing arrived during the window: the burst is over and the next
      // delta should paint immediately again.
      if (pending === null) return;
      flushPending();
      // Still streaming: keep the window rolling so a steady stream renders
      // exactly once per window instead of alternating sync and buffered.
      timer = window.setTimeout(onTimer, DELTA_COALESCE_MS);
    };
    const onDelta = (payload: LlmDeltaEvent): void => {
      if (timer === null) {
        dispatch({ type: 'event/llmDelta', payload });
        timer = window.setTimeout(onTimer, DELTA_COALESCE_MS);
        return;
      }
      if (pending === null) {
        pending = payload;
      } else if (pending.sessionId === payload.sessionId) {
        pending = { sessionId: payload.sessionId, delta: pending.delta + payload.delta };
      } else {
        // Never merge across sessions — the reducer keys on sessionId, and
        // a stale session's token must not ride into the live answer.
        flushPending();
        pending = payload;
      }
    };

    // R1: while an attempt waits for its id, session events are held for it
    // instead of dispatched — the reducer could not match them yet and would
    // drop them. `adopt` replays the adopted id's share exactly once.
    const hold = (ev: BufferedEvent): boolean => {
      const pending = pendingRef.current;
      if (pending === null) return false;
      bufferEvent(pending, ev);
      return true;
    };

    let disposed = false;
    let sessionSubs: Subscription[] = [];
    let hotkeySub: Subscription | null = null;

    const subscribeSessionEvents = (): void => {
      sessionSubs = [
        bridge.on('stt:partial', (payload) => {
          if (hold({ kind: 'partial', payload })) return;
          flushNow();
          dispatch({ type: 'event/sttPartial', payload });
        }),
        bridge.on('llm:delta', (payload) => {
          if (hold({ kind: 'delta', payload })) return;
          onDelta(payload);
        }),
        bridge.on('llm:done', (payload) => {
          if (hold({ kind: 'done', payload })) return;
          flushNow();
          dispatch({ type: 'event/llmDone', payload });
        }),
        bridge.on('session:error', (payload) => {
          if (hold({ kind: 'error', payload })) return;
          flushNow();
          dispatch({ type: 'event/sessionError', payload });
        }),
      ];
      // audio:level only drives the meter: its failure is shown but does not
      // gate starting a session (review F2, secondary).
      const levelSub = bridge.on('audio:level', (payload) => {
        if (pendingRef.current !== null) return;
        dispatch({ type: 'event/audioLevel', payload });
      });
      void levelSub.ready.then((env) => {
        if (!disposed && !env.ok) dispatch({ type: 'error/set', error: env.error });
      });
      const gatedSubs = sessionSubs;
      sessionSubs = [...gatedSubs, levelSub];
      const promise = withListenTimeout(firstFailure(gatedSubs));
      const readiness: Readiness = { state: 'pending', promise };
      readinessRef.current = readiness;
      void promise.then((error) => {
        // A newer (re)subscription owns the readiness now.
        if (disposed || readinessRef.current !== readiness) return;
        readinessRef.current = error === null ? { state: 'ok' } : { state: 'failed', error };
      });
    };
    const unsubscribeSessionEvents = (): void => {
      for (const sub of sessionSubs) sub.unsubscribe();
      sessionSubs = [];
    };
    resubscribeRef.current = () => {
      unsubscribeSessionEvents();
      subscribeSessionEvents();
    };
    subscribeSessionEvents();

    hotkeySub = bridge.on('hotkey:toggle', () => {
      flushNow();
      // Default enabled: a caller with no opinion gets the plain toggle.
      if (hotkeyEnabledRef.current?.() ?? true) toggleRef.current();
    });
    // The shortcut is not part of the start contract (Record still works
    // without it), but a dead shortcut must not be silent either.
    void hotkeySub.ready.then((env) => {
      if (!disposed && !env.ok) dispatch({ type: 'error/set', error: env.error });
    });

    return () => {
      disposed = true;
      unsubscribeSessionEvents();
      hotkeySub?.unsubscribe();
      resubscribeRef.current = () => {};
      readinessRef.current = { state: 'ok' };
      // Whatever is still buffered belongs to the reducer, not to a timer
      // that would fire into an unsubscribed world.
      flushNow();
      flushRef.current = () => {};
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

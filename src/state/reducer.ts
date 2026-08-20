/**
 * Pure session state machine (§9).
 *
 * Every hard rule lives here as a pure transition so it can be unit-tested
 * without React, timers, or a bridge. The hook (useSession) only wires side
 * effects (bridge calls, the interval, event subscriptions) to these actions.
 *
 * Dropped inputs return the SAME state object so callers (and tests) can tell
 * "ignored" apart from "changed" by identity.
 */
import {
  HISTORY_LIMIT,
  MAX_RECORDING_SECONDS,
  type AppError,
  type AudioLevelEvent,
  type LlmDeltaEvent,
  type LlmDoneEvent,
  type Metrics,
  type SessionErrorEvent,
  type SessionId,
  type SttPartialEvent,
} from '../types';

export type UiState = 'idle' | 'starting' | 'recording' | 'finalizing' | 'answering';

export interface HistoryEntry {
  key: string;
  sessionId: SessionId | null;
  question: string;
  answer: string;
  metrics: Metrics | null;
}

export interface SessionState {
  ui: UiState;
  error: AppError | null;
  rms: number;
  elapsedMs: number;
  hitRecordingCap: boolean;
  history: HistoryEntry[];
  viewIndex: number;
  /**
   * The core session currently being tracked. null while a start/ask call is
   * still in flight — events carry a sessionId, so before adoption there is
   * nothing to match against and every event must be dropped, not buffered:
   * a buffered event could belong to a session the user already superseded.
   */
  activeId: SessionId | null;
  /** Key of the in-flight history entry; null when nothing is live. */
  liveKey: string | null;
}

export const initialState: SessionState = {
  ui: 'idle',
  error: null,
  rms: 0,
  elapsedMs: 0,
  hitRecordingCap: false,
  history: [],
  viewIndex: 0,
  activeId: null,
  liveKey: null,
};

export type SessionAction =
  | { type: 'record/start'; key: string }
  | { type: 'record/started'; key: string; id: SessionId }
  | { type: 'record/startFailed'; key: string; error: AppError }
  | { type: 'record/abortStarting' }
  | { type: 'record/stop' }
  | { type: 'record/stopRejected'; key: string; error: AppError }
  | { type: 'ask/start'; key: string; question: string }
  | { type: 'ask/accepted'; key: string; id: SessionId }
  | { type: 'ask/failed'; key: string; error: AppError }
  | { type: 'tick'; deltaMs: number }
  | { type: 'event/sttPartial'; payload: SttPartialEvent }
  | { type: 'event/llmDelta'; payload: LlmDeltaEvent }
  | { type: 'event/llmDone'; payload: LlmDoneEvent }
  | { type: 'event/sessionError'; payload: SessionErrorEvent }
  | { type: 'event/audioLevel'; payload: AudioLevelEvent }
  | { type: 'history/clear' }
  | { type: 'view/prev' }
  | { type: 'view/next' }
  | { type: 'error/set'; error: AppError | null };

const RECORDING_CAP_MS = MAX_RECORDING_SECONDS * 1000;

/** Whitespace-only capture is indistinguishable from nothing to the user. */
function captured(entry: HistoryEntry): boolean {
  return entry.question.trim() !== '' || entry.answer.trim() !== '';
}

/**
 * Ends the live entry's in-flight status. An attempt that captured nothing is
 * discarded outright; one holding a question or partial answer is retired —
 * the transcript is the user's work and a half-streamed answer vanishing
 * looks like data loss.
 */
function settleLive(state: SessionState): Pick<SessionState, 'history' | 'viewIndex'> {
  const { liveKey } = state;
  if (liveKey === null) return { history: state.history, viewIndex: state.viewIndex };
  const live = state.history.find((e) => e.key === liveKey);
  if (live === undefined || captured(live)) {
    return { history: state.history, viewIndex: state.viewIndex };
  }
  const history = state.history.filter((e) => e.key !== liveKey);
  return {
    history,
    viewIndex: Math.max(0, Math.min(state.viewIndex, history.length - 1)),
  };
}

/**
 * Appends a fresh live entry, jumps the view to it, and trims beyond the
 * limit — always skipping the entry just pushed, because trimming the
 * in-flight entry would strand a session with nowhere to write.
 */
function pushLive(
  history: HistoryEntry[],
  entry: HistoryEntry,
): Pick<SessionState, 'history' | 'viewIndex'> {
  let next = [...history, entry];
  while (next.length > HISTORY_LIMIT) {
    const idx = next.findIndex((e) => e.key !== entry.key);
    if (idx === -1) break;
    next = [...next.slice(0, idx), ...next.slice(idx + 1)];
  }
  return { history: next, viewIndex: next.indexOf(entry) };
}

function updateLive(
  history: HistoryEntry[],
  liveKey: string | null,
  patch: (entry: HistoryEntry) => HistoryEntry,
): HistoryEntry[] {
  if (liveKey === null) return history;
  return history.map((e) => (e.key === liveKey ? patch(e) : e));
}

/**
 * Common end-of-attempt shape. 'aborted' is never surfaced: it only ever
 * means the user superseded or cancelled on purpose, and an error banner
 * would punish an intentional action.
 */
function teardown(state: SessionState, error: AppError | null): SessionState {
  return {
    ...state,
    ...settleLive(state),
    ui: 'idle',
    activeId: null,
    liveKey: null,
    rms: 0,
    error: error !== null && error.code !== 'aborted' ? error : state.error,
  };
}

/** True when the event belongs to the session currently tracked. */
function isCurrent(state: SessionState, sessionId: SessionId): boolean {
  return state.activeId !== null && state.activeId === sessionId;
}

export function sessionReducer(state: SessionState, action: SessionAction): SessionState {
  switch (action.type) {
    case 'record/start': {
      if (state.ui !== 'idle' && state.ui !== 'answering') return state;
      // Starting over a streaming answer supersedes it: the old live entry is
      // settled (kept if it captured anything) before the new one is pushed.
      const settled = { ...state, ...settleLive(state) };
      const entry: HistoryEntry = {
        key: action.key,
        sessionId: null,
        question: '',
        answer: '',
        metrics: null,
      };
      return {
        ...settled,
        ...pushLive(settled.history, entry),
        ui: 'starting',
        activeId: null,
        liveKey: action.key,
        error: null,
        rms: 0,
        elapsedMs: 0,
        hitRecordingCap: false,
      };
    }

    case 'record/started': {
      // A resolution from a superseded/aborted attempt must not resurrect it.
      if (state.ui !== 'starting' || state.liveKey !== action.key) return state;
      return {
        ...state,
        ui: 'recording',
        activeId: action.id,
        history: updateLive(state.history, action.key, (e) => ({ ...e, sessionId: action.id })),
      };
    }

    case 'record/startFailed': {
      if (state.liveKey !== action.key) return state;
      return teardown(state, action.error);
    }

    case 'record/abortStarting': {
      if (state.ui !== 'starting') return state;
      // Silent by contract: the user changed their mind, nothing to report.
      return teardown(state, null);
    }

    case 'record/stop': {
      if (state.ui !== 'recording') return state;
      return { ...state, ui: 'finalizing', rms: 0 };
    }

    case 'record/stopRejected': {
      // Stop being refused means nothing will ever be emitted for this
      // session — waiting in "Finalizing…" would hang forever.
      if (state.ui !== 'finalizing' || state.liveKey !== action.key) return state;
      return teardown(state, action.error);
    }

    case 'ask/start': {
      if (state.ui !== 'idle' && state.ui !== 'answering') return state;
      const settled = { ...state, ...settleLive(state) };
      const entry: HistoryEntry = {
        key: action.key,
        sessionId: null,
        question: action.question,
        answer: '',
        metrics: null,
      };
      return {
        ...settled,
        ...pushLive(settled.history, entry),
        ui: 'answering',
        activeId: null,
        liveKey: action.key,
        error: null,
        rms: 0,
        elapsedMs: 0,
        hitRecordingCap: false,
      };
    }

    case 'ask/accepted': {
      if (state.ui !== 'answering' || state.liveKey !== action.key) return state;
      return {
        ...state,
        activeId: action.id,
        history: updateLive(state.history, action.key, (e) => ({ ...e, sessionId: action.id })),
      };
    }

    case 'ask/failed': {
      if (state.liveKey !== action.key) return state;
      return teardown(state, action.error);
    }

    case 'tick': {
      if (state.ui !== 'recording') return state;
      const elapsedMs = Math.min(state.elapsedMs + action.deltaMs, RECORDING_CAP_MS);
      if (elapsedMs < RECORDING_CAP_MS) return { ...state, elapsedMs };
      // §3: the core enforces the same 120 s cap with a timer armed before
      // this clock even started, so by the time we cross it the core has
      // ALREADY auto-stopped and is answering — and it emits no event for the
      // auto-stop itself. Issuing stop_session now would come back "not
      // taken" (§4), and tearing down on that refusal is exactly the
      // double-stop race that nulled activeId and dropped every capped
      // recording's answer. So the cap is purely local: flip to finalizing,
      // KEEP activeId/liveKey, and let the core's answer events land
      // normally. hitRecordingCap stays latched until the next start so the
      // status line can show §9's "Reached the 120s limit — answering now"
      // throughout finalizing/answering. Because ui leaves 'recording' here,
      // this transition can only happen once per attempt.
      return { ...state, elapsedMs, ui: 'finalizing', rms: 0, hitRecordingCap: true };
    }

    case 'event/audioLevel': {
      if (state.ui !== 'recording' || !isCurrent(state, action.payload.sessionId)) return state;
      return { ...state, rms: action.payload.rms };
    }

    case 'event/sttPartial': {
      if (state.ui !== 'recording' && state.ui !== 'finalizing') return state;
      if (!isCurrent(state, action.payload.sessionId)) return state;
      // Full transcript so far, not a delta — replace, don't append.
      return {
        ...state,
        history: updateLive(state.history, state.liveKey, (e) => ({
          ...e,
          question: action.payload.text,
        })),
      };
    }

    case 'event/llmDelta': {
      // 'recording' is accepted too, as a belt-and-braces companion to the
      // tick-driven cap: answer deltas can only exist after the recording
      // ended, so a delta while we still think we are recording means the
      // core beat our clock to the 120 s auto-stop (§3) without emitting an
      // event for it. The delta itself is the authoritative "recording over"
      // signal — dropping it is how a capped recording would lose its answer.
      if (state.ui !== 'recording' && state.ui !== 'finalizing' && state.ui !== 'answering') {
        return state;
      }
      if (!isCurrent(state, action.payload.sessionId)) return state;
      return {
        ...state,
        ui: 'answering',
        rms: 0,
        history: updateLive(state.history, state.liveKey, (e) => ({
          ...e,
          answer: e.answer + action.payload.delta,
        })),
      };
    }

    case 'event/llmDone': {
      // Same recovery as llmDelta: a done from 'recording' means the whole
      // core-side auto-stop answer raced our clock — complete it, don't drop.
      if (state.ui !== 'recording' && state.ui !== 'finalizing' && state.ui !== 'answering') {
        return state;
      }
      if (!isCurrent(state, action.payload.sessionId)) return state;
      return {
        ...state,
        ui: 'idle',
        activeId: null,
        liveKey: null,
        rms: 0,
        history: updateLive(state.history, state.liveKey, (e) => ({
          ...e,
          question: action.payload.transcript,
          answer: action.payload.answer,
          metrics: action.payload.metrics,
        })),
      };
    }

    case 'event/sessionError': {
      if (!isCurrent(state, action.payload.sessionId)) return state;
      // teardown keeps a captured entry as-is, so a partial answer survives
      // an error mid-stream.
      return teardown(state, action.payload.error);
    }

    case 'history/clear': {
      // Clearing under a live session would strand it with nowhere to write.
      if (state.ui !== 'idle') return state;
      return { ...state, history: [], viewIndex: 0 };
    }

    case 'view/prev': {
      return { ...state, viewIndex: Math.max(0, state.viewIndex - 1) };
    }

    case 'view/next': {
      return {
        ...state,
        viewIndex: Math.max(0, Math.min(state.history.length - 1, state.viewIndex + 1)),
      };
    }

    case 'error/set': {
      return { ...state, error: action.error };
    }
  }
}

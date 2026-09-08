/**
 * The frontend <-> core contract (§4).
 *
 * Everything crossing the IPC boundary is declared here and nowhere else, so a
 * change to the Rust side has exactly one place to land on the TypeScript side.
 */

/** The closed set of error codes the UI keys its behavior off (§4). */
export type ErrorCode =
  | 'no_stt_key'
  | 'no_llm_key'
  | 'stt_connect'
  | 'stt_error'
  | 'stt_timeout'
  | 'no_speech'
  | 'llm_auth'
  | 'llm_http'
  | 'llm_rate_limit'
  | 'llm_first_token_timeout'
  | 'llm_timeout'
  | 'aborted'
  | 'internal';

export interface AppError {
  code: ErrorCode;
  message: string;
}

/**
 * Commands return this envelope rather than throwing across the boundary, so a
 * validation failure and a pipeline failure are handled by the same code path.
 */
export type Envelope<T> = { ok: true; value: T } | { ok: false; error: AppError };

export type SessionId = number;

export type LlmProviderKind = 'anthropic' | 'groq' | 'local';
export interface LocalVoiceStatus {
  ollamaRunning: boolean;
  modelAvailable: boolean;
  speechReady: boolean;
}

export type AnswerStyle = 'brief' | 'balanced' | 'detailed';

/** All measured from the instant Stop was requested (§3). */
export interface Metrics {
  /** Exactly 0 for typed questions — there was no STT stage. */
  sttFinalizeMs: number;
  /** Never 0: a non-streaming provider reports totalMs instead. */
  firstTokenMs: number;
  totalMs: number;
}

export interface SettingsView {
  resume: string;
  jobDescription: string;
  alwaysOnTop: boolean;
  llmProvider: LlmProviderKind;
  answerStyle: AnswerStyle;
  hotkey: string;
  /** Key material never crosses this boundary — only whether one is stored. */
  hasDeepgramKey: boolean;
  hasAnthropicKey: boolean;
  hasGroqKey: boolean;
}

/**
 * Omitted fields are left untouched. For the three key fields that is
 * load-bearing: the Settings form only sends a key the user actually typed
 * into, and an empty string means "clear it".
 */
export interface SettingsPatch {
  resume?: string;
  jobDescription?: string;
  alwaysOnTop?: boolean;
  llmProvider?: LlmProviderKind;
  answerStyle?: AnswerStyle;
  hotkey?: string;
  deepgramKey?: string;
  anthropicKey?: string;
  groqKey?: string;
}

/* ---------------------------------------------------------------- events -- */

export interface SttPartialEvent {
  sessionId: SessionId;
  /** The full transcript so far, not a delta. */
  text: string;
  isFinal: boolean;
}

export interface LlmDeltaEvent {
  sessionId: SessionId;
  delta: string;
}

export interface LlmDoneEvent {
  sessionId: SessionId;
  transcript: string;
  answer: string;
  metrics: Metrics;
}

export interface SessionErrorEvent {
  sessionId: SessionId;
  error: AppError;
}

export interface AudioLevelEvent {
  sessionId: SessionId;
  /** 0.0..=1.0 */
  rms: number;
}

/** Event name -> payload. Used to type the bridge's `on` helper. */
export interface EventMap {
  'stt:partial': SttPartialEvent;
  'llm:delta': LlmDeltaEvent;
  'llm:done': LlmDoneEvent;
  'session:error': SessionErrorEvent;
  'audio:level': AudioLevelEvent;
  'hotkey:toggle': null;
}

export type EventName = keyof EventMap;

/** Hard limits mirrored from the core, for UI copy only (§3). */
export const MAX_RECORDING_SECONDS = 120;
export const MAX_ASK_CHARS = 8000;
export const HISTORY_LIMIT = 6;

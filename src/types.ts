/**
 * The frontend <-> core contract (§4).
 *
 * Everything crossing the IPC boundary is declared here and nowhere else, so a
 * change to the Rust side has exactly one place to land on the TypeScript side.
 * The Rust twins live in `src-tauri/core/src/store/mod.rs` (settings shapes),
 * `src-tauri/core/src/llm/{mod,prompt}.rs` (enums) and `src-tauri/src/hotkey.rs`.
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
  | 'internal'
  /**
   * A full Settings form was seeded from an older settings revision than the
   * one now committed (R5, ADR 016). The form keeps its draft and offers a
   * reload; nothing was saved.
   */
  | 'settings_conflict';

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

/**
 * Why generation stopped (R2), separate from whether the answer finished:
 * `token_limit` is a complete-but-capped answer the UI labels "cut short".
 * Mirrors `StopReason` in `src-tauri/core/src/llm/mod.rs`.
 */
export type StopReason = 'complete' | 'token_limit';

/**
 * How a core session ended, or that it has not yet (R1, ADR 015). Returned
 * next to the id by `start_session`/`ask` and by the `session_outcome`
 * lookup, so a terminal event that arrived before the UI could match its id
 * is never the only record of how the session ended.
 */
export type SessionOutcome =
  | { status: 'active' }
  | { status: 'completed'; transcript: string; answer: string; metrics: Metrics; stopReason: StopReason }
  | { status: 'failed'; error: AppError; transcript: string; partial: string }
  | { status: 'cancelled' }
  /** Never started, or retired from the core's bounded outcome log. */
  | { status: 'unknown' };

/** What `start_session` and `ask` resolve with. */
export interface SessionStart {
  sessionId: SessionId;
  outcome: SessionOutcome;
}

/* ------------------------------------------------------------- enums ---- */

export type LlmProviderKind = 'anthropic' | 'groq' | 'local';
export type AnswerStyle = 'brief' | 'balanced' | 'detailed';

/**
 * What kind of call a profile is for (§7). `interview` is the v3 behavior and
 * the migration default; the other kinds swap the prompt's section headers
 * and add a one-line call framing inside the cached prefix.
 */
export type CallType = 'interview' | 'sales' | 'support' | 'meeting' | 'other';

/**
 * Where the window goes on launch (§9). `camera` re-docks to the top-centre of
 * the display every launch (saved size kept, saved position ignored) so the
 * answer sits directly under the webcam; `remembered` restores saved bounds.
 */
export type LaunchPlacement = 'remembered' | 'camera';

/**
 * How the answer panel scrolls while a stream lands (§9). `tail` is the
 * pinned default (stick to the bottom only if already there); `top` keeps the
 * reader parked at the opening sentence — teleprompter pacing.
 */
export type StreamFollow = 'tail' | 'top';

/* ------------------------------------------------------------ shapes ---- */

export interface LocalVoiceStatus {
  ollamaRunning: boolean;
  modelAvailable: boolean;
  speechReady: boolean;
}

/**
 * What `hotkey_status` reports. `registered: false` with a non-empty
 * accelerator is the honest "another app owns this combo (or it did not
 * parse)" signal (§9).
 */
export interface HotkeyStatus {
  accelerator: string;
  registered: boolean;
}

/**
 * One call profile (§8): a self-contained grounding bundle. Exactly one is
 * active; the prompt's cached prefix is built from the active one, so
 * switching profiles is a deliberate (rare, between-calls) cache write.
 */
export interface CallProfile {
  /** Stable id; `[A-Za-z0-9_-]{1,40}`. The core repairs invalid/duplicate ids. */
  id: string;
  /** Display name, trimmed, ≤ MAX_PROFILE_NAME_CHARS; empty becomes "Untitled". */
  name: string;
  callType: CallType;
  /** Stored verbatim (never trimmed), ≤ MAX_PROFILE_CHARS. */
  resume: string;
  /** Job description (interview) or call context (other call types). Verbatim, ≤ MAX_PROFILE_CHARS. */
  jobDescription: string;
  /** What to emphasise, e.g. a tech stack. ≤ MAX_FOCUS_CHARS. */
  focus: string;
  /** Free-form extra instructions appended to the cached prefix. ≤ MAX_EXTRA_INSTRUCTIONS_CHARS. */
  extraInstructions: string;
}

/** All measured from the instant Stop was requested (§3). */
export interface Metrics {
  /** Exactly 0 for typed questions — there was no STT stage. */
  sttFinalizeMs: number;
  /** Never 0: a non-streaming provider reports totalMs instead. */
  firstTokenMs: number;
  totalMs: number;
}

export interface SettingsView {
  /**
   * The committed editable-settings revision (R5, ADR 016). Bumped once per
   * successful save, never by a failed write or a window move. The UI keeps
   * the highest one it has seen and ignores older responses; the Settings
   * form sends the one it was seeded from as `expectedRevision`.
   */
  revision: number;
  /**
   * Set when the settings file could not be used at startup (quarantined as
   * damaged, or unreadable and protected from overwriting). Null normally.
   */
  storageWarning: string | null;
  /** Never empty (the core guarantees at least one profile), ≤ MAX_PROFILES. */
  profiles: CallProfile[];
  /** Always names an entry of `profiles`. */
  activeProfileId: string;
  alwaysOnTop: boolean;
  llmProvider: LlmProviderKind;
  answerStyle: AnswerStyle;
  hotkey: string;
  launchPlacement: LaunchPlacement;
  streamFollow: StreamFollow;
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
  /**
   * The revision the form was seeded from. The core commits only if it is
   * still current, else fails with `settings_conflict`. The full form always
   * sends it; the single-field chip patches never do, because they merge.
   */
  expectedRevision?: number;
  /** Whole-array replace: the Settings form owns the draft and sends all of it. */
  profiles?: CallProfile[];
  /**
   * Sent ALONE by the main-view profile switcher — a switch never rewrites
   * profile text. An id that names no profile leaves the active one unchanged.
   */
  activeProfileId?: string;
  alwaysOnTop?: boolean;
  llmProvider?: LlmProviderKind;
  answerStyle?: AnswerStyle;
  hotkey?: string;
  launchPlacement?: LaunchPlacement;
  streamFollow?: StreamFollow;
  deepgramKey?: string;
  anthropicKey?: string;
  groqKey?: string;
}

/**
 * How a local request sits against the byte limit (R4). `tight` is a
 * usability warning (under `reserveBytes` left for the question); only
 * `over` is a refusal, and only for the local provider.
 */
export type BudgetStatus = 'ok' | 'tight' | 'over';

/**
 * What `local_prompt_budget` returns: the exact byte size of the local request
 * the core would build for a profile draft, style and question, computed by
 * the same Rust code the answer path enforces. Mirrors `LocalPromptBudget` in
 * `src-tauri/core/src/llm/prompt.rs`.
 */
export interface LocalPromptBudget {
  usedBytes: number;
  limitBytes: number;
  /** `limitBytes - usedBytes`; negative when over. */
  remainingBytes: number;
  /** Instructions, headers, style suffix and question wrapper. */
  fixedBytes: number;
  /** The profile fields as the prompt uses them (edge-trimmed). */
  profileBytes: number;
  questionBytes: number;
  reserveBytes: number;
  status: BudgetStatus;
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
  /** Outcome metadata, kept apart from the timing in `metrics` (R2). */
  stopReason: StopReason;
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

/* ------------------------------------------------------------- limits ---- */

/** Hard limits mirrored from the core, for UI copy and client-side caps only (§3, §8). */
export const MAX_RECORDING_SECONDS = 120;
export const MAX_ASK_CHARS = 8000;
export const HISTORY_LIMIT = 6;
export const MAX_PROFILES = 8;
export const MAX_PROFILE_NAME_CHARS = 60;
export const MAX_PROFILE_CHARS = 200_000;
export const MAX_FOCUS_CHARS = 2000;
export const MAX_EXTRA_INSTRUCTIONS_CHARS = 2000;
/** Mirrors core `DEFAULT_HOTKEY` (src-tauri/core/src/store/mod.rs) exactly — the string the core stores. */
export const DEFAULT_HOTKEY = 'Ctrl+Shift+Space';
export const DEFAULT_PROFILE_ID = 'default';
export const DEFAULT_PROFILE_NAME = 'Default';

/* ---------------------------------------------------------- catalogues --- */

/**
 * Per-provider capabilities, so the UI derives option lists, the first-run
 * rule, hidden key fields and the local-mode banner from ONE table instead
 * of scattering `=== 'local'` checks.
 */
export const PROVIDERS: Record<
  LlmProviderKind,
  { label: string; keyFlag: 'hasAnthropicKey' | 'hasGroqKey' | null; usesDeepgram: boolean }
> = {
  anthropic: { label: 'Claude Haiku 4.5 (recommended)', keyFlag: 'hasAnthropicKey', usesDeepgram: true },
  groq: { label: 'Groq GPT-OSS 120B (fastest)', keyFlag: 'hasGroqKey', usesDeepgram: true },
  local: { label: 'Free local voice (Qwen3.5 2B + Moonshine)', keyFlag: null, usesDeepgram: false },
};

export const PROVIDER_ORDER: ReadonlyArray<LlmProviderKind> = ['anthropic', 'groq', 'local'];

export const CALL_TYPES: ReadonlyArray<{ value: CallType; label: string }> = [
  { value: 'interview', label: 'Job interview' },
  { value: 'sales', label: 'Sales call' },
  { value: 'support', label: 'Customer support call' },
  { value: 'meeting', label: 'Work meeting' },
  { value: 'other', label: 'Other' },
];

/** A blank profile in the shape the core would produce for `id`/`name`. */
export function emptyProfile(id: string, name: string): CallProfile {
  return { id, name, callType: 'interview', resume: '', jobDescription: '', focus: '', extraInstructions: '' };
}

/** True when the selected provider (plus Deepgram, when used) has every key it needs. */
export function hasRequiredKeys(s: SettingsView): boolean {
  const p = PROVIDERS[s.llmProvider];
  if (p.usesDeepgram && !s.hasDeepgramKey) return false;
  if (p.keyFlag !== null && !s[p.keyFlag]) return false;
  return true;
}

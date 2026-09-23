import type { AppError, ErrorCode } from '../types';

/**
 * Short human titles per code; the raw message carries the detail. Keyed as a
 * total record so adding an error code without UI copy is a compile error, not
 * a blank box at runtime.
 */
const TITLES: Record<ErrorCode, string> = {
  no_stt_key: 'Deepgram key missing',
  no_llm_key: 'AI key missing',
  stt_connect: 'Could not reach the transcription service',
  stt_error: 'Transcription failed',
  stt_timeout: 'Transcription timed out',
  no_speech: 'No speech detected',
  llm_auth: 'The AI provider rejected the key',
  llm_http: 'The AI request failed',
  llm_rate_limit: 'Rate limited by the AI provider',
  llm_first_token_timeout: 'The model was slow to start',
  llm_timeout: 'The model timed out',
  aborted: '', // never rendered — see below
  internal: 'Something went wrong',
  settings_conflict: 'Settings changed elsewhere',
};

export function ErrorBox({ error }: { error: AppError | null }) {
  // `aborted` is the user's own cancel echoing back through the pipeline;
  // presenting it as a failure would punish the exact action we offered.
  if (error == null || error.code === 'aborted') return null;
  return (
    <div className="error-box" role="alert">
      <strong className="error-title">{TITLES[error.code]}</strong>
      <span className="error-message">{error.message}</span>
    </div>
  );
}

import { forwardRef } from 'react';
import type { UiState } from '../state/useSession';
import { formatHotkey } from '../format';
import type { HotkeyStatus } from '../types';

interface RecordButtonProps {
  state: UiState;
  hotkey: HotkeyStatus | null;
  onToggle(): void;
}

function label(state: UiState): string {
  switch (state) {
    case 'starting':
      return 'Starting…';
    case 'recording':
      return 'Stop & Answer';
    case 'idle':
    case 'finalizing':
    case 'answering':
      // During finalizing/answering the next press means "record the next
      // question", so the label cycles back to Record rather than to a
      // meaningless Stop.
      return 'Record';
  }
}

/**
 * The one big button. Exposes its ref so Clear-history can hand focus here
 * when the button that had focus disappears with the history bar.
 */
export const RecordButton = forwardRef<HTMLButtonElement, RecordButtonProps>(
  function RecordButton({ state, hotkey, onToggle }, ref) {
    return (
      <button
        ref={ref}
        type="button"
        className={`record-button${state === 'recording' ? ' record-button--recording' : ''}`}
        // Pressing during "starting" aborts the attempt (the hook supports
        // it), so the button stays live there; finalizing is the one state
        // where a press has no defined meaning — the stop is already
        // committed and the answer is about to arrive.
        disabled={state === 'finalizing'}
        onClick={onToggle}
      >
        <span className="record-label">{label(state)}</span>
        {hotkey?.registered && <kbd className="hotkey-chip">{formatHotkey(hotkey.accelerator)}</kbd>}
      </button>
    );
  }
);

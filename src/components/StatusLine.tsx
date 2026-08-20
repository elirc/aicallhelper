import type { UiState } from '../state/useSession';
import { formatHotkey } from '../format';
import type { HotkeyStatus } from './hotkey';

interface StatusLineProps {
  state: UiState;
  hitRecordingCap: boolean;
  /** The last session finished cleanly and nothing new has started. */
  justCompleted: boolean;
  /** A key needed by the selected provider (or Deepgram) is missing. */
  firstRun: boolean;
  hotkey: HotkeyStatus | null;
}

/**
 * One always-present line that says what the app is doing right now. It is the
 * screen-reader narration of the whole state machine, hence role="status".
 */
export function StatusLine({ state, hitRecordingCap, justCompleted, firstRun, hotkey }: StatusLineProps) {
  let text: string;
  if (hitRecordingCap && (state === 'recording' || state === 'finalizing' || state === 'answering')) {
    // The stop the user never pressed: without this line the app appears to
    // stop itself for no reason at exactly 2 minutes.
    text = 'Reached the 120s limit — answering now';
  } else {
    switch (state) {
      case 'starting':
        text = 'Opening the microphone feed…';
        break;
      case 'recording':
        text = 'Recording call audio…';
        break;
      case 'finalizing':
        text = 'Finalizing transcript…';
        break;
      case 'answering':
        text = 'Generating answer…';
        break;
      case 'idle':
        if (firstRun) {
          // Pressing Record with no keys would only produce an error box; point
          // at the fix before the first failure instead of after it.
          text = 'First run: open Settings (gear icon) and add your API keys';
        } else if (justCompleted) {
          text = 'Done — press Record for the next question';
        } else {
          text =
            'Ready — press Record while the other person is speaking' +
            (hotkey?.registered ? ` or ${formatHotkey(hotkey.accelerator)}` : '');
        }
        break;
    }
  }
  return (
    <p className="status-line" role="status">
      {text}
    </p>
  );
}

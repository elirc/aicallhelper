import { useState } from 'react';
import type { FormEvent } from 'react';
import { MAX_ASK_CHARS } from '../types';

interface AskFormProps {
  /** True while starting/recording/finalizing — a typed ask would collide with the live pipeline. */
  disabled: boolean;
  onAsk(text: string): Promise<boolean>;
}

/** Typed questions, for when repeating the interviewer aloud isn't an option. */
export function AskForm({ disabled, onAsk }: AskFormProps) {
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const trimmed = text.trim();
    // The disabled attribute can lag a keyboard Enter by a render; refusing
    // here too means a stale submit can never reach the core mid-recording.
    if (disabled || busy || trimmed === '') return;
    setBusy(true);
    try {
      const accepted = await onAsk(trimmed);
      // Clear only on acceptance: a failed ask keeps the text so the user can
      // fix the problem and retry without retyping.
      if (accepted) setText('');
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="ask-form" onSubmit={(e) => void submit(e)}>
      <input
        type="text"
        className="ask-input"
        value={text}
        maxLength={MAX_ASK_CHARS}
        placeholder="Type a question instead…"
        aria-label="Type a question"
        disabled={disabled}
        onChange={(e) => setText(e.target.value)}
      />
      <button type="submit" className="ask-button" disabled={disabled || text.trim() === ''}>
        Ask
      </button>
    </form>
  );
}

import { useState } from 'react';
import type { AnswerStyle } from '../types';

const STYLES: ReadonlyArray<{ value: AnswerStyle; label: string }> = [
  { value: 'brief', label: 'Brief' },
  { value: 'balanced', label: 'Balanced' },
  { value: 'detailed', label: 'Detailed' },
];

interface StyleChipsProps {
  /** The PERSISTED style — what the next answer will actually use. */
  value: AnswerStyle;
  onSelect(style: AnswerStyle): Promise<void>;
}

export function StyleChips({ value, onSelect }: StyleChipsProps) {
  const [saving, setSaving] = useState(false);

  async function pick(style: AnswerStyle) {
    // Serialize saves: two racing setSettings calls could land out of order
    // and leave the lit chip describing the loser.
    if (saving) return;
    setSaving(true);
    try {
      await onSelect(style);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="style-chips" role="group" aria-label="Answer style">
      {STYLES.map((s) => (
        <button
          key={s.value}
          type="button"
          className="chip style-chip"
          // Pressed mirrors the persisted value, never the clicked chip: an
          // optimistic chip that a failed save leaves lit would promise a style
          // the next answer won't use.
          aria-pressed={s.value === value}
          disabled={saving}
          onClick={() => void pick(s.value)}
        >
          {s.label}
        </button>
      ))}
    </div>
  );
}

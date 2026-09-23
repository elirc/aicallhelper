import { useState } from 'react';
import type { CallProfile } from '../types';

interface ProfileChipsProps {
  profiles: ReadonlyArray<Pick<CallProfile, 'id' | 'name'>>;
  /** The PERSISTED active id — what the next answer will actually use. */
  activeId: string;
  onSelect(id: string): Promise<void>;
  /** Focus mode keeps the row mounted but out of the layout and the a11y tree. */
  hidden?: boolean;
}

/** One chip per call profile; the active one is the prompt's grounding (§8). */
export function ProfileChips({ profiles, activeId, onSelect, hidden = false }: ProfileChipsProps) {
  const [saving, setSaving] = useState(false);

  // One profile = nothing to switch. Same hide rule as the history bar: the
  // row only earns its vertical space once there is a choice to make.
  if (profiles.length < 2) return null;

  async function pick(id: string) {
    // Serialize saves (two racing setSettings calls could land out of order)
    // and skip the no-op: re-selecting the active profile is not a cache
    // write worth making.
    if (saving || id === activeId) return;
    setSaving(true);
    try {
      await onSelect(id);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="profile-chips" role="group" aria-label="Call profile" hidden={hidden}>
      {profiles.map((p) => (
        <button
          key={p.id}
          type="button"
          className="chip profile-chip"
          title={p.name}
          // Pressed mirrors the persisted id, never the clicked chip: an
          // optimistic chip a failed save leaves lit would promise grounding
          // the next answer won't have.
          aria-pressed={p.id === activeId}
          disabled={saving}
          onClick={() => void pick(p.id)}
        >
          {p.name}
        </button>
      ))}
    </div>
  );
}

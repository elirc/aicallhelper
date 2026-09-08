import { lazy, memo, Suspense, useCallback, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { MAX_ASK_CHARS } from '../types';
import { DeferredView } from './DeferredView';

const PracticeLibrary = lazy(() => import('./PracticeLibrary'));

interface AskFormProps {
  /** A typed ask must not collide with the live recording pipeline. */
  disabled: boolean;
  onAsk(text: string): Promise<boolean>;
}

export const AskForm = memo(function AskForm({ disabled, onAsk }: AskFormProps) {
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const libraryButtonRef = useRef<HTMLButtonElement>(null);
  const [libraryOpen, setLibraryOpen] = useState(false);
  const closeLibrary = useCallback(() => {
    setLibraryOpen(false);
    libraryButtonRef.current?.focus();
  }, []);
  const chooseQuestion = useCallback((question: string) => {
    setText(question);
    setLibraryOpen(false);
    inputRef.current?.focus();
  }, []);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const trimmed = text.trim();
    // The ref closes the gap before React commits the disabled button.
    if (disabled || busyRef.current || trimmed === '') return;
    busyRef.current = true;
    setBusy(true);
    try {
      const accepted = await onAsk(trimmed);
      // Keep rejected questions available to retry.
      if (accepted) setText('');
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }

  return (
    <div className="ask-section">
      <form className="ask-form" onSubmit={(e) => void submit(e)}>
        <input
          ref={inputRef}
          type="text"
          className="ask-input"
          value={text}
          maxLength={MAX_ASK_CHARS}
          placeholder="Type a question instead…"
          aria-label="Type a question"
          disabled={disabled || busy}
          onChange={(e) => setText(e.target.value)}
        />
        <button type="submit" className="ask-button" disabled={disabled || busy || text.trim() === ''}>
          {busy ? 'Sending…' : 'Ask'}
        </button>
      </form>
      <button
        ref={libraryButtonRef}
        type="button"
        className="practice-toggle"
        aria-expanded={libraryOpen}
        aria-controls="practice-library"
        onClick={() => setLibraryOpen((open) => !open)}
      >
        <span>Interview prep</span>
        <span className="practice-toggle-note">Questions & quick guides {libraryOpen ? '−' : '+'}</span>
      </button>
      {libraryOpen && (
        <div id="practice-library">
          <DeferredView onClose={closeLibrary}>
            <Suspense fallback={<p role="status">Loading interview prep…</p>}>
              <PracticeLibrary disabled={disabled || busy} onChoose={chooseQuestion} />
            </Suspense>
          </DeferredView>
        </div>
      )}
    </div>
  );
});

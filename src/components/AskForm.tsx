import { lazy, memo, startTransition, Suspense, useCallback, useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { MAX_ASK_CHARS } from '../types';
import { DeferredView } from './DeferredView';

const loadPracticeLibrary = () => import('./PracticeLibrary');
const PracticeLibrary = lazy(loadPracticeLibrary);

/**
 * FE-3: a pointer heading for the toggle (or focus landing on it) is a click
 * a few hundred ms away; fetching the chunk now means the click never waits
 * on disk. A failed fetch is not reported here — the click's own load
 * surfaces it through DeferredView.
 */
function preloadPracticeLibrary(): void {
  void loadPracticeLibrary().catch(() => undefined);
}

interface AskFormProps {
  /** A typed ask must not collide with the live recording pipeline. */
  disabled: boolean;
  onAsk(text: string): Promise<boolean>;
  /**
   * The Interview-prep library is interview-only content; for a sales or
   * support profile the toggle is hidden and an open library is closed.
   */
  showPrep?: boolean;
}

export const AskForm = memo(function AskForm({ disabled, onAsk, showPrep = true }: AskFormProps) {
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
  // Opening mounts a lazy chunk. As a transition the input stays responsive
  // (and a streaming answer keeps painting) while the chunk resolves, instead
  // of the click blocking behind the mount (FE-3).
  const toggleLibrary = useCallback(() => {
    startTransition(() => setLibraryOpen((open) => !open));
  }, []);

  // Switching to a non-interview profile mid-browse: the library must not
  // linger open under a hidden toggle (nothing could close it), and it must
  // not spring back when an interview profile is selected again.
  useEffect(() => {
    if (!showPrep) setLibraryOpen(false);
  }, [showPrep]);

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

  const prepOpen = showPrep && libraryOpen;

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
      {showPrep && (
        <button
          ref={libraryButtonRef}
          type="button"
          className="practice-toggle"
          aria-expanded={prepOpen}
          aria-controls="practice-library"
          onPointerEnter={preloadPracticeLibrary}
          onFocus={preloadPracticeLibrary}
          onClick={toggleLibrary}
        >
          <span>Interview prep</span>
          <span className="practice-toggle-note">Questions & quick guides {prepOpen ? '−' : '+'}</span>
        </button>
      )}
      {prepOpen && (
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

import { memo, useEffect, useLayoutEffect, useRef, useState } from 'react';
import type { AppError } from '../types';
import type { HistoryEntry } from '../state/useSession';
import { formatLatency, latencyTitle } from '../format';
import { Markdown } from '../markdown';

/**
 * A reader who scrolled up to re-read is deliberately not at the bottom; only
 * someone within this many pixels of it is "following the stream" and wants to
 * be carried along.
 */
const STICK_THRESHOLD_PX = 28;
const COPIED_NOTE_MS = 1200;
/**
 * How long the screen-reader announcement stays in the live region before it
 * is wiped. The wipe is what makes the SECOND copy audible: an identical
 * string is a state update React bails out of — no DOM change, no
 * announcement.
 */
const ANNOUNCEMENT_CLEAR_MS = 1500;

interface AnswerPanelProps {
  viewed: HistoryEntry | null;
  /** The viewed entry is the one currently receiving tokens. */
  streaming: boolean;
  canRegenerate: boolean;
  onRegenerate(): void;
  /** Clipboard failures surface in the shared error box, not silently. */
  onCopyError(e: AppError): void;
}

export const AnswerPanel = memo(function AnswerPanel({ viewed, streaming, canRegenerate, onRegenerate, onCopyError }: AnswerPanelProps) {
  const source = viewed?.answer ?? '';
  const entryKey = viewed?.key ?? null;

  // ------------------------------------------------ frame-coalesced markdown
  // Tokens can arrive far faster than 60Hz; re-parsing markdown per token
  // burns the exact CPU the STT/LLM pipeline needs. Hold the freshest source
  // in a ref and commit it to state at most once per animation frame.
  const [displayed, setDisplayed] = useState(source);
  const latestRef = useRef(source);
  const frameRef = useRef<number | null>(null);

  useEffect(() => {
    latestRef.current = source;
    if (!streaming) {
      // Outside a stream there is no flood — render immediately so history
      // switches and final answers never lag a frame behind.
      if (frameRef.current != null) {
        cancelAnimationFrame(frameRef.current);
        frameRef.current = null;
      }
      setDisplayed(source);
      return;
    }
    if (frameRef.current == null) {
      frameRef.current = requestAnimationFrame(() => {
        frameRef.current = null;
        setDisplayed(latestRef.current);
      });
    }
  }, [source, streaming]);

  useEffect(
    () => () => {
      if (frameRef.current != null) cancelAnimationFrame(frameRef.current);
    },
    []
  );

  // ------------------------------------------------------------- autoscroll
  const bodyRef = useRef<HTMLDivElement>(null);
  const nearBottomRef = useRef(true);
  const prevKeyRef = useRef(entryKey);
  // Set on an entry switch, consumed by the [displayed] effect below: the
  // scroll reset must be measured against the NEW entry's committed DOM.
  const keyJustChangedRef = useRef(false);

  function handleScroll() {
    const el = bodyRef.current;
    if (el) {
      nearBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight <= STICK_THRESHOLD_PX;
    }
  }

  // §9: "Switching history entries resets scroll to top", and "following" is
  // re-derived so short content resumes sticking when a new stream starts.
  // The reset CANNOT run here: at this point the DOM still holds the PREVIOUS
  // entry's content, so any measurement lies — a short old entry (fits,
  // nearBottom=true) followed by a long one landed the reader at the BOTTOM,
  // and an empty new stream measured against a long old answer left
  // nearBottom=false so it never stuck. Defer to the [displayed] pass that
  // commits the new content.
  useLayoutEffect(() => {
    if (prevKeyRef.current === entryKey) return;
    prevKeyRef.current = entryKey;
    if (displayed === source) {
      // Identical text: React bails out of setDisplayed, no re-render will
      // follow to consume the flag — but the DOM already IS the new entry's
      // content, so measuring it now is honest.
      resetScrollForNewEntry();
    } else {
      keyJustChangedRef.current = true;
      setDisplayed(source);
    }
  });

  function resetScrollForNewEntry() {
    const el = bodyRef.current;
    if (el) {
      el.scrollTop = 0;
      // Re-derive "following" against the entry now on screen: content that
      // fits keeps sticking for the next stream; a long answer leaves the
      // reader parked at the top it was just reset to.
      nearBottomRef.current = el.scrollHeight - el.clientHeight <= STICK_THRESHOLD_PX;
    }
  }

  useLayoutEffect(() => {
    const el = bodyRef.current;
    if (el == null) return;
    if (keyJustChangedRef.current) {
      // First commit after an entry switch: reset to top instead of sticking —
      // the geometry finally belongs to the new entry.
      keyJustChangedRef.current = false;
      resetScrollForNewEntry();
      return;
    }
    // Stick only if the reader was already at the bottom before this paint —
    // yanking someone down mid-re-read on every token is the failure mode.
    if (nearBottomRef.current) {
      // Clamped: content that fits produces a negative difference, and jsdom
      // (unlike browsers) would store it verbatim.
      el.scrollTop = Math.max(0, el.scrollHeight - el.clientHeight);
    }
  }, [displayed]);

  // ------------------------------------------------------------------- copy
  const [copied, setCopied] = useState(false);
  // Separate from `copied`: the visible note and the announcement have
  // different lifetimes, and tying the live region's text to `copied` would
  // re-couple its clearing to the visual timer.
  const [copyAnnouncement, setCopyAnnouncement] = useState('');
  const copiedTimerRef = useRef<number | null>(null);
  const announcementTimerRef = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (copiedTimerRef.current != null) window.clearTimeout(copiedTimerRef.current);
      if (announcementTimerRef.current != null) window.clearTimeout(announcementTimerRef.current);
    },
    []
  );

  async function copy() {
    try {
      const clipboard = navigator.clipboard;
      if (clipboard == null) throw new Error('clipboard unavailable');
      // Copy the markdown SOURCE, not the rendered text: bullets and numbering
      // survive pasting into notes that way.
      await clipboard.writeText(source);
      setCopied(true);
      if (copiedTimerRef.current != null) window.clearTimeout(copiedTimerRef.current);
      copiedTimerRef.current = window.setTimeout(() => setCopied(false), COPIED_NOTE_MS);
      // §9: Copy "announces to screen readers". Wiped after a beat so the next
      // copy writes a real DOM change (see ANNOUNCEMENT_CLEAR_MS).
      setCopyAnnouncement('Answer copied to clipboard');
      if (announcementTimerRef.current != null) window.clearTimeout(announcementTimerRef.current);
      announcementTimerRef.current = window.setTimeout(() => setCopyAnnouncement(''), ANNOUNCEMENT_CLEAR_MS);
    } catch {
      onCopyError({ code: 'internal', message: 'Could not copy the answer to the clipboard.' });
    }
  }

  const metrics = viewed?.metrics ?? null;

  return (
    <section className="panel answer-panel">
      <div className="panel-head">
        <h2 className="panel-title">Suggested answer</h2>
        {streaming && <span className="tag">generating…</span>}
        {metrics != null && (
          <span className="chip latency-chip" title={latencyTitle(metrics)}>
            {formatLatency(metrics.firstTokenMs)} to first word
          </span>
        )}
        <span className="panel-actions">
          {canRegenerate && (
            <button type="button" className="ghost-button" onClick={onRegenerate}>
              Regenerate
            </button>
          )}
          {source !== '' && (
            <button type="button" className="ghost-button" onClick={() => void copy()}>
              {copied ? 'Copied ✓' : 'Copy'}
            </button>
          )}
        </span>
      </div>
      {/*
        aria-live announces the answer, and aria-busy holds that announcement
        until the stream settles — otherwise a screen reader narrates every
        token batch as a separate interruption.
      */}
      <div
        ref={bodyRef}
        className="panel-body answer-body"
        aria-live="polite"
        aria-busy={streaming}
        onScroll={handleScroll}
      >
        {displayed !== '' ? (
          <Markdown source={displayed} />
        ) : (
          <p className="placeholder">Your AI-suggested answer will stream here.</p>
        )}
      </div>
      {/* Permanently mounted with only its TEXT swapped: several SR/browser
          combos ignore a live region that is inserted together with its
          content — only a content change inside a pre-existing region is
          announced. */}
      <span role="status" className="sr-only">
        {copyAnnouncement}
      </span>
    </section>
  );
});

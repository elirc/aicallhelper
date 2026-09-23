import { memo, useCallback, useLayoutEffect, useRef } from 'react';
import type { AppError, StreamFollow } from '../types';
import type { EntryStatus, HistoryEntry } from '../state/useSession';
import { formatLatency, latencyTitle } from '../format';
import { Markdown } from '../markdown';
import { HistoryBar } from './HistoryBar';
import { useFrameCoalesced } from './useFrameCoalesced';
import { useAnnouncer, useTransient } from './useTransient';

/**
 * A reader who scrolled up to re-read is deliberately not at the bottom; only
 * someone within this many pixels of it is "following the stream" and wants to
 * be carried along.
 */
const STICK_THRESHOLD_PX = 28;
const COPIED_NOTE_MS = 1200;

/**
 * R2: the head tag for an entry that did not simply finish. Completed and
 * still-streaming entries get none — the tag exists to make an interrupted or
 * capped answer recognisable at a glance, long after the global error box
 * was cleared by the next question.
 */
const STATUS_TAG: Partial<Record<EntryStatus, string>> = {
  incomplete: 'incomplete',
  limited: 'cut short',
  cancelled: 'stopped',
};

interface AnswerPanelProps {
  viewed: HistoryEntry | null;
  /** The viewed entry is the one currently receiving tokens. */
  streaming: boolean;
  canRegenerate: boolean;
  onRegenerate(): void;
  /** Clipboard failures surface in the shared error box, not silently. */
  onCopyError(e: AppError): void;
  /**
   * History navigation lives in this panel's head (§9) so prev/next sit
   * next to the answer they page. Primitives + stable callbacks keep the
   * memo effective: the history ARRAY changes identity on every token.
   */
  historyLength: number;
  viewIndex: number;
  /** Clearing mid-session would delete the entry currently being written. */
  idle: boolean;
  onPrev(): void;
  onNext(): void;
  onClear(): void;
  /**
   * §9 stream-follow: `tail` sticks to the bottom (only if already there);
   * `top` parks the reader at the opening sentence — teleprompter pacing.
   */
  streamFollow: StreamFollow;
}

export const AnswerPanel = memo(function AnswerPanel({
  viewed,
  streaming,
  canRegenerate,
  onRegenerate,
  onCopyError,
  historyLength,
  viewIndex,
  idle,
  onPrev,
  onNext,
  onClear,
  streamFollow,
}: AnswerPanelProps) {
  const source = viewed?.answer ?? '';
  const entryKey = viewed?.key ?? null;

  // One markdown parse per animation frame while tokens flood in; an entry
  // switch commits synchronously so the scroll reset below measures the new
  // entry's DOM (see useFrameCoalesced).
  const displayed = useFrameCoalesced(source, streaming, entryKey);

  // ------------------------------------------------------------- autoscroll
  const bodyRef = useRef<HTMLDivElement>(null);
  // `top` is implemented purely by SEEDING this flag (here and on every entry
  // switch): the stick branch below never runs unless the reader is deemed
  // "following", so a teleprompter reader is simply never following.
  const nearBottomRef = useRef(streamFollow === 'tail');
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

  // Declared above both effects that call it: the entry-switch effect (when
  // the DOM already holds the new text) and the [displayed] effect (the
  // first commit after a switch). Re-derives "following" against the entry
  // now on screen: content that fits keeps sticking for the next stream; a
  // long answer leaves the reader parked at the top it was just reset to. In
  // `top` mode the reader is parked regardless — the opening sentence is the
  // point.
  const resetScrollForNewEntry = useCallback(() => {
    const el = bodyRef.current;
    if (el) {
      el.scrollTop = 0;
      nearBottomRef.current = streamFollow === 'tail' && el.scrollHeight - el.clientHeight <= STICK_THRESHOLD_PX;
    }
  }, [streamFollow]);

  // §9: "Switching history entries resets scroll to top", and "following" is
  // re-derived so short content resumes sticking when a new stream starts.
  // The reset CANNOT run here unconditionally: at this point the DOM still
  // holds the PREVIOUS entry's content, so any measurement lies — a short
  // old entry (fits, nearBottom=true) followed by a long one landed the
  // reader at the BOTTOM, and an empty new stream measured against a long
  // old answer left nearBottom=false so it never stuck. Defer to the
  // [displayed] pass that commits the new content — unless the text is
  // identical, in which case React bails out of that commit, no re-render
  // will follow to consume the flag, and the DOM already IS the new entry's
  // content, so measuring it now is honest.
  useLayoutEffect(() => {
    if (prevKeyRef.current === entryKey) return;
    prevKeyRef.current = entryKey;
    if (displayed === source) {
      resetScrollForNewEntry();
    } else {
      keyJustChangedRef.current = true;
    }
  }, [entryKey]);

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
  const [copied, setCopied] = useTransient(false, COPIED_NOTE_MS);
  // Separate from `copied`: the visible note and the announcement have
  // different lifetimes, and tying the live region's text to `copied` would
  // re-couple its clearing to the visual timer.
  const [copyAnnouncement, announce] = useAnnouncer();

  async function copy() {
    try {
      const clipboard = navigator.clipboard;
      if (clipboard == null) throw new Error('clipboard unavailable');
      // Copy the markdown SOURCE, not the rendered text: bullets and numbering
      // survive pasting into notes that way.
      await clipboard.writeText(source);
      setCopied(true);
      // §9: Copy "announces to screen readers". Wiped after a beat so the next
      // copy writes a real DOM change (see useAnnouncer).
      announce('Answer copied to clipboard');
    } catch {
      onCopyError({ code: 'internal', message: 'Could not copy the answer to the clipboard.' });
    }
  }

  const metrics = viewed?.metrics ?? null;
  const status = viewed?.status ?? 'pending';
  const statusTag = STATUS_TAG[status];
  const reason = viewed?.reason ?? null;

  return (
    <section className="panel answer-panel">
      <div className="panel-head">
        <h2 className="panel-title">Suggested answer</h2>
        {streaming && <span className="tag">generating…</span>}
        {statusTag !== undefined && (
          <span className={`tag status-tag status-${status}`} title={reason ?? undefined}>
            {statusTag}
          </span>
        )}
        {metrics != null && (
          <span className="chip latency-chip" title={latencyTitle(metrics)}>
            {formatLatency(metrics.firstTokenMs)} to first word
          </span>
        )}
        <span className="panel-actions">
          <HistoryBar
            historyLength={historyLength}
            viewIndex={viewIndex}
            idle={idle}
            onPrev={onPrev}
            onNext={onNext}
            onClear={onClear}
          />
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
      {/* The reason travels with the entry, so it is still here when the user
          pages back to it; timing stays in the latency chip, never here. */}
      {statusTag !== undefined && reason !== null && (
        <p className={`answer-caption status-${status}`}>{reason}</p>
      )}
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

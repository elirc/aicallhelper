import type { HistoryEntry } from '../state/useSession';

interface HistoryBarProps {
  history: HistoryEntry[];
  viewIndex: number;
  /** Clearing mid-session would delete the entry currently being written. */
  idle: boolean;
  onPrev(): void;
  onNext(): void;
  onClear(): void;
}

/** Prev/next navigation over past answers. */
export function HistoryBar({ history, viewIndex, idle, onPrev, onNext, onClear }: HistoryBarProps) {
  // With one entry there is nothing to navigate to; the bar only earns its
  // vertical space in a 700px window once a second entry exists.
  if (history.length < 2) return null;
  return (
    <nav className="history-bar" aria-label="Answer history">
      <button
        type="button"
        className="ghost-button history-arrow"
        aria-label="Previous answer"
        disabled={viewIndex <= 0}
        onClick={onPrev}
      >
        ‹
      </button>
      <span className="history-label">
        {viewIndex + 1}/{history.length}
      </span>
      <button
        type="button"
        className="ghost-button history-arrow"
        aria-label="Next answer"
        disabled={viewIndex >= history.length - 1}
        onClick={onNext}
      >
        ›
      </button>
      <button type="button" className="ghost-button history-clear" disabled={!idle} onClick={onClear}>
        Clear
      </button>
    </nav>
  );
}

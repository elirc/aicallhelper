interface HistoryBarProps {
  /**
   * A count, not the array: the bar lives inside the memo'd answer panel,
   * and the history array's identity changes on every streamed token.
   */
  historyLength: number;
  viewIndex: number;
  /** Clearing mid-session would delete the entry currently being written. */
  idle: boolean;
  onPrev(): void;
  onNext(): void;
  onClear(): void;
}

/** Prev/next navigation over past answers. */
export function HistoryBar({ historyLength, viewIndex, idle, onPrev, onNext, onClear }: HistoryBarProps) {
  // With one entry there is nothing to navigate to; the bar only earns its
  // space in the panel head once a second entry exists.
  if (historyLength < 2) return null;
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
        {viewIndex + 1}/{historyLength}
      </span>
      <button
        type="button"
        className="ghost-button history-arrow"
        aria-label="Next answer"
        disabled={viewIndex >= historyLength - 1}
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

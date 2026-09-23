import { memo, useEffect, useState } from 'react';

interface TranscriptPanelProps {
  /** The viewed entry's question — live during recording, final afterwards. */
  question: string;
  /** The viewed entry is the one the STT is feeding right now (the "live" tag). */
  recording: boolean;
  /**
   * starting | recording | finalizing: the transcript is the thing to watch,
   * so the strip stays open regardless of what the user toggled last time.
   */
  active: boolean;
  /** Focus mode keeps the panel mounted but out of the layout and the a11y tree. */
  hidden?: boolean;
}

/**
 * "Question heard": the live transcript so the user can see the STT keeping
 * up. Demoted to a compact strip under the controls (§9): it is a confidence
 * signal while recording, not something to read while answering, so once the
 * question is final it auto-collapses to a one-line caption and gives the
 * height back to the answer.
 */
export const TranscriptPanel = memo(function TranscriptPanel({ question, recording, active, hidden = false }: TranscriptPanelProps) {
  // null = follow the auto rule; a boolean = the user's explicit choice.
  const [manual, setManual] = useState<boolean | null>(null);

  // Every new recording starts a fresh auto cycle: a strip the user opened
  // to re-read the last question must still collapse after the next one.
  useEffect(() => {
    if (active) setManual(null);
  }, [active]);

  // Idle with nothing heard yet stays open so the pinned placeholder is
  // visible — a collapsed empty strip would hide the one line that explains
  // what this panel is for.
  const auto = active || question === '';
  const expanded = manual ?? auto;

  return (
    <section className="panel transcript-panel" hidden={hidden}>
      {/* Standard disclosure: the <button> sits INSIDE the <h2>, never the
          other way round. A button permits only phrasing content and its
          descendants are presentational, so a heading nested in it drops out
          of a screen reader's heading list and everything else in the strip
          (the caption included — up to MAX_ASK_CHARS) becomes the button's
          name, read on every focus. The whole strip still acts as one click
          target through the toggle's ::after overlay. */}
      <div className="panel-head transcript-head">
        <h2 className="panel-title">
          <button
            type="button"
            className="transcript-toggle"
            aria-expanded={expanded}
            aria-controls="transcript-body"
            onClick={() => setManual(!expanded)}
          >
            Question heard
          </button>
        </h2>
        {/* The tag distinguishes "this text is still moving" from a finished
            transcript that merely looks short. */}
        {recording && <span className="tag tag-live">live</span>}
        {/* Left in the a11y tree on purpose: while collapsed this is the only
            copy of the question in the DOM, and as a sibling of the heading it
            reads as plain text, not as part of the button's name. */}
        {!expanded && question !== '' && <span className="transcript-caption">{question}</span>}
        <span className="transcript-chevron" aria-hidden="true">{expanded ? '▾' : '▸'}</span>
      </div>
      {/* Collapsed: hidden AND empty, so the question text exists exactly once
          in the DOM (in the caption) rather than once visible, once hidden. */}
      <div id="transcript-body" className="panel-body transcript-body" hidden={!expanded}>
        {!expanded ? null : question !== '' ? (
          <p className="transcript-text">{question}</p>
        ) : recording ? (
          // Mic open but nothing decoded yet — silence here would read as a
          // broken pipeline rather than a quiet speaker.
          <p className="placeholder">Listening…</p>
        ) : (
          <p className="placeholder">The live transcript will appear here while you record.</p>
        )}
      </div>
    </section>
  );
});

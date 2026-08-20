interface TranscriptPanelProps {
  /** The viewed entry's question — live during recording, final afterwards. */
  question: string;
  recording: boolean;
}

/** "Question heard": the live transcript so the user can see the STT keeping up. */
export function TranscriptPanel({ question, recording }: TranscriptPanelProps) {
  return (
    <section className="panel transcript-panel">
      <div className="panel-head">
        <h2 className="panel-title">Question heard</h2>
        {/* The tag distinguishes "this text is still moving" from a finished
            transcript that merely looks short. */}
        {recording && <span className="tag tag-live">live</span>}
      </div>
      <div className="panel-body transcript-body">
        {question !== '' ? (
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
}

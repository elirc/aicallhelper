import { useEffect, useRef, useState } from 'react';
import type { RefObject } from 'react';
import type { AnswerStyle, Envelope, SettingsView as Settings } from '../types';
import type { SessionApi } from '../state/useSession';
import { formatDuration, formatHotkey } from '../format';
import type { HotkeyStatus } from '../components/hotkey';
import { RecordButton } from '../components/RecordButton';
import { StatusLine } from '../components/StatusLine';
import { LevelMeter } from '../components/LevelMeter';
import { AskForm } from '../components/AskForm';
import { StyleChips } from '../components/StyleChips';
import { TranscriptPanel } from '../components/TranscriptPanel';
import { AnswerPanel } from '../components/AnswerPanel';
import { ErrorBox } from '../components/ErrorBox';
import { HistoryBar } from '../components/HistoryBar';

interface MainViewProps {
  session: SessionApi;
  /** null until the first getSettings resolves. */
  settings: Settings | null;
  hotkey: HotkeyStatus | null;
  onOpenSettings(): void;
  /** Persists the style; the caller updates `settings` from the save's response. */
  onSelectStyle(style: AnswerStyle): Promise<Envelope<Settings>>;
  /** Owned by App so it can restore focus here when Settings closes. */
  gearRef: RefObject<HTMLButtonElement>;
}

/**
 * How long an announcement stays in the live region before it is wiped. The
 * wipe is what makes REPEATED announcements audible: setting an identical
 * string twice is a state update React bails out of, so no DOM mutation
 * reaches the screen reader the second time.
 */
const ANNOUNCEMENT_CLEAR_MS = 1500;

export function MainView({ session, settings, hotkey, onOpenSettings, onSelectStyle, gearRef }: MainViewProps) {
  const recordRef = useRef<HTMLButtonElement>(null);
  const [announcement, setAnnouncement] = useState('');
  const announcementTimerRef = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (announcementTimerRef.current != null) window.clearTimeout(announcementTimerRef.current);
    },
    []
  );

  // "Done" is a transition, not a state: idle-after-answering with no error.
  // Tracked here because the hook's idle cannot distinguish fresh from done.
  const [justCompleted, setJustCompleted] = useState(false);
  const prevStateRef = useRef(session.state);
  useEffect(() => {
    const prev = prevStateRef.current;
    prevStateRef.current = session.state;
    // llm:done can land straight from finalizing — a non-streaming provider
    // never passes through answering — so both transitions count as done.
    if ((prev === 'answering' || prev === 'finalizing') && session.state === 'idle' && session.error == null) {
      setJustCompleted(true);
    } else if (session.state !== 'idle') {
      setJustCompleted(false);
    }
  }, [session.state, session.error]);

  const recording = session.state === 'recording';

  // The streaming decorations belong to the entry receiving tokens — the
  // newest one — not to an older entry the user may have navigated back to.
  const lastKey = session.history[session.history.length - 1]?.key;
  const viewingLive = session.viewed != null && session.viewed.key === lastKey;
  const streaming = session.state === 'answering' && viewingLive;

  // First-run nudge keys off the SELECTED provider: a missing Groq key is not
  // a problem while the Anthropic preset is chosen.
  const firstRun =
    settings != null &&
    (!settings.hasDeepgramKey ||
      (settings.llmProvider === 'anthropic' && !settings.hasAnthropicKey) ||
      (settings.llmProvider === 'groq' && !settings.hasGroqKey));

  const canRegenerate =
    (session.viewed?.question ?? '') !== '' && (session.state === 'idle' || session.state === 'answering');

  async function selectStyle(style: AnswerStyle) {
    const env = await onSelectStyle(style);
    // A silent failure would leave the user believing the chip they clicked;
    // the unchanged pressed state plus this error tells the real story.
    if (!env.ok) session.setError(env.error);
  }

  function clearHistory() {
    session.clearHistory();
    // §9 pins the "History cleared" announcement. It must be wiped afterwards:
    // left in place, a second Clear would set the identical string, React
    // would skip the DOM write, and the screen reader would hear nothing.
    setAnnouncement('History cleared');
    if (announcementTimerRef.current != null) window.clearTimeout(announcementTimerRef.current);
    announcementTimerRef.current = window.setTimeout(() => setAnnouncement(''), ANNOUNCEMENT_CLEAR_MS);
    // The Clear button unmounts with its bar; without an explicit target,
    // focus falls to <body> and keyboard users lose their place.
    recordRef.current?.focus();
  }

  return (
    <div className="app main-view">
      <header className="app-header">
        <span className={`status-dot status-dot--${session.state}`} aria-hidden="true" />
        <h1 className="app-title">AI Call Assistant</h1>
        <button
          ref={gearRef}
          type="button"
          className="icon-button"
          aria-label="Settings"
          // Settings edits are patches over loaded values; opening before the
          // load resolves would risk saving a form built from nothing.
          disabled={settings == null}
          onClick={onOpenSettings}
        >
          <span aria-hidden="true">⚙</span>
        </button>
      </header>

      <StatusLine
        state={session.state}
        hitRecordingCap={session.hitRecordingCap}
        justCompleted={justCompleted}
        firstRun={firstRun}
        hotkey={hotkey}
      />

      <RecordButton ref={recordRef} state={session.state} hotkey={hotkey} onToggle={session.toggleRecord} />

      {/* §9 pins this notice for a hotkey TAKEN by another app. An empty
          accelerator also reports registered:false, but that is the user
          DISABLING the shortcut (§8: empty = disabled) — accusing another app
          of stealing a nameless key would be a false claim. */}
      {hotkey != null && !hotkey.registered && hotkey.accelerator.trim() !== '' && (
        <p className="hotkey-notice">
          {formatHotkey(hotkey.accelerator)} is already taken by another app, so the shortcut is off — record from
          this window, or pick a different one in Settings.
        </p>
      )}

      {recording && (
        <div className="recording-row">
          <LevelMeter rms={session.rms} />
          <span className="timer">{formatDuration(session.elapsedMs)}</span>
        </div>
      )}

      <AskForm
        disabled={session.state === 'starting' || session.state === 'recording' || session.state === 'finalizing'}
        onAsk={session.submitAsk}
      />

      <StyleChips value={settings?.answerStyle ?? 'balanced'} onSelect={selectStyle} />

      {/* Like the answer panel's streaming decorations: the "live" tag belongs
          to the entry the STT is feeding, not to an older entry the user
          navigated back to mid-recording — that one is finished text wearing a
          pulsing tag while the real live transcript updates out of view. */}
      <TranscriptPanel question={session.viewed?.question ?? ''} recording={recording && viewingLive} />

      <AnswerPanel
        viewed={session.viewed}
        streaming={streaming}
        canRegenerate={canRegenerate}
        onRegenerate={session.regenerate}
        onCopyError={session.setError}
      />

      <ErrorBox error={session.error} />

      <HistoryBar
        history={session.history}
        viewIndex={session.viewIndex}
        idle={session.state === 'idle'}
        onPrev={session.viewPrev}
        onNext={session.viewNext}
        onClear={clearHistory}
      />

      {/* Off-view live region for announcements whose visual anchor unmounts
          (e.g. "History cleared" — the bar it lived on is gone). Permanently
          mounted with only its TEXT swapped: several SR/browser combos ignore
          a live region that is inserted together with its content — only a
          content change inside a pre-existing region is announced. */}
      <span role="status" className="sr-only">
        {announcement}
      </span>
    </div>
  );
}

import { useCallback, useEffect, useRef, useState } from 'react';
import type { RefObject } from 'react';
import { hasRequiredKeys } from '../types';
import type { AnswerStyle, Envelope, HotkeyStatus, SettingsView as Settings } from '../types';
import type { SessionApi } from '../state/useSession';
import { formatDuration, formatHotkey } from '../format';
import { RecordButton } from '../components/RecordButton';
import { StatusLine } from '../components/StatusLine';
import { LevelMeter } from '../components/LevelMeter';
import { AskForm } from '../components/AskForm';
import { StyleChips } from '../components/StyleChips';
import { ProfileChips } from '../components/ProfileChips';
import { TranscriptPanel } from '../components/TranscriptPanel';
import { AnswerPanel } from '../components/AnswerPanel';
import { ErrorBox } from '../components/ErrorBox';
import { useAnnouncer } from '../components/useTransient';

interface MainViewProps {
  session: SessionApi;
  /** null until the first getSettings resolves. */
  settings: Settings | null;
  hotkey: HotkeyStatus | null;
  onOpenSettings(): void;
  onPrepareSettings?(): void;
  /** Persists the style; the caller updates `settings` from the save's response. */
  onSelectStyle(style: AnswerStyle): Promise<Envelope<Settings>>;
  /** Persists ONLY the active id (§8); the caller updates `settings` from the response. */
  onSelectProfile(id: string): Promise<Envelope<Settings>>;
  /** Moves the window under the webcam; a refusal is surfaced by the caller. */
  onDock(): Promise<void> | void;
  /** Answer-only posture: rows the eyes don't need mid-call are hidden. */
  focusMode: boolean;
  onToggleFocus(): void;
  /**
   * Typed Ask. App passes a wrapper that checks the free-local byte budget
   * first (R4); without one, the question goes straight to the session.
   */
  onAsk?(text: string): Promise<boolean>;
  /** Owned by App so it can restore focus here when Settings closes. */
  gearRef: RefObject<HTMLButtonElement>;
}

/** The in-window focus toggle is a plain keydown, NOT a global shortcut (§9). */
function isFocusShortcut(e: KeyboardEvent): boolean {
  return e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey && (e.code === 'KeyF' || e.key.toLowerCase() === 'f');
}

function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || target.isContentEditable === true;
}

export function MainView({
  session,
  settings,
  hotkey,
  onOpenSettings,
  onPrepareSettings,
  onSelectStyle,
  onSelectProfile,
  onDock,
  focusMode,
  onToggleFocus,
  onAsk,
  gearRef,
}: MainViewProps) {
  const recordRef = useRef<HTMLButtonElement>(null);
  // Wiped after a beat so a REPEATED announcement is a real DOM change the
  // screen reader hears (see useAnnouncer).
  const [announcement, announce] = useAnnouncer();

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

  // Ctrl+Shift+F toggles focus mode while this window has focus. Read through
  // a ref so the listener is attached once; skipped inside text fields where
  // the chord could be a legitimate edit (e.g. a hotkey being typed).
  const toggleFocusRef = useRef(onToggleFocus);
  toggleFocusRef.current = onToggleFocus;
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (!isFocusShortcut(e) || isEditable(e.target)) return;
      e.preventDefault();
      toggleFocusRef.current();
    }
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const recording = session.state === 'recording';
  const active = session.state === 'starting' || session.state === 'recording' || session.state === 'finalizing';

  // The streaming decorations belong to the entry receiving tokens — the
  // newest one — not to an older entry the user may have navigated back to.
  const lastKey = session.history[session.history.length - 1]?.key;
  const viewingLive = session.viewed != null && session.viewed.key === lastKey;
  const streaming = session.state === 'answering' && viewingLive;

  // First-run nudge keys off the SELECTED provider (a missing Groq key is not
  // a problem while the Anthropic preset is chosen); null until settings
  // load so the status line renders nothing rather than a wrong first frame.
  const firstRun = settings == null ? null : !hasRequiredKeys(settings);

  // The core guarantees activeProfileId names an entry; the first-entry
  // fallback only covers a view assembled by hand (tests) or a stale cache.
  const activeProfile =
    settings?.profiles.find((p) => p.id === settings.activeProfileId) ?? settings?.profiles[0] ?? null;
  const ungrounded =
    settings != null &&
    activeProfile != null &&
    activeProfile.resume.trim() === '' &&
    activeProfile.jobDescription.trim() === '' &&
    hasRequiredKeys(settings);

  const canRegenerate =
    (session.viewed?.question ?? '') !== '' && (session.state === 'idle' || session.state === 'answering');

  const selectStyle = useCallback(
    async (style: AnswerStyle) => {
      const env = await onSelectStyle(style);
      // A silent failure would leave the user believing the chip they clicked;
      // the unchanged pressed state plus this error tells the real story.
      if (!env.ok) session.setError(env.error);
    },
    [onSelectStyle, session.setError]
  );

  const selectProfile = useCallback(
    async (id: string) => {
      const env = await onSelectProfile(id);
      if (!env.ok) session.setError(env.error);
    },
    [onSelectProfile, session.setError]
  );

  const clearHistory = useCallback(() => {
    session.clearHistory();
    // §9 pins the "History cleared" announcement.
    announce('History cleared');
    // The Clear button unmounts with its bar; without an explicit target,
    // focus falls to <body> and keyboard users lose their place.
    recordRef.current?.focus();
  }, [session.clearHistory, announce]);

  return (
    <div className={`app main-view${focusMode ? ' main-view--focus' : ''}`}>
      <header className="app-header">
        <span className={`status-dot status-dot--${session.state}`} aria-hidden="true" />
        <h1 className="app-title">AI Call Assistant</h1>
        <button
          type="button"
          className="icon-button"
          aria-label="Focus mode"
          aria-pressed={focusMode}
          title="Show only the answer (Ctrl+Shift+F)"
          onClick={onToggleFocus}
        >
          <span aria-hidden="true">◎</span>
        </button>
        {/* A window operation, not a settings edit: live before settings load. */}
        <button
          type="button"
          className="icon-button"
          aria-label="Dock to camera"
          title="Move this window to the top of the screen, under the webcam"
          onClick={() => void onDock()}
        >
          <span aria-hidden="true">⬆</span>
        </button>
        <button
          ref={gearRef}
          type="button"
          className="icon-button"
          aria-label="Settings"
          // Settings edits are patches over loaded values; opening before the
          // load resolves would risk saving a form built from nothing.
          disabled={settings == null}
          onClick={onOpenSettings}
          onPointerEnter={onPrepareSettings}
          onFocus={onPrepareSettings}
        >
          <span aria-hidden="true">⚙</span>
        </button>
      </header>

      <ProfileChips
        profiles={settings?.profiles ?? []}
        activeId={settings?.activeProfileId ?? ''}
        onSelect={selectProfile}
        hidden={focusMode}
      />

      {/* Answer first: the output sits at the top of the window, which the
          dock action puts directly under the webcam. */}
      <AnswerPanel
        viewed={session.viewed}
        streaming={streaming}
        canRegenerate={canRegenerate}
        onRegenerate={session.regenerate}
        onCopyError={session.setError}
        historyLength={session.history.length}
        viewIndex={session.viewIndex}
        idle={session.state === 'idle'}
        onPrev={session.viewPrev}
        onNext={session.viewNext}
        onClear={clearHistory}
        streamFollow={settings?.streamFollow ?? 'tail'}
      />

      {/* Next to the output it interrupts, not four rows below it. */}
      <ErrorBox error={session.error} />

      <StatusLine
        state={session.state}
        hitRecordingCap={session.hitRecordingCap}
        justCompleted={justCompleted}
        firstRun={firstRun}
        hotkey={hotkey}
      />

      <div className="control-row">
        <RecordButton ref={recordRef} state={session.state} hotkey={hotkey} onToggle={session.toggleRecord} />
        <StyleChips value={settings?.answerStyle ?? 'balanced'} onSelect={selectStyle} />
      </div>

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

      {/* Always rendered with a reserved height: the meter appearing must not
          push the answer the reader is mid-sentence in. */}
      <div className="recording-row">
        {recording && (
          <>
            <LevelMeter rms={session.rms} />
            <span className="timer">{formatDuration(session.elapsedMs)}</span>
          </>
        )}
      </div>

      <div className="ask-row" hidden={focusMode}>
        <AskForm
          disabled={active}
          onAsk={onAsk ?? session.submitAsk}
          showPrep={activeProfile == null || activeProfile.callType === 'interview'}
        />
        {session.history.length === 0 && hotkey?.registered === true && (
          <p className="field-help hint">
            Tip: press {formatHotkey(hotkey.accelerator)} from the meeting window — the answer streams here.
          </p>
        )}
        {ungrounded && activeProfile != null && (
          <p className="field-help hint">
            No resume or job description saved for {activeProfile.name} — answers won't be grounded. Add them in
            Settings.
          </p>
        )}
      </div>

      {settings?.llmProvider === 'local' && (
        <aside className="local-mode-banner" aria-label="Current mode" hidden={focusMode}>
          <strong>Free local voice · no API fees</strong>
          <p>English system audio · CPU answers may take longer.</p>
          <button type="button" className="ghost-button" onClick={onOpenSettings}>Local setup</button>
        </aside>
      )}

      {/* Like the answer panel's streaming decorations: the "live" tag belongs
          to the entry the STT is feeding, not to an older entry the user
          navigated back to mid-recording — that one is finished text wearing a
          pulsing tag while the real live transcript updates out of view. */}
      <TranscriptPanel
        question={session.viewed?.question ?? ''}
        recording={recording && viewingLive}
        active={active}
        hidden={focusMode}
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

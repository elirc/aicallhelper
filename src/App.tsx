import { lazy, startTransition, Suspense, useCallback, useEffect, useRef, useState } from 'react';
import type { AnswerStyle, Envelope, HotkeyStatus, SettingsPatch, SettingsView as Settings } from './types';
import { getBridge } from './bridge';
import { useSession } from './state/useSession';
import { MainView } from './views/MainView';
import { DeferredView } from './components/DeferredView';

const loadSettingsView = () => import('./views/SettingsView');
const SettingsView = lazy(() => loadSettingsView().then((module) => ({ default: module.SettingsView })));
// Same module AskForm lazy-loads: Rollup emits one chunk per module however
// many import() sites name it, so this warms exactly the chunk that click uses.
const loadPracticeLibrary = () => import('./components/PracticeLibrary');
/**
 * FE-3: warm both lazy chunks once first paint and the settings load are out
 * of the way, so the first click on the gear or on Interview prep never waits
 * on disk. 1.5 s is past the launch-critical window on the slowest tested
 * machine; earlier and the fetch competes with the main chunk's own parse.
 */
const PRELOAD_DELAY_MS = 1500;

const fmtBytes = (n: number) => n.toLocaleString('en-US');

/** The typed-Ask refusal: how far over, and where to shorten (R4). */
export function overLimitMessage(overBy: number, limit: number): string {
  return `This question is ${fmtBytes(overBy)} bytes too long for free local mode, which allows ${fmtBytes(limit)} bytes for the instructions, the active profile and the question together. Shorten the question or the active profile in Settings, or use a cloud model.`;
}

/**
 * Owns the two-view navigation, the loaded settings, and the hotkey gate. The
 * session hook lives here so a trip into Settings does not unmount an
 * in-flight recording or the answer history.
 */
export default function App() {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const settingsOpenRef = useRef(settingsOpen);
  settingsOpenRef.current = settingsOpen;

  // §9: the global shortcut is ignored while Settings is open — the user may
  // be typing the accelerator itself. The hook checks the ref per event, so
  // no bridge wrapping is needed and the subscription never churns.
  const session = useSession({ hotkeyEnabled: () => !settingsOpenRef.current });

  const [settings, setSettings] = useState<Settings | null>(null);
  const [hotkey, setHotkey] = useState<HotkeyStatus | null>(null);
  // Not persisted on purpose: focus mode is a per-call posture, and a window
  // that opens with its controls hidden reads as broken on the next launch.
  const [focusMode, setFocusMode] = useState(false);

  const setErrorRef = useRef(session.setError);
  setErrorRef.current = session.setError;

  const gearRef = useRef<HTMLButtonElement>(null);
  const wasOpenRef = useRef(false);

  /**
   * Install a settings view only if it is at least as new as the one held
   * (R5, ADR 016). Responses to overlapping saves can arrive in any order;
   * the functional update compares against the state React actually holds
   * at apply time, not a value captured when the request was sent, so two
   * handlers racing each other cannot reinstall an older view.
   */
  const applyView = useCallback((view: Settings) => {
    setSettings((held) => (held == null || view.revision >= held.revision ? view : held));
  }, []);

  useEffect(() => {
    let alive = true;
    const bridge = getBridge();
    void bridge.getSettings().then((env) => {
      if (!alive) return;
      if (env.ok) {
        applyView(env.value);
        // A quarantined or unreadable settings file is reported where the
        // user is looking at launch; Settings keeps its own banner.
        if (env.value.storageWarning != null) {
          setErrorRef.current({ code: 'internal', message: env.value.storageWarning });
        }
      }
      // Without loaded settings the gear stays disabled — surface why.
      else setErrorRef.current(env.error);
    });
    void bridge.hotkeyStatus().then((env) => {
      if (alive && env.ok) setHotkey(env.value);
    });
    return () => {
      alive = false;
    };
  }, [applyView]);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      // Failures are deliberately swallowed: the real load on click reports
      // through DeferredView, and a preload has no user to tell.
      void loadSettingsView().catch(() => undefined);
      void loadPracticeLibrary().catch(() => undefined);
    }, PRELOAD_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, []);

  useEffect(() => {
    // Focus returns to the gear that opened Settings; without this it lands
    // on <body> and a keyboard user restarts from the top of the window.
    if (wasOpenRef.current && !settingsOpen) gearRef.current?.focus();
    wasOpenRef.current = settingsOpen;
  }, [settingsOpen]);

  const hotkeyRef = useRef(hotkey);
  hotkeyRef.current = hotkey;

  const applySettings = useCallback(async (patch: SettingsPatch): Promise<Envelope<Settings>> => {
    const env = await getBridge().setSettings(patch);
    if (env.ok) {
      applyView(env.value);
      // A saved hotkey may have gained or lost its OS registration; the chip
      // and the taken-notice must reflect the new reality, not the old one.
      // A refused hotkey is retried by every save's effect step (ADR 016),
      // so while one is refused any save may be the one that registers it.
      const held = hotkeyRef.current;
      const refused = held != null && !held.registered && held.accelerator.trim() !== '';
      if (patch.hotkey !== undefined || refused) {
        const hk = await getBridge().hotkeyStatus();
        if (hk.ok) setHotkey(hk.value);
      }
    }
    return env;
  }, [applyView]);

  /**
   * The Settings form's "reload" after a conflict (R5): fetch the committed
   * view, install it through the same revision guard, and hand it to the
   * form, which rebases its unsaved edits onto it.
   */
  const reloadSettings = useCallback(async (): Promise<Envelope<Settings>> => {
    const env = await getBridge().getSettings();
    if (env.ok) applyView(env.value);
    return env;
  }, [applyView]);

  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const submitAsk = session.submitAsk;

  /**
   * Typed Ask in free local mode checks the ACTUAL question against the
   * byte limit before anything is sent or superseded (R4), so a question
   * that cannot fit keeps the current answer and stays in the box. The core
   * repeats the check authoritatively; a failed budget call just defers to it.
   */
  const askChecked = useCallback(
    async (text: string): Promise<boolean> => {
      const s = settingsRef.current;
      const profile = s?.profiles.find((p) => p.id === s.activeProfileId) ?? s?.profiles[0];
      if (s != null && s.llmProvider === 'local' && profile !== undefined) {
        const env = await getBridge().localPromptBudget(profile, s.answerStyle, text);
        if (env.ok && env.value.status === 'over') {
          setErrorRef.current({ code: 'llm_http', message: overLimitMessage(-env.value.remainingBytes, env.value.limitBytes) });
          return false;
        }
      }
      return submitAsk(text);
    },
    [submitAsk]
  );

  const selectStyle = useCallback(
    (style: AnswerStyle) => applySettings({ answerStyle: style }),
    [applySettings]
  );

  // §8: a switch sends ONLY the id — profile text is never rewritten by the
  // main view, so a stale in-memory copy can never clobber a saved edit.
  const selectProfile = useCallback(
    (id: string) => applySettings({ activeProfileId: id }),
    [applySettings]
  );

  const dockToCamera = useCallback(async () => {
    const env = await getBridge().dockToCamera();
    // A refused dock (no display found) is real information; the error box
    // is where every other core refusal lands.
    if (!env.ok) setErrorRef.current(env.error);
  }, []);

  const toggleFocus = useCallback(() => setFocusMode((on) => !on), []);

  // Opening Settings swaps the whole tree for a lazy chunk; as a transition
  // the main view stays interactive (and the stream keeps painting) instead
  // of the input being blocked behind the mount (FE-3).
  const openSettings = useCallback(() => {
    startTransition(() => setSettingsOpen(true));
  }, []);
  const closeSettings = useCallback(() => setSettingsOpen(false), []);
  const prepareSettings = useCallback(() => { void loadSettingsView().catch(() => undefined); }, []);

  if (settingsOpen && settings != null) {
    return (
      <DeferredView onClose={closeSettings}>
        <Suspense fallback={
          <div className="app">
            <p role="status">Loading settings…</p>
            <button type="button" className="ghost-button" onClick={closeSettings}>Back to assistant</button>
          </div>
        }>
          <SettingsView settings={settings} onSave={applySettings} onReload={reloadSettings} onBack={closeSettings} />
        </Suspense>
      </DeferredView>
    );
  }

  return (
    <MainView
      session={session}
      settings={settings}
      hotkey={hotkey}
      gearRef={gearRef}
      focusMode={focusMode}
      onOpenSettings={openSettings}
      onPrepareSettings={prepareSettings}
      onSelectStyle={selectStyle}
      onSelectProfile={selectProfile}
      onDock={dockToCamera}
      onToggleFocus={toggleFocus}
      onAsk={askChecked}
    />
  );
}

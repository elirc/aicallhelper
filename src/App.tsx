import { useCallback, useEffect, useRef, useState } from 'react';
import type { MutableRefObject } from 'react';
import type { AnswerStyle, Envelope, EventMap, EventName, SettingsPatch, SettingsView as Settings } from './types';
import { getBridge, setBridge } from './bridge';
import type { Bridge } from './bridge';
import { useSession } from './state/useSession';
import type { HotkeyStatus } from './components/hotkey';
import { MainView } from './views/MainView';
import { SettingsView } from './views/SettingsView';

/** Bridges that are already hotkey-gated; re-wrapping would stack filters. */
const gatedBridges = new WeakSet<Bridge>();

/**
 * useSession subscribes to `hotkey:toggle` itself, so the only way to honor
 * "the hotkey is IGNORED while Settings is open — the user may be typing the
 * accelerator itself" is to filter the event before the hook ever sees it.
 * This wraps the current bridge so `hotkey:toggle` is dropped while the flag
 * is set; every other call and event passes through untouched.
 *
 * Declared BEFORE useSession in App so this effect runs first on mount and the
 * hook's subscription lands on the gated bridge.
 */
function useHotkeyGate(settingsOpenRef: MutableRefObject<boolean>) {
  useEffect(() => {
    const inner = getBridge();
    if (gatedBridges.has(inner)) return;
    const gated: Bridge = {
      getSettings: () => inner.getSettings(),
      setSettings: (patch) => inner.setSettings(patch),
      startSession: () => inner.startSession(),
      stopSession: (id) => inner.stopSession(id),
      ask: (text) => inner.ask(text),
      cancelSession: (id) => inner.cancelSession(id),
      hotkeyStatus: () => inner.hotkeyStatus(),
      on: <K extends EventName>(name: K, handler: (payload: EventMap[K]) => void): (() => void) => {
        if (name !== 'hotkey:toggle') return inner.on(name, handler);
        return inner.on(name, (payload) => {
          if (!settingsOpenRef.current) handler(payload);
        });
      },
    };
    gatedBridges.add(gated);
    setBridge(gated);
  }, [settingsOpenRef]);
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

  // Must run before useSession (see useHotkeyGate).
  useHotkeyGate(settingsOpenRef);
  const session = useSession();

  const [settings, setSettings] = useState<Settings | null>(null);
  const [hotkey, setHotkey] = useState<HotkeyStatus | null>(null);

  const setErrorRef = useRef(session.setError);
  setErrorRef.current = session.setError;

  const gearRef = useRef<HTMLButtonElement>(null);
  const wasOpenRef = useRef(false);

  useEffect(() => {
    let alive = true;
    const bridge = getBridge();
    void bridge.getSettings().then((env) => {
      if (!alive) return;
      if (env.ok) setSettings(env.value);
      // Without loaded settings the gear stays disabled — surface why.
      else setErrorRef.current(env.error);
    });
    void bridge.hotkeyStatus().then((env) => {
      if (alive && env.ok) setHotkey(env.value);
    });
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    // Focus returns to the gear that opened Settings; without this it lands
    // on <body> and a keyboard user restarts from the top of the window.
    if (wasOpenRef.current && !settingsOpen) gearRef.current?.focus();
    wasOpenRef.current = settingsOpen;
  }, [settingsOpen]);

  const applySettings = useCallback(async (patch: SettingsPatch): Promise<Envelope<Settings>> => {
    const env = await getBridge().setSettings(patch);
    if (env.ok) {
      setSettings(env.value);
      // A saved hotkey may have gained or lost its OS registration; the chip
      // and the taken-notice must reflect the new reality, not the old one.
      const hk = await getBridge().hotkeyStatus();
      if (hk.ok) setHotkey(hk.value);
    }
    return env;
  }, []);

  const selectStyle = useCallback(
    (style: AnswerStyle) => applySettings({ answerStyle: style }),
    [applySettings]
  );

  if (settingsOpen && settings != null) {
    return <SettingsView settings={settings} onSave={applySettings} onBack={() => setSettingsOpen(false)} />;
  }

  return (
    <MainView
      session={session}
      settings={settings}
      hotkey={hotkey}
      gearRef={gearRef}
      onOpenSettings={() => setSettingsOpen(true)}
      onSelectStyle={selectStyle}
    />
  );
}

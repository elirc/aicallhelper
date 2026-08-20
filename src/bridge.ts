/**
 * The single place the UI touches Tauri. Everything else depends on the
 * Bridge interface, so tests swap in a fake via setBridge and the state layer
 * never learns what "invoke" is.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type {
  Envelope,
  EventMap,
  EventName,
  SessionId,
  SettingsPatch,
  SettingsView,
} from './types';

export interface Bridge {
  getSettings(): Promise<Envelope<SettingsView>>;
  setSettings(patch: SettingsPatch): Promise<Envelope<SettingsView>>;
  startSession(): Promise<Envelope<SessionId>>;
  stopSession(id: SessionId): Promise<Envelope<null>>;
  ask(text: string): Promise<Envelope<SessionId>>;
  cancelSession(id: SessionId): void;
  hotkeyStatus(): Promise<Envelope<{ accelerator: string; registered: boolean }>>;
  on<K extends EventName>(name: K, handler: (payload: EventMap[K]) => void): () => void;
}

/**
 * A rejected invoke (IPC breakage, missing command) would otherwise be a
 * second error path every caller has to remember to catch; folding it into
 * the envelope keeps exactly one.
 */
async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<Envelope<T>> {
  try {
    return await invoke<Envelope<T>>(cmd, args);
  } catch (err) {
    return {
      ok: false,
      error: { code: 'internal', message: err instanceof Error ? err.message : String(err) },
    };
  }
}

function createTauriBridge(): Bridge {
  return {
    getSettings: () => call('get_settings'),
    setSettings: (patch) => call('set_settings', { patch }),
    startSession: () => call('start_session'),
    // Tauri maps a Rust `session_id` parameter to a camelCase `sessionId`
    // argument key; sending `{ id }` makes every stop fail with a missing-key
    // error the UI can only render as "internal".
    stopSession: (id) => call('stop_session', { sessionId: id }),
    ask: (text) => call('ask', { text }),
    cancelSession: (id) => {
      // Fire-and-forget: cancellation legitimately races session teardown,
      // so a rejection here is expected noise, not something to surface.
      void invoke('cancel_session', { sessionId: id }).catch(() => undefined);
    },
    hotkeyStatus: () => call('hotkey_status'),
    on: <K extends EventName>(name: K, handler: (payload: EventMap[K]) => void): (() => void) => {
      // listen() resolves asynchronously; unsubscribing before it settles
      // must still detach, or a torn-down component keeps receiving events.
      let disposed = false;
      let unlisten: (() => void) | null = null;
      void listen<EventMap[K]>(name, (event) => {
        if (!disposed) handler(event.payload);
      })
        .then((un) => {
          if (disposed) un();
          else unlisten = un;
        })
        .catch(() => undefined);
      return () => {
        disposed = true;
        if (unlisten !== null) unlisten();
      };
    },
  };
}

let current: Bridge | null = null;

export function getBridge(): Bridge {
  if (current === null) current = createTauriBridge();
  return current;
}

/** Test seam: install a fake so the state layer can be driven without Tauri. */
export function setBridge(b: Bridge): void {
  current = b;
}

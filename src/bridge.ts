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
  AnswerStyle,
  CallProfile,
  HotkeyStatus,
  LocalPromptBudget,
  LocalVoiceStatus,
  SessionId,
  SessionOutcome,
  SessionStart,
  SettingsPatch,
  SettingsView,
} from './types';

/**
 * One event subscription. `ready` settles once the listener is actually
 * registered with Tauri — as an ok envelope, or as an error envelope when
 * registration failed. Readiness is part of the session start contract (R1,
 * ADR 015): the state layer awaits it before starting anything, because an
 * event emitted before its listener exists is simply never delivered.
 */
export interface Subscription {
  ready: Promise<Envelope<null>>;
  unsubscribe(): void;
}

export interface Bridge {
  getSettings(): Promise<Envelope<SettingsView>>;
  setSettings(patch: SettingsPatch): Promise<Envelope<SettingsView>>;
  /** The new id plus its outcome as of the response (R1). */
  startSession(): Promise<Envelope<SessionStart>>;
  stopSession(id: SessionId): Promise<Envelope<null>>;
  /** The new id plus its outcome as of the response (R1). */
  ask(text: string): Promise<Envelope<SessionStart>>;
  /** Adoption-time reconciliation: how `id` ended, or `active` (R1). */
  sessionOutcome(id: SessionId): Promise<Envelope<SessionOutcome>>;
  cancelSession(id: SessionId): void;
  hotkeyStatus(): Promise<Envelope<HotkeyStatus>>;
  /** Move the window to the top-centre of its display — under the webcam (§9). */
  dockToCamera(): Promise<Envelope<null>>;
  /** Probe the local Ollama/Moonshine services without starting anything (§6.5). */
  localVoiceStatus(): Promise<Envelope<LocalVoiceStatus>>;
  /** Start the local services and warm the model; slow on a CPU (§6.5). */
  prepareLocalVoice(): Promise<Envelope<LocalVoiceStatus>>;
  /**
   * The exact local request size for an UNSAVED profile draft, style and
   * question (R4). `question` '' asks what is left for a question.
   */
  localPromptBudget(profile: CallProfile, answerStyle: AnswerStyle, question: string): Promise<Envelope<LocalPromptBudget>>;
  on<K extends EventName>(name: K, handler: (payload: EventMap[K]) => void): Subscription;
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
    // Same argument-key rule as stopSession.
    sessionOutcome: (id) => call('session_outcome', { sessionId: id }),
    cancelSession: (id) => {
      // Fire-and-forget: cancellation legitimately races session teardown,
      // so a rejection here is expected noise, not something to surface.
      void invoke('cancel_session', { sessionId: id }).catch(() => undefined);
    },
    hotkeyStatus: () => call('hotkey_status'),
    dockToCamera: () => call('dock_to_camera'),
    localVoiceStatus: () => call('local_voice_status'),
    prepareLocalVoice: () => call('prepare_local_voice'),
    // Tauri maps the Rust `answer_style` parameter to `answerStyle`.
    localPromptBudget: (profile, answerStyle, question) =>
      call('local_prompt_budget', { profile, answerStyle, question }),
    on: <K extends EventName>(name: K, handler: (payload: EventMap[K]) => void): Subscription => {
      // listen() resolves asynchronously; unsubscribing before it settles
      // must still detach, or a torn-down component keeps receiving events.
      let disposed = false;
      let unlisten: (() => void) | null = null;
      const ready = listen<EventMap[K]>(name, (event) => {
        if (!disposed) handler(event.payload);
      }).then(
        (un): Envelope<null> => {
          if (disposed) un();
          else unlisten = un;
          return { ok: true, value: null };
        },
        // A failed registration used to be swallowed here, leaving the UI
        // waiting on events that could never arrive. It is now an error the
        // state layer shows before any session starts (R1).
        (err: unknown): Envelope<null> => ({
          ok: false,
          error: {
            code: 'internal',
            message: `The app could not listen for ${name} events (${
              err instanceof Error ? err.message : String(err)
            }). Restart the app.`,
          },
        }),
      );
      return {
        ready,
        unsubscribe: () => {
          disposed = true;
          if (unlisten !== null) unlisten();
        },
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

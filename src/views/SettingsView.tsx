import { useCallback, useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import {
  CALL_TYPES,
  DEFAULT_HOTKEY,
  DEFAULT_PROFILE_ID,
  DEFAULT_PROFILE_NAME,
  MAX_EXTRA_INSTRUCTIONS_CHARS,
  MAX_FOCUS_CHARS,
  MAX_PROFILES,
  MAX_PROFILE_CHARS,
  MAX_PROFILE_NAME_CHARS,
  PROVIDERS,
  PROVIDER_ORDER,
  emptyProfile,
} from '../types';
import type {
  AnswerStyle,
  AppError,
  CallProfile,
  CallType,
  Envelope,
  LaunchPlacement,
  LlmProviderKind,
  LocalPromptBudget,
  SettingsPatch,
  SettingsView as Settings,
  StreamFollow,
} from '../types';
import { getBridge } from '../bridge';
import { ErrorBox } from '../components/ErrorBox';
import { LocalVoicePanel } from '../components/LocalVoicePanel';
import { useTransient } from '../components/useTransient';
import './SettingsView.css';

const SAVED_NOTE_MS = 1500;
const RELOADED_NOTE_MS = 4000;
/**
 * The local budget preview asks the core after typing pauses this long (R4):
 * a keystroke-rate IPC would rebuild a prompt that can hold two 200 000-char
 * fields on every key.
 */
const BUDGET_DEBOUNCE_MS = 250;

type KeyField = 'deepgramKey' | 'anthropicKey' | 'groqKey';
/** Absent = untouched = omitted from the patch; '' = typed then cleared = wipe. */
type KeyDrafts = Partial<Record<KeyField, string>>;

/**
 * One table for the three key inputs so ids/labels stay exactly as today
 * and "which keys does this provider need" is read from PROVIDERS, not from
 * scattered `=== 'local'` checks.
 */
const KEY_FIELDS: ReadonlyArray<{
  field: KeyField;
  id: string;
  label: string;
  has: 'hasDeepgramKey' | 'hasAnthropicKey' | 'hasGroqKey';
  needed(provider: LlmProviderKind): boolean;
}> = [
  {
    field: 'deepgramKey',
    id: 'set-deepgram',
    label: 'Deepgram API key',
    has: 'hasDeepgramKey',
    needed: (p) => PROVIDERS[p].usesDeepgram,
  },
  {
    field: 'anthropicKey',
    id: 'set-anthropic',
    label: 'Anthropic API key',
    has: 'hasAnthropicKey',
    needed: (p) => PROVIDERS[p].keyFlag === 'hasAnthropicKey',
  },
  {
    field: 'groqKey',
    id: 'set-groq',
    label: 'Groq API key (only for the Groq preset)',
    has: 'hasGroqKey',
    needed: (p) => PROVIDERS[p].keyFlag === 'hasGroqKey',
  },
];

/** Everything the form edits except the write-only keys. */
interface Draft {
  profiles: CallProfile[];
  /** Both "the profile being edited" and "the active profile on Save" (§9). */
  selectedId: string;
  alwaysOnTop: boolean;
  llmProvider: LlmProviderKind;
  answerStyle: AnswerStyle;
  hotkey: string;
  launchPlacement: LaunchPlacement;
  streamFollow: StreamFollow;
}

/**
 * Copies, never aliases, the profiles: the form is a proposal and the
 * loaded view stays the source of truth until a save returns.
 */
function seedDraft(s: Settings): Draft {
  const profiles =
    s.profiles.length > 0
      ? s.profiles.map((p) => ({ ...p }))
      : [emptyProfile(DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME)];
  const first = profiles[0];
  const selectedId = profiles.some((p) => p.id === s.activeProfileId) ? s.activeProfileId : (first?.id ?? '');
  return {
    profiles,
    selectedId,
    alwaysOnTop: s.alwaysOnTop,
    llmProvider: s.llmProvider,
    answerStyle: s.answerStyle,
    hotkey: s.hotkey,
    launchPlacement: s.launchPlacement,
    streamFollow: s.streamFollow,
  };
}

function sameProfile(a: CallProfile, b: CallProfile): boolean {
  return (
    a.id === b.id &&
    a.name === b.name &&
    a.callType === b.callType &&
    a.resume === b.resume &&
    a.jobDescription === b.jobDescription &&
    a.focus === b.focus &&
    a.extraInstructions === b.extraInstructions
  );
}

/** Field-by-field against the loaded view, so a typed-then-reverted edit is clean again. */
function isDirty(d: Draft, s: Settings): boolean {
  if (d.selectedId !== s.activeProfileId) return true;
  if (d.profiles.length !== s.profiles.length) return true;
  if (d.profiles.some((p, i) => { const o = s.profiles[i]; return o === undefined || !sameProfile(p, o); })) return true;
  return (
    d.alwaysOnTop !== s.alwaysOnTop ||
    d.llmProvider !== s.llmProvider ||
    d.answerStyle !== s.answerStyle ||
    d.hotkey !== s.hotkey ||
    d.launchPlacement !== s.launchPlacement ||
    d.streamFollow !== s.streamFollow
  );
}

/**
 * Rebase an unsaved draft onto a newer committed view (R5, ADR 016): every
 * field the user changed since `base` keeps the user's value, every field
 * they did not touch takes the newer committed value. Profiles are merged
 * per id: an edited profile is kept as typed, an untouched one takes its
 * committed version, and one that exists only in `latest` is appended.
 */
function rebaseDraft(base: Settings, draft: Draft, latest: Settings): Draft {
  const was = seedDraft(base);
  const now = seedDraft(latest);
  const pick = <K extends keyof Draft>(k: K): Draft[K] => (draft[k] !== was[k] ? draft[k] : now[k]);

  const inBase = new Map(was.profiles.map((p) => [p.id, p]));
  const inLatest = new Map(now.profiles.map((p) => [p.id, p]));
  const inDraft = new Set(draft.profiles.map((p) => p.id));
  const profiles: CallProfile[] = [];
  for (const p of draft.profiles) {
    const before = inBase.get(p.id);
    const committed = inLatest.get(p.id);
    if (before !== undefined && sameProfile(p, before)) {
      // Untouched here: the committed version wins, and a profile that was
      // deleted elsewhere stays deleted.
      if (committed !== undefined) profiles.push({ ...committed });
    } else {
      profiles.push(p);
    }
  }
  for (const p of now.profiles) {
    if (!inBase.has(p.id) && !inDraft.has(p.id) && profiles.length < MAX_PROFILES) profiles.push({ ...p });
  }
  if (profiles.length === 0) profiles.push(...now.profiles);

  const wanted = pick('selectedId');
  const selectedId = profiles.some((p) => p.id === wanted) ? wanted : (profiles[0]?.id ?? now.selectedId);
  return {
    profiles,
    selectedId,
    alwaysOnTop: pick('alwaysOnTop'),
    llmProvider: pick('llmProvider'),
    answerStyle: pick('answerStyle'),
    hotkey: pick('hotkey'),
    launchPlacement: pick('launchPlacement'),
    streamFollow: pick('streamFollow'),
  };
}

/** The core repairs anything outside `[A-Za-z0-9_-]{1,40}`; both forms fit. */
function newProfileId(): string {
  const c = globalThis.crypto;
  if (c !== undefined && typeof c.randomUUID === 'function') return c.randomUUID();
  return `p-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
}

const fmt = (n: number) => n.toLocaleString('en-US');

interface SettingsViewProps {
  settings: Settings;
  onSave(patch: SettingsPatch): Promise<Envelope<Settings>>;
  /** Fetch the committed view after a conflict (R5); the form rebases onto it. */
  onReload(): Promise<Envelope<Settings>>;
  onBack(): void;
}

/**
 * The core's local budget for the profile being edited, debounced, with
 * obsolete answers discarded (R4). Keyed by profile id and style so a result
 * computed for the previous selection is never shown for the new one.
 */
function useLocalBudget(
  enabled: boolean,
  profile: CallProfile,
  style: AnswerStyle
): LocalPromptBudget | 'failed' | null {
  const [result, setResult] = useState<{ id: string; style: AnswerStyle; budget: LocalPromptBudget | 'failed' } | null>(
    null
  );
  const seq = useRef(0);
  useEffect(() => {
    if (!enabled) return undefined;
    const mine = ++seq.current;
    const timer = window.setTimeout(() => {
      void getBridge()
        .localPromptBudget(profile, style, '')
        .then((env) => {
          // A newer edit, selection or style change has already asked again;
          // this answer describes a draft that no longer exists.
          if (mine !== seq.current) return;
          // A failed check is said as such; "Checking…" would never end.
          setResult({ id: profile.id, style, budget: env.ok ? env.value : 'failed' });
        });
    }, BUDGET_DEBOUNCE_MS);
    return () => {
      window.clearTimeout(timer);
      // Invalidate an in-flight answer too, not just the pending timer.
      seq.current += 1;
    };
  }, [enabled, profile, style]);
  if (!enabled || result === null || result.id !== profile.id || result.style !== style) return null;
  return result.budget;
}

/**
 * Full-window settings form (replaces the main view — no dialog stacking in a
 * 460px window). Key material never round-trips: the password fields start
 * empty, and only fields the user typed into are sent.
 */
export function SettingsView({ settings, onSave, onReload, onBack }: SettingsViewProps) {
  // `base` is the committed view the draft was seeded from: the dirty check
  // compares against it, and its revision travels with the save (R5).
  const [base, setBase] = useState<Settings>(settings);
  const [draft, setDraft] = useState<Draft>(() => seedDraft(settings));
  const [keys, setKeys] = useState<KeyDrafts>({});

  const [saving, setSaving] = useState(false);
  // The ref closes the gap before React commits the disabled fieldset: a
  // second Enter in the same tick must not start a second save (R3).
  const savingRef = useRef(false);
  // Settings changed elsewhere while this form held unsaved edits (R5).
  const [stale, setStale] = useState(false);
  const [reloading, setReloading] = useState(false);
  const [reloaded, setReloaded] = useTransient(false, RELOADED_NOTE_MS);
  const [saved, setSaved] = useTransient(false, SAVED_NOTE_MS);
  // Settings-local: the main error box is behind this view, so a failure
  // reported there would be invisible.
  const [localError, setLocalError] = useState<AppError | null>(null);
  const [confirmDiscard, setConfirmDiscard] = useState(false);

  const headingRef = useRef<HTMLHeadingElement>(null);
  const saveButtonRef = useRef<HTMLButtonElement>(null);
  const wasSavingRef = useRef(false);
  const keepEditingRef = useRef<HTMLButtonElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);

  const draftDirty = isDirty(draft, base);
  const dirty = draftDirty || Object.keys(keys).length > 0;
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const confirmRef = useRef(confirmDiscard);
  confirmRef.current = confirmDiscard;

  // Focus lands on the heading so a screen-reader user hears where the whole
  // window just went.
  useEffect(() => {
    headingRef.current?.focus();
  }, []);

  // A newer committed view arrived while the form is open (a chip save that
  // was still in flight when Settings opened, say). An untouched form simply
  // follows it; a form with unsaved edits keeps them and offers the reload.
  // Never mid-save: the save's own response re-seeds the form first.
  useEffect(() => {
    if (saving || settings.revision <= base.revision) return;
    if (draftDirty) {
      setStale(true);
    } else {
      // Following the newer view makes the form current again, so a banner
      // raised while it held edits (since undone) no longer applies.
      setBase(settings);
      setDraft(seedDraft(settings));
      setStale(false);
    }
  }, [settings, base.revision, draftDirty, saving]);

  // A dirty form never closes on the first Escape/Back: the guard asks
  // instead of silently dropping a resume the user spent ten minutes on.
  // Neither closes during a save (R3): unmounting then would drop the
  // response and leave the user unsure what was stored.
  const requestClose = useCallback(() => {
    if (savingRef.current) return;
    if (!dirtyRef.current) {
      onBack();
      return;
    }
    restoreFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setConfirmDiscard(true);
  }, [onBack]);

  const keepEditing = useCallback(() => {
    setConfirmDiscard(false);
    restoreFocusRef.current?.focus();
  }, []);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key !== 'Escape') return;
      if (savingRef.current) return;
      // Escape on the guard means "keep editing" — a second Escape must never
      // be the destructive answer.
      if (confirmRef.current) keepEditing();
      else requestClose();
    }
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [requestClose, keepEditing]);

  // The lock disables the focused Save button, which drops focus to <body>
  // in the webview; when the lock lifts, give it back so a keyboard user does
  // not restart from the top of the window after every save (R3).
  useEffect(() => {
    const ended = wasSavingRef.current && !saving;
    wasSavingRef.current = saving;
    if (!ended) return;
    const lost = document.activeElement == null || document.activeElement === document.body;
    if (lost) saveButtonRef.current?.focus();
  }, [saving]);

  // The least destructive action takes focus when the guard appears.
  useEffect(() => {
    if (confirmDiscard) keepEditingRef.current?.focus();
  }, [confirmDiscard]);

  const patch = useCallback((p: Partial<Draft>) => setDraft((d) => ({ ...d, ...p })), []);

  const selected =
    draft.profiles.find((p) => p.id === draft.selectedId) ??
    draft.profiles[0] ??
    emptyProfile(DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME);
  const interview = selected.callType === 'interview';

  const updateSelected = useCallback((p: Partial<CallProfile>) => {
    setDraft((d) => ({
      ...d,
      profiles: d.profiles.map((x) => (x.id === d.selectedId ? { ...x, ...p } : x)),
    }));
  }, []);

  function addProfile(from?: CallProfile) {
    if (draft.profiles.length >= MAX_PROFILES) return;
    // Generated outside the updater: StrictMode runs updaters twice in dev
    // and two ids for one click would be confusing to debug.
    const id = newProfileId();
    const next: CallProfile = from ? { ...from, id, name: `${from.name} copy` } : emptyProfile(id, 'New profile');
    setDraft((d) => ({ ...d, profiles: [...d.profiles, next], selectedId: id }));
  }

  function deleteSelected() {
    setDraft((d) => {
      if (d.profiles.length < 2) return d;
      const i = d.profiles.findIndex((p) => p.id === d.selectedId);
      const rest = d.profiles.filter((p) => p.id !== d.selectedId);
      // The previous neighbour: the eye is already there in the list.
      const neighbour = rest[Math.max(0, i - 1)] ?? rest[0];
      return { ...d, profiles: rest, selectedId: neighbour?.id ?? d.selectedId };
    });
  }

  async function save(e: FormEvent) {
    e.preventDefault();
    if (savingRef.current) return;
    savingRef.current = true;
    setSaving(true);
    setLocalError(null);
    const body: SettingsPatch = {
      // The revision this draft was seeded from: the core commits only if it
      // is still current (R5, ADR 016).
      expectedRevision: base.revision,
      profiles: draft.profiles,
      activeProfileId: draft.selectedId,
      alwaysOnTop: draft.alwaysOnTop,
      llmProvider: draft.llmProvider,
      answerStyle: draft.answerStyle,
      hotkey: draft.hotkey,
      launchPlacement: draft.launchPlacement,
      streamFollow: draft.streamFollow,
      ...keys,
    };
    try {
      const env = await onSave(body);
      if (env.ok) {
        // Re-seed from what the core actually stored: ids may have been
        // repaired, names trimmed, the hotkey normalized. Keys go back to
        // "untouched" so a second Save doesn't re-send the same key — and
        // so the placeholders flip to "saved — type to replace".
        setBase(env.value);
        setDraft(seedDraft(env.value));
        setKeys({});
        setConfirmDiscard(false);
        setStale(false);
        setSaved(true);
      } else if (env.error.code === 'settings_conflict') {
        // Nothing was saved. The draft and typed keys stay exactly as they
        // are; the banner offers the reload that rebases them.
        setStale(true);
      } else {
        setLocalError(env.error);
      }
    } finally {
      // Success or failure, the form is editable again with whatever the
      // user had typed (R3).
      savingRef.current = false;
      setSaving(false);
    }
  }

  async function reload() {
    if (savingRef.current || reloading) return;
    setReloading(true);
    setLocalError(null);
    try {
      const env = await onReload();
      if (env.ok) {
        // Unsaved edits are rebased onto the committed view; typed keys are
        // separate drafts and survive untouched.
        setDraft((d) => rebaseDraft(base, d, env.value));
        setBase(env.value);
        setStale(false);
        setReloaded(true);
      } else {
        setLocalError(env.error);
      }
    } finally {
      setReloading(false);
    }
  }

  const keyPlaceholder = (has: boolean) => (has ? 'saved — type to replace' : '');

  const local = draft.llmProvider === 'local';
  // The UNSAVED draft is what gets previewed: the profile being edited and
  // the style chosen in this form (R4).
  const budget = useLocalBudget(local, selected, draft.answerStyle);

  return (
    <div className="app settings-view">
      <header className="app-header">
        <h1 ref={headingRef} tabIndex={-1} className="app-title">
          Settings
        </h1>
      </header>
      {base.storageWarning != null && (
        <p className="settings-warning" role="note">
          {base.storageWarning}
        </p>
      )}
      <form className="settings-form" onSubmit={(e) => void save(e)} aria-busy={saving}>
        {/* One lock for every editing and navigation control while a save is
            in flight (R3): profile add/duplicate/delete/selection, every
            field, the key inputs, Save and Back. The response then re-seeds
            a form nobody could have typed into, so nothing typed is lost. */}
        <fieldset className="settings-lock" disabled={saving}>
        <fieldset className="settings-section">
          <legend>Keys &amp; model</legend>

          <div className="field">
            <label className="field-label" htmlFor="set-provider">
              Answer model
            </label>
            <select
              id="set-provider"
              value={draft.llmProvider}
              onChange={(e) => patch({ llmProvider: e.target.value as LlmProviderKind })}
            >
              {PROVIDER_ORDER.map((k) => (
                <option key={k} value={k}>
                  {PROVIDERS[k].label}
                </option>
              ))}
            </select>
          </div>

          {/* Hidden, not unmounted: a typed key survives a provider round
              trip, and `hidden` keeps the row out of the a11y tree. */}
          {KEY_FIELDS.map((k) => (
            <div key={k.field} className="field" hidden={!k.needed(draft.llmProvider)}>
              <label className="field-label" htmlFor={k.id}>
                {k.label}
              </label>
              <input
                id={k.id}
                type="password"
                autoComplete="off"
                value={keys[k.field] ?? ''}
                placeholder={keyPlaceholder(settings[k.has])}
                onChange={(e) => setKeys((prev) => ({ ...prev, [k.field]: e.target.value }))}
              />
            </div>
          ))}

          {local && <LocalVoicePanel />}

          <div className="field">
            <label className="field-label" htmlFor="set-style">
              Answer style
            </label>
            <select
              id="set-style"
              value={draft.answerStyle}
              onChange={(e) => patch({ answerStyle: e.target.value as AnswerStyle })}
            >
              <option value="brief">Brief</option>
              <option value="balanced">Balanced</option>
              <option value="detailed">Detailed</option>
            </select>
          </div>
        </fieldset>

        <fieldset className="settings-section">
          <legend>Profile</legend>

          <div className="field">
            <label className="field-label" htmlFor="set-profile">
              Profile
            </label>
            <div className="profile-toolbar">
              <select
                id="set-profile"
                value={draft.selectedId}
                onChange={(e) => patch({ selectedId: e.target.value })}
              >
                {draft.profiles.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
              <button
                type="button"
                className="ghost-button"
                disabled={draft.profiles.length >= MAX_PROFILES}
                onClick={() => addProfile()}
              >
                New
              </button>
              <button
                type="button"
                className="ghost-button"
                disabled={draft.profiles.length >= MAX_PROFILES}
                onClick={() => addProfile(selected)}
              >
                Duplicate
              </button>
              <button
                type="button"
                className="ghost-button"
                disabled={draft.profiles.length < 2}
                onClick={deleteSelected}
              >
                Delete
              </button>
            </div>
            <p className="field-help">The selected profile grounds the next answer once you save.</p>
          </div>

          <div className="field">
            <label className="field-label" htmlFor="set-profile-name">
              Profile name
            </label>
            <input
              id="set-profile-name"
              type="text"
              maxLength={MAX_PROFILE_NAME_CHARS}
              value={selected.name}
              onChange={(e) => updateSelected({ name: e.target.value })}
            />
          </div>

          <div className="field">
            <label className="field-label" htmlFor="set-call-type">
              Call type
            </label>
            <select
              id="set-call-type"
              value={selected.callType}
              onChange={(e) => updateSelected({ callType: e.target.value as CallType })}
            >
              {CALL_TYPES.map((t) => (
                <option key={t.value} value={t.value}>
                  {t.label}
                </option>
              ))}
            </select>
          </div>

          <div className="field">
            <label className="field-label" htmlFor="set-focus">
              Focus (optional)
            </label>
            <input
              id="set-focus"
              type="text"
              placeholder="What to emphasize, e.g. Rust, tokio, async"
              aria-describedby="set-focus-count"
              value={selected.focus}
              onChange={(e) => updateSelected({ focus: e.target.value })}
            />
            <p id="set-focus-count" className="field-help">
              {fmt(selected.focus.length)} / {fmt(MAX_FOCUS_CHARS)} characters
            </p>
          </div>

          <div className="field">
            <label className="field-label" htmlFor="set-resume">
              {interview ? 'Resume' : 'About you (optional)'}
            </label>
            <textarea
              id="set-resume"
              rows={10}
              aria-describedby="set-resume-count"
              value={selected.resume}
              onChange={(e) => updateSelected({ resume: e.target.value })}
            />
            <p id="set-resume-count" className="field-help">
              {fmt(selected.resume.length)} / {fmt(MAX_PROFILE_CHARS)} characters
            </p>
          </div>

          <div className="field">
            <label className="field-label" htmlFor="set-jd">
              {interview ? 'Job description' : 'Call context (account, product, agenda)'}
            </label>
            <textarea
              id="set-jd"
              rows={10}
              aria-describedby="set-jd-count"
              value={selected.jobDescription}
              onChange={(e) => updateSelected({ jobDescription: e.target.value })}
            />
            <p id="set-jd-count" className="field-help">
              {fmt(selected.jobDescription.length)} / {fmt(MAX_PROFILE_CHARS)} characters
            </p>
          </div>

          <div className="field">
            <label className="field-label" htmlFor="set-extra">
              Extra instructions (optional)
            </label>
            <textarea
              id="set-extra"
              rows={2}
              aria-describedby="set-extra-count"
              value={selected.extraInstructions}
              onChange={(e) => updateSelected({ extraInstructions: e.target.value })}
            />
            <p id="set-extra-count" className="field-help">
              {fmt(selected.extraInstructions.length)} / {fmt(MAX_EXTRA_INSTRUCTIONS_CHARS)} characters
            </p>
          </div>

          {local && <LocalBudgetNote budget={budget} />}
        </fieldset>

        <fieldset className="settings-section">
          <legend>Shortcut &amp; window</legend>

          <div className="field">
            <label className="field-label" htmlFor="set-hotkey">
              Global shortcut
            </label>
            <input
              id="set-hotkey"
              type="text"
              value={draft.hotkey}
              placeholder={DEFAULT_HOTKEY}
              aria-describedby="set-hotkey-help"
              onChange={(e) => patch({ hotkey: e.target.value })}
            />
            <p id="set-hotkey-help" className="field-help">
              Modifier+key combination, e.g. Ctrl+Shift+Space. Works while any app is focused. Leave empty to turn
              the shortcut off. Applied when you save.
            </p>
          </div>

          <div className="field">
            <div className="field--checkbox">
              <input
                id="set-aot"
                type="checkbox"
                checked={draft.alwaysOnTop}
                aria-describedby="set-aot-help"
                onChange={(e) => patch({ alwaysOnTop: e.target.checked })}
              />
              <label className="field-label" htmlFor="set-aot">
                Keep this window always on top
              </label>
            </div>
            <p id="set-aot-help" className="field-help">
              Keeps the assistant above your call window so you can read answers while the meeting is focused.
            </p>
          </div>

          <div className="field">
            <label className="field-label" htmlFor="set-placement">
              Window position at launch
            </label>
            <select
              id="set-placement"
              value={draft.launchPlacement}
              aria-describedby="set-placement-help"
              onChange={(e) => patch({ launchPlacement: e.target.value as LaunchPlacement })}
            >
              <option value="remembered">Remember where I left it</option>
              <option value="camera">Dock under the camera (top centre)</option>
            </select>
            <p id="set-placement-help" className="field-help">
              Docked windows sit at the top of the screen, so reading the answer looks like eye contact with the
              webcam. The ⬆ button on the main screen docks at any time.
            </p>
          </div>

          <div className="field">
            <label className="field-label" htmlFor="set-follow">
              While an answer streams
            </label>
            <select
              id="set-follow"
              value={draft.streamFollow}
              onChange={(e) => patch({ streamFollow: e.target.value as StreamFollow })}
            >
              <option value="tail">Follow the newest text</option>
              <option value="top">Stay at the opening sentence (teleprompter)</option>
            </select>
          </div>
        </fieldset>

        <p className="settings-note">
          Keys are stored encrypted and never shown again. Windows capture exclusion was verified at launch; test your sharing app before relying on it.
        </p>

        <ErrorBox error={localError} />

        {stale && (
          <div className="settings-conflict" role="alert">
            <p>Settings changed elsewhere — reload. Your unsaved edits are kept.</p>
            <button type="button" className="primary-button" onClick={() => void reload()} disabled={reloading}>
              {reloading ? 'Reloading…' : 'Reload'}
            </button>
          </div>
        )}
        {reloaded && (
          <p role="status" className="saved-note">
            Reloaded. Your unsaved edits were kept; review them and Save.
          </p>
        )}

        <div className="settings-actions">
          {/* Focus lands on "Keep editing", so the question has to be the
              dialog's description — otherwise a screen reader announces the
              dialog name and the focused button, and never what "Discard"
              throws away. */}
          {confirmDiscard && (
            <div
              role="alertdialog"
              aria-label="Unsaved changes"
              aria-describedby="discard-question"
              className="discard-dialog"
            >
              <p id="discard-question">Discard unsaved changes?</p>
              <button type="button" className="ghost-button" onClick={onBack}>
                Discard
              </button>
              <button ref={keepEditingRef} type="button" className="primary-button" onClick={keepEditing}>
                Keep editing
              </button>
            </div>
          )}
          <div className="settings-actions-row">
            <button ref={saveButtonRef} type="submit" className="primary-button" disabled={saving || stale}>
              {saving ? 'Saving…' : 'Save'}
            </button>
            <button type="button" className="ghost-button" onClick={requestClose}>
              Back
            </button>
            {saving && (
              <span role="status" className="saved-note">
                Saving…
              </span>
            )}
            {saved && (
              <span role="status" className="saved-note">
                Saved ✓
              </span>
            )}
          </div>
        </div>
        </fieldset>
      </form>
    </div>
  );
}

/**
 * The local budget line under the profile (R4). Three states: room to spare,
 * little room for a question (a usability warning at `reserveBytes`), and
 * over the limit (local use blocked; saving for cloud models stays possible).
 */
function LocalBudgetNote({ budget }: { budget: LocalPromptBudget | 'failed' | null }) {
  if (budget === null) {
    return <p className="field-help">Checking the free local mode size…</p>;
  }
  if (budget === 'failed') {
    return (
      <p className="field-help">
        Could not check the free local mode size. The limit is still checked when you record or ask.
      </p>
    );
  }
  const { usedBytes, limitBytes, remainingBytes, reserveBytes } = budget;
  if (budget.status === 'over') {
    return (
      <p className="field-help field-help--error">
        Too long for free local mode: the instructions and this profile use {fmt(usedBytes)} of {fmt(limitBytes)}{' '}
        bytes, so no question fits. Recording and Ask are refused in local mode with this profile; you can still save
        it for a cloud model.
      </p>
    );
  }
  if (budget.status === 'tight') {
    return (
      <p className="field-help field-help--warn">
        Only {fmt(remainingBytes)} bytes left for the question in free local mode (most spoken questions need about{' '}
        {fmt(reserveBytes)}). Shorten this profile or use a cloud model.
      </p>
    );
  }
  return (
    <p className="field-help">
      {fmt(remainingBytes)} bytes left for the question in free local mode ({fmt(usedBytes)} of {fmt(limitBytes)} used
      by the instructions and this profile).
    </p>
  );
}

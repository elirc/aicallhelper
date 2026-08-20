import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import type {
  AnswerStyle,
  AppError,
  Envelope,
  LlmProviderKind,
  SettingsPatch,
  SettingsView as Settings,
} from '../types';
import { ErrorBox } from '../components/ErrorBox';

/** Mirrors the core's default accelerator; shown as the hotkey placeholder. */
const DEFAULT_HOTKEY = 'CommandOrControl+Shift+Space';
const SAVED_NOTE_MS = 1500;

interface SettingsViewProps {
  settings: Settings;
  onSave(patch: SettingsPatch): Promise<Envelope<Settings>>;
  onBack(): void;
}

/**
 * Full-window settings form (replaces the main view — no dialog stacking in a
 * 460px window). Key material never round-trips: the password fields start
 * empty, and only fields the user typed into are sent.
 */
export function SettingsView({ settings, onSave, onBack }: SettingsViewProps) {
  const [resume, setResume] = useState(settings.resume);
  const [jobDescription, setJobDescription] = useState(settings.jobDescription);
  const [alwaysOnTop, setAlwaysOnTop] = useState(settings.alwaysOnTop);
  const [llmProvider, setLlmProvider] = useState<LlmProviderKind>(settings.llmProvider);
  const [answerStyle, setAnswerStyle] = useState<AnswerStyle>(settings.answerStyle);
  const [hotkey, setHotkey] = useState(settings.hotkey);

  // null = untouched = omit from the patch ("leave the stored key alone");
  // '' = the user typed and then cleared = deliberately wipe the key.
  // Sending '' for an untouched field would silently destroy a stored key.
  const [deepgramKey, setDeepgramKey] = useState<string | null>(null);
  const [anthropicKey, setAnthropicKey] = useState<string | null>(null);
  const [groqKey, setGroqKey] = useState<string | null>(null);

  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  // Settings-local: the main error box is behind this view, so a failure
  // reported there would be invisible.
  const [localError, setLocalError] = useState<AppError | null>(null);

  const headingRef = useRef<HTMLHeadingElement>(null);
  const savedTimerRef = useRef<number | null>(null);

  // Focus lands on the heading so a screen-reader user hears where the whole
  // window just went.
  useEffect(() => {
    headingRef.current?.focus();
  }, []);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === 'Escape') onBack();
    }
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onBack]);

  useEffect(
    () => () => {
      if (savedTimerRef.current != null) window.clearTimeout(savedTimerRef.current);
    },
    []
  );

  async function save(e: FormEvent) {
    e.preventDefault();
    if (saving) return;
    setSaving(true);
    setLocalError(null);
    const patch: SettingsPatch = { resume, jobDescription, alwaysOnTop, llmProvider, answerStyle, hotkey };
    if (deepgramKey != null) patch.deepgramKey = deepgramKey;
    if (anthropicKey != null) patch.anthropicKey = anthropicKey;
    if (groqKey != null) patch.groqKey = groqKey;
    try {
      const env = await onSave(patch);
      if (env.ok) {
        // Back to "untouched" so a second Save doesn't re-send the same key —
        // and so the placeholders flip to "saved — type to replace".
        setDeepgramKey(null);
        setAnthropicKey(null);
        setGroqKey(null);
        setSaved(true);
        if (savedTimerRef.current != null) window.clearTimeout(savedTimerRef.current);
        savedTimerRef.current = window.setTimeout(() => setSaved(false), SAVED_NOTE_MS);
      } else {
        setLocalError(env.error);
      }
    } finally {
      setSaving(false);
    }
  }

  const keyPlaceholder = (has: boolean) => (has ? 'saved — type to replace' : '');

  return (
    <div className="app settings-view">
      <header className="app-header">
        <h1 ref={headingRef} tabIndex={-1} className="app-title">
          Settings
        </h1>
      </header>
      <form className="settings-form" onSubmit={(e) => void save(e)}>
        <div className="field">
          <label className="field-label" htmlFor="set-deepgram">
            Deepgram API key
          </label>
          <input
            id="set-deepgram"
            type="password"
            autoComplete="off"
            value={deepgramKey ?? ''}
            placeholder={keyPlaceholder(settings.hasDeepgramKey)}
            onChange={(e) => setDeepgramKey(e.target.value)}
          />
        </div>

        <div className="field">
          <label className="field-label" htmlFor="set-provider">
            Answer model
          </label>
          <select
            id="set-provider"
            value={llmProvider}
            onChange={(e) => setLlmProvider(e.target.value as LlmProviderKind)}
          >
            <option value="anthropic">Claude Haiku 4.5 (recommended)</option>
            <option value="groq">Groq GPT-OSS 120B (fastest)</option>
          </select>
        </div>

        <div className="field">
          <label className="field-label" htmlFor="set-anthropic">
            Anthropic API key
          </label>
          <input
            id="set-anthropic"
            type="password"
            autoComplete="off"
            value={anthropicKey ?? ''}
            placeholder={keyPlaceholder(settings.hasAnthropicKey)}
            onChange={(e) => setAnthropicKey(e.target.value)}
          />
        </div>

        <div className="field">
          <label className="field-label" htmlFor="set-groq">
            Groq API key (only for the Groq preset)
          </label>
          <input
            id="set-groq"
            type="password"
            autoComplete="off"
            value={groqKey ?? ''}
            placeholder={keyPlaceholder(settings.hasGroqKey)}
            onChange={(e) => setGroqKey(e.target.value)}
          />
        </div>

        <div className="field">
          <label className="field-label" htmlFor="set-style">
            Answer style
          </label>
          <select id="set-style" value={answerStyle} onChange={(e) => setAnswerStyle(e.target.value as AnswerStyle)}>
            <option value="brief">Brief</option>
            <option value="balanced">Balanced</option>
            <option value="detailed">Detailed</option>
          </select>
        </div>

        <div className="field">
          <label className="field-label" htmlFor="set-hotkey">
            Global shortcut
          </label>
          <input
            id="set-hotkey"
            type="text"
            value={hotkey}
            placeholder={DEFAULT_HOTKEY}
            aria-describedby="set-hotkey-help"
            onChange={(e) => setHotkey(e.target.value)}
          />
          <p id="set-hotkey-help" className="field-help">
            Modifier+key combination, e.g. CommandOrControl+Shift+Space. Applied when you save.
          </p>
        </div>

        <div className="field">
          <label className="field-label" htmlFor="set-resume">
            Resume
          </label>
          <textarea id="set-resume" rows={5} value={resume} onChange={(e) => setResume(e.target.value)} />
        </div>

        <div className="field">
          <label className="field-label" htmlFor="set-jd">
            Job description
          </label>
          <textarea id="set-jd" rows={5} value={jobDescription} onChange={(e) => setJobDescription(e.target.value)} />
        </div>

        <div className="field field--checkbox">
          <input
            id="set-aot"
            type="checkbox"
            checked={alwaysOnTop}
            onChange={(e) => setAlwaysOnTop(e.target.checked)}
          />
          <label className="field-label" htmlFor="set-aot">
            Keep this window always on top
          </label>
        </div>

        <p className="settings-note">
          Keys are stored encrypted and never shown again. This window is hidden from screen sharing.
        </p>

        <ErrorBox error={localError} />

        <div className="settings-actions">
          <button type="submit" className="primary-button" disabled={saving}>
            Save
          </button>
          <button type="button" className="ghost-button" onClick={onBack}>
            Back
          </button>
          {saved && (
            <span role="status" className="saved-note">
              Saved ✓
            </span>
          )}
        </div>
      </form>
    </div>
  );
}

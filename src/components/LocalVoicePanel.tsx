import { useEffect, useRef, useState } from 'react';
import { checkLocalVoice, prepareLocalVoice } from '../bridge';
import type { LocalVoiceStatus } from '../types';

export function LocalVoicePanel() {
  const [status, setStatus] = useState<LocalVoiceStatus | null>(null);
  const [busy, setBusy] = useState<'check' | 'warm' | null>(null);
  const [message, setMessage] = useState('');
  const pending = useRef(false);
  const mounted = useRef(true);

  async function check(warm: boolean) {
    if (pending.current) return;
    pending.current = true;
    setBusy(warm ? 'warm' : 'check');
    setMessage('');
    try {
      const result = await (warm ? prepareLocalVoice() : checkLocalVoice());
      if (!mounted.current) return;
      if (result.ok) {
        setStatus(result.value);
        if (warm) setMessage('Ready. Save this mode, then play call audio and press Record.');
      } else {
        setMessage(result.error.message);
        setStatus(null);
      }
    } catch {
      if (mounted.current) {
        setStatus(null);
        setMessage('Could not reach local setup. Try again in the desktop app.');
      }
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(null);
    }
  }

  useEffect(() => {
    mounted.current = true;
    void check(false);
    return () => { mounted.current = false; };
  }, []);

  return (
    <section className="local-voice-panel" aria-label="Free local voice setup">
      <h2>Free local voice</h2>
      <p>Qwen3.5 2B answers and Moonshine Tiny Streaming speech run on this computer.
        No API keys or per-minute fees. After setup, this mode works offline.</p>
      <ul className="local-service-list" aria-label="Local services">
        <li>Ollama: <strong>{status == null ? 'Not checked' : status.ollamaRunning ? 'Running' : 'Not running'}</strong></li>
        <li>Qwen3.5 2B: <strong>{status == null ? 'Not checked' : status.modelAvailable ? 'Installed' : 'Not found'}</strong></li>
        <li>English speech: <strong>{status == null ? 'Not checked' : status.speechReady ? 'Ready' : 'Not running'}</strong></li>
      </ul>
      <div className="local-service-actions">
        <button type="button" className="primary-button" disabled={busy != null} onClick={() => void check(true)}>
          {busy === 'warm' ? 'Starting and warming…' : 'Start and warm free mode'}
        </button>
        <button type="button" className="ghost-button" disabled={busy != null} onClick={() => void check(false)}>
          {busy === 'check' ? 'Checking…' : 'Check status'}
        </button>
      </div>
      <p className="field-help" role="status">
        {busy === 'warm' ? 'Loading local models can take a minute or two on a CPU.' : message}
      </p>
      <details>
        <summary>First-time setup and testing</summary>
        <p>From the project folder, run:</p>
        <code className="local-setup-command">powershell -ExecutionPolicy Bypass -File .\scripts\setup-free-voice.ps1</code>
        <p>Setup downloads the free models once. Allow at least 8 GB of free disk space;
          use the script’s -DataDir option for another drive.</p>
        <p>Record captures audio playing through your speakers or headphones, just like cloud mode.
          Play a practice question, press Record, then Stop &amp; Answer. Typed Ask, resume context,
          answer styles, regenerate, copy, history, and the global shortcut use the same controls.</p>
        <p>Speech recognition is English. CPU answers may be slower and less accurate than cloud answers.
          Keep the combined resume, job description and question short (about 7 KB).
          Downloads need internet; local testing has no API usage charges.</p>
      </details>
    </section>
  );
}

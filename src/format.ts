/**
 * Display formatting kept out of components so the exact strings — which the
 * spec pins verbatim — can be asserted without rendering anything.
 */
import type { Metrics } from './types';

/** "mm:ss" for the recording timer. Negative input clamps to zero. */
export function formatDuration(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const mm = String(Math.floor(totalSeconds / 60)).padStart(2, '0');
  const ss = String(totalSeconds % 60).padStart(2, '0');
  return `${mm}:${ss}`;
}

/** "1.2s" — one decimal, always shown so the column of numbers lines up. */
export function formatLatency(ms: number): string {
  return `${(Math.max(0, ms) / 1000).toFixed(1)}s`;
}

/**
 * Accelerator tokens as the user should read them. This app is Windows-only,
 * so the cross-platform "CommandOrControl" spelling always means Ctrl here —
 * showing the raw token would look like a bug.
 */
const TOKEN_DISPLAY: Record<string, string> = {
  commandorcontrol: 'Ctrl',
  cmdorctrl: 'Ctrl',
  control: 'Ctrl',
  ctrl: 'Ctrl',
  command: 'Cmd',
  cmd: 'Cmd',
  option: 'Alt',
  altgr: 'AltGr',
  alt: 'Alt',
  shift: 'Shift',
  super: 'Win',
  meta: 'Win',
  win: 'Win',
  space: 'Space',
  escape: 'Esc',
  esc: 'Esc',
  return: 'Enter',
  enter: 'Enter',
};

function displayToken(token: string): string {
  const mapped = TOKEN_DISPLAY[token.toLowerCase()];
  if (mapped !== undefined) return mapped;
  if (token.length === 1) return token.toUpperCase();
  return token.charAt(0).toUpperCase() + token.slice(1);
}

export function formatHotkey(accelerator: string): string {
  return accelerator
    .split('+')
    .map((part) => part.trim())
    .filter((part) => part !== '')
    .map(displayToken)
    .join('+');
}

/**
 * Tooltip for the metrics chip. Wording is pinned by the spec — do not edit.
 * §9 spells the total as "X.X s" WITH a space before the unit, unlike the
 * chip's "X.Xs to first word" (formatLatency) which has none — so the two
 * must not share a formatter.
 */
export function latencyTitle(m: Metrics): string {
  return `First word ${m.firstTokenMs} ms after Stop · transcript finalized ${m.sttFinalizeMs} ms · full answer ${(m.totalMs / 1000).toFixed(1)} s`;
}

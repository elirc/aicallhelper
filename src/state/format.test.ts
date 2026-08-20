/**
 * The spec pins these strings verbatim (including the "·" separators); a
 * "harmless" wording tweak is a UI regression, so the assertions are exact.
 */
import { describe, expect, it } from 'vitest';
import { formatDuration, formatHotkey, formatLatency, latencyTitle } from '../format';
import type { Metrics } from '../types';

describe('formatDuration', () => {
  it('renders mm:ss with zero padding', () => {
    expect(formatDuration(0)).toBe('00:00');
    expect(formatDuration(999)).toBe('00:00'); // floors, never rounds up a second early
    expect(formatDuration(1000)).toBe('00:01');
    expect(formatDuration(59_000)).toBe('00:59');
    expect(formatDuration(60_000)).toBe('01:00');
    expect(formatDuration(65_432)).toBe('01:05');
    expect(formatDuration(120_000)).toBe('02:00');
  });

  it('clamps negative input to zero', () => {
    expect(formatDuration(-500)).toBe('00:00');
  });
});

describe('formatLatency', () => {
  it('renders seconds with exactly one decimal', () => {
    expect(formatLatency(1200)).toBe('1.2s');
    expect(formatLatency(1000)).toBe('1.0s');
    expect(formatLatency(0)).toBe('0.0s');
    expect(formatLatency(340)).toBe('0.3s');
    expect(formatLatency(12_340)).toBe('12.3s');
  });

  it('clamps negative input to zero', () => {
    expect(formatLatency(-100)).toBe('0.0s');
  });
});

describe('formatHotkey', () => {
  it('passes an already-displayable accelerator through', () => {
    expect(formatHotkey('Ctrl+Shift+Space')).toBe('Ctrl+Shift+Space');
  });

  it('normalizes the cross-platform Ctrl spellings (this app is Windows-only)', () => {
    expect(formatHotkey('CommandOrControl+Shift+Space')).toBe('Ctrl+Shift+Space');
    expect(formatHotkey('CmdOrCtrl+Shift+Space')).toBe('Ctrl+Shift+Space');
    expect(formatHotkey('Control+Alt+P')).toBe('Ctrl+Alt+P');
  });

  it('capitalizes lowercase tokens and single keys', () => {
    expect(formatHotkey('ctrl+shift+space')).toBe('Ctrl+Shift+Space');
    expect(formatHotkey('alt+f4')).toBe('Alt+F4');
    expect(formatHotkey('ctrl+a')).toBe('Ctrl+A');
  });

  it('handles empty and whitespace-padded input without inventing separators', () => {
    expect(formatHotkey('')).toBe('');
    expect(formatHotkey(' Ctrl + Shift + Space ')).toBe('Ctrl+Shift+Space');
  });
});

describe('latencyTitle', () => {
  // §9 pins the total as "X.X s" with a space before the unit; the chip's
  // "X.Xs to first word" (formatLatency, asserted above) has none. The two
  // formats diverging is spec text, not sloppiness.
  it('emits the pinned wording, order, and "·" separators exactly', () => {
    const m: Metrics = { sttFinalizeMs: 480, firstTokenMs: 950, totalMs: 3210 };
    expect(latencyTitle(m)).toBe(
      'First word 950 ms after Stop · transcript finalized 480 ms · full answer 3.2 s',
    );
  });

  it('reports 0 ms transcript finalization for typed questions (no STT stage)', () => {
    const m: Metrics = { sttFinalizeMs: 0, firstTokenMs: 400, totalMs: 1000 };
    expect(latencyTitle(m)).toBe(
      'First word 400 ms after Stop · transcript finalized 0 ms · full answer 1.0 s',
    );
  });
});

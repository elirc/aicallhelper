interface LevelMeterProps {
  /** 0.0..=1.0 from the audio:level event. */
  rms: number;
}

/**
 * Live microphone level. Its only job is reassurance: with no visible motion a
 * user cannot tell a dead microphone from a quiet room, and finds out only
 * after wasting a recording.
 */
export function LevelMeter({ rms }: LevelMeterProps) {
  // The core promises 0..1 but a meter that overflows its track on a bad
  // sample looks broken, so clamp anyway.
  const pct = Math.round(Math.min(1, Math.max(0, rms)) * 100);
  return (
    <div
      className="level-meter"
      role="meter"
      aria-label="Microphone level"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={pct}
    >
      {/* Not a CSP concern, despite looking like one: React applies a style
          prop via direct CSSOM property assignment (element.style.width = …),
          which `style-src` does not gate. What IS gated: style attributes
          (whether from the parser or setAttribute('style', …)) and <style>
          elements (including JS-inserted ones — which is why index.html's dev
          meta keeps 'unsafe-inline' for Vite's injected styles). The shipped
          policy is `style-src 'self'` (`src-tauri/tauri.conf.json`, §10) and
          a per-frame width here costs it nothing. */}
      <div className="level-meter-fill" style={{ width: `${pct}%` }} />
    </div>
  );
}

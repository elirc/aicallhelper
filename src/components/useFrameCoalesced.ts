/**
 * Frame-coalesced text for a streaming source (R9).
 *
 * Tokens can arrive far faster than 60 Hz, and every commit re-parses the
 * markdown — CPU the STT/LLM pipeline is competing for. While `streaming`,
 * the freshest source is held in a ref and committed at most once per
 * animation frame. Outside a stream there is no flood, so the source is
 * committed immediately: history switches and final answers must never lag
 * a frame behind.
 *
 * An ENTRY switch commits synchronously (layout effect) even mid-stream, and
 * drops any frame queued for the old entry, so the caller's scroll reset can
 * measure the NEW entry's DOM in the same commit — measuring the previous
 * entry's geometry is how a short old answer used to bottom-stick a long
 * new one.
 */
import { useEffect, useLayoutEffect, useRef, useState } from 'react';

export function useFrameCoalesced(source: string, streaming: boolean, entryKey: string | null): string {
  const [displayed, setDisplayed] = useState(source);
  const latestRef = useRef(source);
  const frameRef = useRef<number | null>(null);
  const prevKeyRef = useRef(entryKey);

  function cancelFrame(): void {
    if (frameRef.current != null) {
      cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    }
  }

  // `source` is deliberately not a dep: this is the text at the moment of
  // the switch; later tokens go through the frame path below.
  useLayoutEffect(() => {
    if (prevKeyRef.current === entryKey) return;
    prevKeyRef.current = entryKey;
    cancelFrame();
    setDisplayed(source);
  }, [entryKey]);

  useEffect(() => {
    latestRef.current = source;
    if (!streaming) {
      cancelFrame();
      setDisplayed(source);
      return;
    }
    if (frameRef.current == null) {
      frameRef.current = requestAnimationFrame(() => {
        frameRef.current = null;
        setDisplayed(latestRef.current);
      });
    }
  }, [source, streaming]);

  useEffect(() => () => cancelFrame(), []);

  return displayed;
}

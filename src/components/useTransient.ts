/**
 * A value that reverts to `idle` on its own after `ms` (R7).
 *
 * Four places used to hand-roll the same setState + clearTimeout +
 * setTimeout + unmount-cleanup dance ("Copied ✓", "Saved ✓" and two live-
 * region announcements); each copy was one missed clearTimeout away from a
 * state update on an unmounted component.
 */
import { useCallback, useEffect, useRef, useState } from 'react';

export function useTransient<T>(idle: T, ms: number): [T, (value: T) => void] {
  const [value, setValue] = useState<T>(idle);
  const timerRef = useRef<number | null>(null);
  // Read through refs so the setter is referentially stable: callers list it
  // in useCallback deps, and a fresh setter per render would defeat that.
  const idleRef = useRef(idle);
  idleRef.current = idle;
  const msRef = useRef(ms);
  msRef.current = ms;

  useEffect(
    () => () => {
      if (timerRef.current != null) window.clearTimeout(timerRef.current);
    },
    []
  );

  const set = useCallback((next: T) => {
    if (timerRef.current != null) window.clearTimeout(timerRef.current);
    timerRef.current = null;
    setValue(next);
    // Setting the idle value by hand is a manual reset: nothing to revert.
    if (Object.is(next, idleRef.current)) return;
    // Restarted on every set, so a second Copy within the window extends the
    // note instead of having the first timer snatch it away early.
    timerRef.current = window.setTimeout(() => {
      timerRef.current = null;
      setValue(idleRef.current);
    }, msRef.current);
  }, []);

  return [value, set];
}

/**
 * Text for a PERMANENTLY mounted live region (UX-12). The wipe after `ms` is
 * what makes a repeated announcement audible: setting an identical string
 * twice is a state update React bails out of — no DOM mutation, nothing for
 * the screen reader to read the second time.
 */
export function useAnnouncer(ms = 1500): [string, (text: string) => void] {
  return useTransient('', ms);
}

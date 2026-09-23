/**
 * The shared "show for a beat, then revert" hook behind Copied ✓, Saved ✓
 * and the two live-region announcements (R7).
 */
import { act, renderHook } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { useAnnouncer, useTransient } from './useTransient';

afterEach(() => {
  vi.useRealTimers();
});

describe('useTransient', () => {
  it('reverts to idle exactly after the delay', () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useTransient(false, 1200));
    expect(result.current[0]).toBe(false);
    act(() => result.current[1](true));
    expect(result.current[0]).toBe(true);
    act(() => vi.advanceTimersByTime(1199));
    expect(result.current[0]).toBe(true);
    act(() => vi.advanceTimersByTime(1));
    expect(result.current[0]).toBe(false);
  });

  // A second Copy 800 ms into a 1000 ms note used to be cut short by the
  // FIRST timer 200 ms later, so the confirmation of the second click
  // vanished almost immediately.
  it('a second set inside the window restarts the clock', () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useTransient(false, 1000));
    act(() => result.current[1](true));
    act(() => vi.advanceTimersByTime(800));
    act(() => result.current[1](true));
    act(() => vi.advanceTimersByTime(800));
    expect(result.current[0]).toBe(true);
    act(() => vi.advanceTimersByTime(200));
    expect(result.current[0]).toBe(false);
  });

  it('setting the idle value by hand is a plain reset with no timer left behind', () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useTransient('', 500));
    const baseline = vi.getTimerCount();
    act(() => result.current[1]('note'));
    expect(vi.getTimerCount()).toBe(baseline + 1);
    act(() => result.current[1](''));
    expect(result.current[0]).toBe('');
    expect(vi.getTimerCount()).toBe(baseline);
  });

  it('the setter is referentially stable, so it can sit in useCallback deps', () => {
    const { result, rerender } = renderHook(() => useTransient(false, 10));
    const first = result.current[1];
    rerender();
    expect(result.current[1]).toBe(first);
  });

  it('unmount clears the pending revert', () => {
    vi.useFakeTimers();
    const { result, unmount } = renderHook(() => useTransient(false, 100));
    const baseline = vi.getTimerCount();
    act(() => result.current[1](true));
    expect(vi.getTimerCount()).toBe(baseline + 1);
    unmount();
    expect(vi.getTimerCount()).toBe(baseline);
  });
});

describe('useAnnouncer', () => {
  // The wipe is what makes a REPEATED announcement audible: an identical
  // string is a state update React bails out of — no DOM change, nothing
  // for the screen reader to read.
  it('starts empty, holds the text, then wipes it after 1.5 s by default', () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useAnnouncer());
    expect(result.current[0]).toBe('');
    act(() => result.current[1]('History cleared'));
    expect(result.current[0]).toBe('History cleared');
    act(() => vi.advanceTimersByTime(1499));
    expect(result.current[0]).toBe('History cleared');
    act(() => vi.advanceTimersByTime(1));
    expect(result.current[0]).toBe('');
  });
});

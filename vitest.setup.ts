import '@testing-library/jest-dom/vitest';
import { afterEach, vi } from 'vitest';
import { cleanup } from '@testing-library/react';

afterEach(() => {
  cleanup();
});

// jsdom has no rAF-driven layout; the answer panel coalesces paints per frame
// (§9), so tests need rAF to actually fire. Run it as a macrotask so awaiting
// a tick flushes it.
if (typeof globalThis.requestAnimationFrame !== 'function') {
  globalThis.requestAnimationFrame = ((cb: FrameRequestCallback) =>
    setTimeout(() => cb(performance.now()), 0) as unknown as number) as typeof requestAnimationFrame;
  globalThis.cancelAnimationFrame = ((id: number) =>
    clearTimeout(id)) as typeof cancelAnimationFrame;
}

// jsdom does not implement scrollTo on elements; the answer panel's
// stick-to-bottom logic calls it.
if (!Object.getOwnPropertyDescriptor(Element.prototype, 'scrollTo')) {
  Element.prototype.scrollTo = vi.fn() as unknown as typeof Element.prototype.scrollTo;
}

// matchMedia backs the prefers-reduced-motion check.
if (typeof globalThis.matchMedia !== 'function') {
  globalThis.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: vi.fn(),
    removeListener: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })) as unknown as typeof matchMedia;
}

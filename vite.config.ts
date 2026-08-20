// vitest/config's defineConfig knows about the `test` key; vite's own does not,
// and using the wrong one fails `tsc --noEmit`.
import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

// Tauri serves the frontend from a fixed port in dev and from disk in release.
export default defineConfig({
  plugins: [react()],
  // Tauri expects a fixed port and fails if it is already taken.
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    // WebView2 on Windows 10/11 is evergreen Chromium; no legacy target needed.
    target: 'chrome105',
    sourcemap: false,
  },
  test: {
    globals: true,
    environment: 'jsdom',
    setupFiles: ['./vitest.setup.ts'],
    include: ['src/**/*.test.ts', 'src/**/*.test.tsx'],
    // jsdom + Testing Library on this machine is slow to boot; give it room
    // rather than letting a cold start read as a failure.
    testTimeout: 20_000,
    hookTimeout: 20_000,
    restoreMocks: true,
  },
});

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
    // WebView2 on Windows 10/11 is evergreen Chromium (120+ since late 2023);
    // a lower target only adds syntax transforms the runtime never needs (FE-6).
    target: 'chrome120',
    // The polyfill exists for browsers without <link rel="modulepreload">;
    // Chromium has had it for years, so it is dead bytes parsed on every
    // launch (FE-7).
    modulePreload: { polyfill: false },
    // Lazy views must stay separate chunks WITH their own CSS, so opening the
    // app never parses the Settings or Interview-prep styles.
    cssCodeSplit: true,
    // Compressed-size reporting gzips every chunk twice for a number nobody
    // reads: Tauri ships the files from disk, not over HTTP.
    reportCompressedSize: false,
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

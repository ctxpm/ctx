import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    // The emulator tests share one set of emulators.
    fileParallelism: false,
    testTimeout: 30_000,
    hookTimeout: 60_000,
  },
});

import { svelte } from '@sveltejs/vite-plugin-svelte';
import { configDefaults, defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  plugins: [svelte()],
  resolve: {
    conditions: ['browser'],
    alias: {
      $lib: fileURLToPath(new URL('./src/lib', import.meta.url))
    }
  },
  test: {
    environment: 'jsdom',
    // These standalone fixtures use node:test and are run by node --test.
    exclude: [
      ...configDefaults.exclude,
      'e2e/browser/support/visual-fixtures.test.mjs',
      'e2e/browser/support/performance-clock.test.mjs'
    ]
  }
});

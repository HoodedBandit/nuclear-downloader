/// <reference types="node" />

import { defineConfig } from 'vite';
import { sveltekit } from '@sveltejs/kit/vite';

const DEV_PORT = 1420;

const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(() => {
  return {
    plugins: [sveltekit()],

    // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
    clearScreen: false,
    server: {
      port: DEV_PORT,
      strictPort: true,
      host: host || false,
      hmr: host
        ? {
            protocol: 'ws',
            host,
            port: 1421
          }
        : undefined,
      watch: {
        ignored: ['**/src-tauri/**']
      }
    }
  };
});

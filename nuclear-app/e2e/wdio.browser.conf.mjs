import path from 'node:path';
import { createServer } from 'vite';

const devServerUrl = 'http://127.0.0.1:1420';
let viteServer;

export const config = {
  runner: 'local',
  specs: [path.resolve('e2e/browser/**/*.e2e.mjs')],
  maxInstances: 1,
  services: [
    [
      '@wdio/tauri-service',
      {
        mode: 'browser',
        devServerUrl
      }
    ]
  ],
  capabilities: [
    {
      browserName: 'tauri',
      timeouts: { script: 150_000 },
      'wdio:tauriServiceOptions': {
        mode: 'browser',
        devServerUrl
      }
    }
  ],
  framework: 'mocha',
  reporters: ['spec'],
  // Browser mode intentionally cannot query native Tauri window state; the
  // service logs that expected limitation at warn level before every command.
  logLevel: 'error',
  bail: 1,
  waitforTimeout: 10_000,
  connectionRetryTimeout: 90_000,
  connectionRetryCount: 1,
  mochaOpts: {
    ui: 'bdd',
    timeout: 150_000
  },
  onPrepare: async () => {
    viteServer = await createServer({
      mode: 'webdriver',
      optimizeDeps: {
        include: [
          '@tauri-apps/api/app',
          '@tauri-apps/api/core',
          '@tauri-apps/api/event',
          '@tauri-apps/plugin-dialog'
        ]
      },
      server: { host: '127.0.0.1', port: 1420, strictPort: true }
    });
    await viteServer.listen();
  },
  onComplete: async () => {
    await viteServer?.close();
  }
};

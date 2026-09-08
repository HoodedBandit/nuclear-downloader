import path from 'node:path';
import { lstatSync, realpathSync } from 'node:fs';
import { createServer } from 'vite';

const devServerUrl = 'http://127.0.0.1:1420';
const configuredProfileRoot = process.env.NUCLEAR_E2E_BROWSER_PROFILE_ROOT;
const configuredBrowserProfile = process.env.NUCLEAR_E2E_BROWSER_PROFILE;
if (
  !configuredProfileRoot ||
  !configuredBrowserProfile ||
  !path.isAbsolute(configuredProfileRoot) ||
  !path.isAbsolute(configuredBrowserProfile)
) {
  throw new Error('Renderer browser profile root and profile must be absolute directories.');
}
const profileRootMetadata = lstatSync(configuredProfileRoot);
if (!profileRootMetadata.isDirectory() || profileRootMetadata.isSymbolicLink()) {
  throw new Error('NUCLEAR_E2E_BROWSER_PROFILE_ROOT must be a regular directory.');
}
const profileRoot = realpathSync(configuredProfileRoot);
const browserProfileMetadata = lstatSync(configuredBrowserProfile);
const browserProfile = realpathSync(configuredBrowserProfile);
if (
  path.dirname(browserProfile) !== profileRoot ||
  !browserProfileMetadata.isDirectory() ||
  browserProfileMetadata.isSymbolicLink() ||
  !/^nuclear-renderer-[A-Za-z0-9][A-Za-z0-9-]{0,127}$/.test(path.basename(browserProfile))
) {
  throw new Error('NUCLEAR_E2E_BROWSER_PROFILE must be an owned child of its profile root.');
}
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
      'goog:chromeOptions': {
        args: [
          '--headless=new',
          '--window-size=1440,1000',
          '--no-first-run',
          '--disable-default-apps',
          `--user-data-dir=${browserProfile}`
        ]
      },
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

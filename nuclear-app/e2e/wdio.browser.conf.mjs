import path from 'node:path';
import { lstatSync, readFileSync, realpathSync } from 'node:fs';
import { createServer } from 'vite';

import { performanceClockIsolation } from './browser/support/performance-clock.mjs';

const devServerUrl = 'http://127.0.0.1:1420';
const deviceScaleFactor = Number(process.env.NUCLEAR_E2E_SCALE ?? '1');
if (![1, 1.5].includes(deviceScaleFactor)) {
  throw new Error('NUCLEAR_E2E_SCALE must be 1 or 1.5.');
}
const chromeBinary = process.env.NUCLEAR_E2E_CHROME_BINARY;
const chromeDriverBinary = process.env.NUCLEAR_E2E_CHROMEDRIVER_BINARY;
for (const executable of [chromeBinary, chromeDriverBinary].filter(Boolean)) {
  const metadata = lstatSync(executable);
  if (!path.isAbsolute(executable) || !metadata.isFile() || metadata.isSymbolicLink()) {
    throw new Error('Configured browser and driver must be absolute regular executable files.');
  }
}
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
const runId = process.env.NUCLEAR_E2E_RUN_ID;
if (runId) {
  if (!/^[0-9]{8}T[0-9]{6}Z-[a-f0-9]{32}$/.test(runId)) {
    throw new Error('NUCLEAR_E2E_RUN_ID is invalid.');
  }
  const markerPath = path.join(profileRoot, '.nuclear-renderer-profile-root.json');
  const markerMetadata = lstatSync(markerPath);
  if (!markerMetadata.isFile() || markerMetadata.isSymbolicLink() || markerMetadata.size > 1024) {
    throw new Error('Renderer profile ownership marker must be a small regular file.');
  }
  const marker = JSON.parse(readFileSync(markerPath, 'utf8'));
  if (
    marker?.schemaVersion !== 'renderer-profile-owner/v1' ||
    marker?.runId !== runId ||
    marker?.profile !== path.basename(browserProfile) ||
    Object.keys(marker).sort().join(',') !== 'profile,runId,schemaVersion'
  ) {
    throw new Error('Renderer profile ownership marker does not match this run.');
  }
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
      ...(process.env.NUCLEAR_E2E_CHROME_VERSION
        ? { browserVersion: process.env.NUCLEAR_E2E_CHROME_VERSION }
        : {}),
      ...(chromeDriverBinary
        ? {
            'wdio:chromedriverOptions': {
              binary: chromeDriverBinary,
              allowedIps: ['127.0.0.1'],
              spawnOpts: { windowsHide: true }
            }
          }
        : {}),
      timeouts: { script: 150_000 },
      'goog:chromeOptions': {
        ...(chromeBinary ? { binary: chromeBinary } : {}),
        args: [
          '--headless=new',
          '--window-size=1440,1000',
          '--no-first-run',
          '--disable-default-apps',
          `--force-device-scale-factor=${deviceScaleFactor}`,
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
  // A loader failure must stop the run, rather than repeatedly launching children.
  connectionRetryCount: 0,
  mochaOpts: {
    ui: 'bdd',
    timeout: 150_000
  },
  onPrepare: async () => {
    try {
      viteServer = await createServer({
        mode: 'webdriver',
        plugins: [performanceClockIsolation()],
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
    } catch (error) {
      await viteServer?.close();
      viteServer = undefined;
      throw error;
    }
  },
  onComplete: async () => {
    await viteServer?.close();
  }
};

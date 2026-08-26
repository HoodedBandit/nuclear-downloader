import { accessSync, constants } from 'node:fs';
import path from 'node:path';

const appBinaryPath = path.resolve(process.env.NUCLEAR_E2E_APP_BINARY ?? '');
if (!process.env.NUCLEAR_E2E_APP_BINARY) {
  throw new Error('NUCLEAR_E2E_APP_BINARY must name the exact x64 application executable.');
}
accessSync(appBinaryPath, constants.R_OK);

const suite = process.env.NUCLEAR_E2E_NATIVE_SUITE ?? 'full';
const specs =
  suite === 'smoke'
    ? [path.resolve('e2e/native/smoke.e2e.mjs')]
    : suite === 'restart'
      ? [path.resolve('e2e/native/restart.e2e.mjs')]
      : suite === 'full'
        ? [path.resolve('e2e/native/workflows.e2e.mjs')]
        : (() => {
            throw new Error(`Unknown NUCLEAR_E2E_NATIVE_SUITE: ${suite}`);
          })();

export const config = {
  runner: 'local',
  specs,
  maxInstances: 1,
  services: [
    [
      '@wdio/tauri-service',
      {
        appBinaryPath,
        driverProvider: 'external',
        autoInstallTauriDriver: false,
        autoDownloadEdgeDriver: true,
        captureBackendLogs: true,
        captureFrontendLogs: true,
        startTimeout: 60_000,
        commandTimeout: 30_000
      }
    ]
  ],
  capabilities: [
    {
      browserName: 'tauri',
      'tauri:options': { application: appBinaryPath }
    }
  ],
  framework: 'mocha',
  reporters: ['spec'],
  logLevel: 'info',
  bail: 1,
  waitforTimeout: 30_000,
  connectionRetryTimeout: 120_000,
  connectionRetryCount: 1,
  mochaOpts: {
    ui: 'bdd',
    timeout: 10 * 60_000
  }
};

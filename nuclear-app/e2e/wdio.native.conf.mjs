import { accessSync, constants } from 'node:fs';
import path from 'node:path';
import { launcher as TauriLauncher } from '@wdio/tauri-service';

const appBinaryPath = path.resolve(process.env.NUCLEAR_E2E_APP_BINARY ?? '');
if (!process.env.NUCLEAR_E2E_APP_BINARY) {
  throw new Error('NUCLEAR_E2E_APP_BINARY must name the exact x64 application executable.');
}
accessSync(appBinaryPath, constants.R_OK);

const webviewDataFolder = process.env.NUCLEAR_E2E_WEBVIEW_DATA_FOLDER;
if (process.platform === 'win32' && !webviewDataFolder) {
  throw new Error(
    'NUCLEAR_E2E_WEBVIEW_DATA_FOLDER must name the exact EdgeDriver automation folder on Windows.'
  );
}
if (webviewDataFolder && !path.isAbsolute(webviewDataFolder)) {
  throw new Error('NUCLEAR_E2E_WEBVIEW_DATA_FOLDER must be an absolute path.');
}

const tauriOptions = { application: appBinaryPath };
if (webviewDataFolder) {
  // Tauri's Windows profile stores DevToolsActivePort in its EBWebView child.
  // Supply that child to EdgeDriver instead of the identifier-based parent.
  tauriOptions.webviewOptions = { userDataFolder: webviewDataFolder };
}

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
      // Exact release binaries exclude tauri-plugin-wdio. Use the official service's
      // launcher only; its worker service assumes the optional plugin is present and
      // otherwise polls on every element lookup for APIs these acceptance tests do not use.
      TauriLauncher,
      {
        appBinaryPath,
        driverProvider: 'external',
        autoInstallTauriDriver: false,
        autoDownloadEdgeDriver: true,
        // The release binary intentionally excludes tauri-plugin-wdio. The acceptance
        // wrapper retains the driver and runner streams without modifying shipped code.
        captureBackendLogs: false,
        captureFrontendLogs: false,
        startTimeout: 60_000,
        commandTimeout: 30_000
      }
    ]
  ],
  capabilities: [
    {
      browserName: 'tauri',
      'tauri:options': tauriOptions
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

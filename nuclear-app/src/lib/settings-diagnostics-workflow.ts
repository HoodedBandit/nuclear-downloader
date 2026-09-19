import type { WorkflowCommands } from './frontend-workflow-ports';
import type { BrowserName, CookieConfig, OutputFormat, QueueItem } from './frontend-types';
import type { UiErrorReporter } from './ui-error-reporter';
import type { StartupSubsystem, StartupSubsystemState } from './startup-state';

export interface SettingsDiagnosticsState {
  outputDir: string;
  outputDirValidated: boolean;
  outputDirError: string | null;
  globalQuality: string;
  globalFormat: OutputFormat;
  useCookies: boolean;
  cookieMode: 'browser' | 'file';
  cookieBrowser: BrowserName;
  cookieFilePath: string;
  compatConfigPath: string;
  accessError: string | null;
  diagnosticsMessage: string | null;
  diagnosticsError: string | null;
}

export function createSettingsDiagnosticsState(): SettingsDiagnosticsState {
  return {
    outputDir: '',
    outputDirValidated: false,
    outputDirError: null,
    globalQuality: 'best',
    globalFormat: 'mp4',
    useCookies: false,
    cookieMode: 'browser',
    cookieBrowser: 'firefox',
    cookieFilePath: '',
    compatConfigPath: '',
    accessError: null,
    diagnosticsMessage: null,
    diagnosticsError: null
  };
}

export type PathSelection = string | string[] | null;

export interface SettingsDiagnosticsDialogs {
  open: (options: {
    directory?: boolean;
    multiple?: boolean;
    filters?: Array<{ name: string; extensions: string[] }>;
  }) => Promise<PathSelection>;
  save: (options: {
    defaultPath: string;
    filters: Array<{ name: string; extensions: string[] }>;
  }) => Promise<string | null>;
}

export interface QueueSettingsPort {
  getItems: () => QueueItem[];
  updateQueueItemSettings: (item: QueueItem, input: { outputDir: string }) => Promise<void>;
}

export interface SettingsDiagnosticsStartupPort {
  setSubsystem: (subsystem: StartupSubsystem, state: StartupSubsystemState) => void;
}

export interface SettingsDiagnosticsUi {
  dialogs: SettingsDiagnosticsDialogs;
  confirm: (message: string) => boolean;
}

export interface SettingsDiagnosticsWorkflowOptions {
  errors: UiErrorReporter;
  commands: WorkflowCommands;
  ui: SettingsDiagnosticsUi;
  queue: QueueSettingsPort;
  startup: SettingsDiagnosticsStartupPort;
}

export function getPathBasename(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

export function pickFirstPath(selection: PathSelection): string | null {
  if (typeof selection === 'string') return selection;
  if (Array.isArray(selection)) return selection[0] ?? null;
  return null;
}

export class SettingsDiagnosticsWorkflow {
  constructor(
    readonly state: SettingsDiagnosticsState,
    private readonly options: SettingsDiagnosticsWorkflowOptions
  ) {}

  getCookieConfig(): CookieConfig | null {
    const { state } = this;
    if (!state.useCookies) return null;
    return {
      enabled: true,
      mode: state.cookieMode,
      browser: state.cookieBrowser,
      cookie_file: state.cookieMode === 'file' ? state.cookieFilePath || null : null
    };
  }

  getCookieConfigSnapshot(): CookieConfig | null {
    const config = this.getCookieConfig();
    return config ? { ...config } : null;
  }

  getCompatConfigSnapshot(): string | null {
    const value = this.state.compatConfigPath.trim();
    return value ? value : null;
  }

  async validateOutputDirectory(candidate: string): Promise<boolean> {
    const { commands } = this.options;
    const { state } = this;
    if (!commands.isActive()) return false;
    state.outputDirError = null;
    const attempt = this.options.errors.begin('folder', 'Validating the download folder');
    if (!candidate.trim()) {
      state.outputDirValidated = false;
      state.outputDirError = attempt.fail('Choose a writable output folder before downloading.');
      return false;
    }
    try {
      const validated = await commands.invoke('validate_output_directory', { path: candidate });
      if (!commands.isActive()) return false;
      state.outputDir = validated;
      state.outputDirValidated = true;
      return true;
    } catch (error) {
      if (!commands.isActive()) return false;
      state.outputDirValidated = false;
      state.outputDirError = attempt.fail(error);
      return false;
    }
  }

  async initializeOutputDirectory(): Promise<void> {
    const { commands, startup } = this.options;
    const { state } = this;
    try {
      const candidate = await commands.invoke('default_download_dir');
      if (!commands.isActive()) return;
      state.outputDir = candidate;
      if (await this.validateOutputDirectory(candidate)) {
        if (!commands.isActive()) return;
        startup.setSubsystem('outputDirectory', 'ready');
      } else {
        if (!commands.isActive()) return;
        startup.setSubsystem('outputDirectory', 'error');
      }
    } catch (error) {
      if (!commands.isActive()) return;
      state.outputDir = '';
      state.outputDirValidated = false;
      state.outputDirError = 'Choose a writable output folder before downloading.';
      this.options.errors.begin('folder', 'Finding the download folder').fail(error);
      startup.setSubsystem('outputDirectory', 'error');
    }
  }

  async browseCookieFile(): Promise<void> {
    const file = await this.choosePath('accessError', 'cookies', 'Choosing a cookie file', () =>
      this.options.ui.dialogs.open({
        filters: [{ name: 'Cookie Files', extensions: ['txt'] }]
      })
    );
    if (!this.options.commands.isActive()) return;
    if (file) this.state.cookieFilePath = file;
  }

  async browseCompatConfigFile(): Promise<void> {
    const file = await this.choosePath(
      'accessError',
      'compatibility',
      'Choosing a compatibility configuration',
      () =>
        this.options.ui.dialogs.open({
          multiple: false,
          filters: [{ name: 'yt-dlp config', extensions: ['conf', 'txt'] }]
        })
    );
    if (!this.options.commands.isActive()) return;
    if (file) this.state.compatConfigPath = file;
  }

  async browseOutputDir(): Promise<void> {
    const dir = await this.choosePath(
      'outputDirError',
      'folder',
      'Choosing a download folder',
      () => this.options.ui.dialogs.open({ directory: true })
    );
    if (!this.options.commands.isActive() || !dir) return;
    this.state.outputDir = dir;
    if (await this.validateOutputDirectory(dir)) {
      if (!this.options.commands.isActive()) return;
      this.options.startup.setSubsystem('outputDirectory', 'ready');
      await Promise.all(
        this.options.queue
          .getItems()
          .filter((item) => item.status === 'ready')
          .map((item) =>
            this.options.queue.updateQueueItemSettings(item, {
              outputDir: this.state.outputDir
            })
          )
      );
      if (!this.options.commands.isActive()) return;
    } else {
      this.options.startup.setSubsystem('outputDirectory', 'error');
    }
  }

  async exportDiagnostics(): Promise<void> {
    const { commands } = this.options;
    const { state } = this;
    state.diagnosticsError = null;
    state.diagnosticsMessage = null;
    const attempt = this.options.errors.begin('diagnostics', 'Exporting diagnostics');
    try {
      const destination = await this.options.ui.dialogs.save({
        defaultPath: 'nuclear-downloader-diagnostics.jsonl',
        filters: [{ name: 'JSON Lines', extensions: ['jsonl'] }]
      });
      if (!commands.isActive() || !destination) return;
      await commands.invoke('export_diagnostics', { destination });
      if (!commands.isActive()) return;
      state.diagnosticsMessage = 'Diagnostics exported successfully.';
    } catch (error) {
      if (!commands.isActive()) return;
      state.diagnosticsError = attempt.fail(error);
    }
  }

  async clearDiagnostics(): Promise<void> {
    const { commands } = this.options;
    const { state } = this;
    state.diagnosticsError = null;
    state.diagnosticsMessage = null;
    const attempt = this.options.errors.begin('diagnostics', 'Clearing diagnostics');
    if (!this.options.ui.confirm('Clear all local Nuclear Downloader diagnostics logs?')) return;
    try {
      await commands.invoke('clear_diagnostics');
      if (!commands.isActive()) return;
      state.diagnosticsMessage = 'Local diagnostics were cleared.';
    } catch (error) {
      if (!commands.isActive()) return;
      state.diagnosticsError = attempt.fail(error);
    }
  }

  private async choosePath(
    field: 'outputDirError' | 'accessError',
    source: string,
    context: string,
    choose: () => Promise<PathSelection>
  ): Promise<string | null> {
    this.state[field] = null;
    const attempt = this.options.errors.begin(source, context);
    try {
      const selected = pickFirstPath(await choose());
      return this.options.commands.isActive() ? selected : null;
    } catch (error) {
      if (this.options.commands.isActive()) this.state[field] = attempt.fail(error);
      return null;
    }
  }
}

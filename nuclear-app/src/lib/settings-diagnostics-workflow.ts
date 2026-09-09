import type { DownloaderRuntimeStatus } from './bindings/DownloaderRuntimeStatus';
import { normalizeAppError } from './frontend-errors';
import type { WorkflowCommands } from './frontend-workflow-ports';
import type { BrowserName, CookieConfig, OutputFormat, QueueItem } from './frontend-types';
import { redactDiagnosticText } from './queue-logic';
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
  reportIssue: (label: string, error: unknown) => void;
}

export interface SettingsDiagnosticsUi {
  dialogs: SettingsDiagnosticsDialogs;
  confirm: (message: string) => boolean;
  clipboard: { writeText: (text: string) => Promise<void> };
}

export interface SettingsDiagnosticsWorkflowOptions {
  commands: WorkflowCommands;
  ui: SettingsDiagnosticsUi;
  queue: QueueSettingsPort;
  startup: SettingsDiagnosticsStartupPort;
  getRuntimeStatus: () => DownloaderRuntimeStatus | null;
  getQueueItemDisplayTitle: (item: QueueItem) => string;
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
    if (!candidate.trim()) {
      state.outputDirValidated = false;
      state.outputDirError = 'Choose a writable output folder before downloading.';
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
      state.outputDirError = normalizeAppError(error);
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
        startup.reportIssue('Output folder', state.outputDirError ?? 'Invalid folder');
        startup.setSubsystem('outputDirectory', 'error');
      }
    } catch (error) {
      if (!commands.isActive()) return;
      state.outputDir = '';
      state.outputDirValidated = false;
      state.outputDirError = 'Choose a writable output folder before downloading.';
      startup.reportIssue('Output folder discovery', error);
      startup.setSubsystem('outputDirectory', 'error');
    }
  }

  async browseCookieFile(): Promise<void> {
    const file = pickFirstPath(
      await this.options.ui.dialogs.open({
        filters: [{ name: 'Cookie Files', extensions: ['txt'] }]
      })
    );
    if (!this.options.commands.isActive()) return;
    if (file) this.state.cookieFilePath = file;
  }

  async browseCompatConfigFile(): Promise<void> {
    const file = pickFirstPath(
      await this.options.ui.dialogs.open({
        multiple: false,
        filters: [{ name: 'yt-dlp config', extensions: ['conf', 'txt'] }]
      })
    );
    if (!this.options.commands.isActive()) return;
    if (file) this.state.compatConfigPath = file;
  }

  async browseOutputDir(): Promise<void> {
    const dir = pickFirstPath(await this.options.ui.dialogs.open({ directory: true }));
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

  buildDiagnostics(item: QueueItem): string {
    const runtimeStatus = this.options.getRuntimeStatus();
    const runtimeLines =
      runtimeStatus?.tools
        .map(
          (tool) =>
            `${tool.name}: ${tool.available ? (tool.version ?? 'available') : 'missing'} (${tool.source})`
        )
        .join('\n') ?? 'Runtime status unavailable';
    return redactDiagnosticText(
      [
        `Title: ${this.options.getQueueItemDisplayTitle(item)}`,
        `Format: ${item.format}`,
        `Quality: ${item.quality}`,
        `Status: ${item.status}`,
        `Phase: ${item.phase ?? 'n/a'}`,
        `Error code: ${item.errorCode ?? 'n/a'}`,
        `Error: ${item.error ?? 'n/a'}`,
        '',
        'Detail:',
        redactDiagnosticText(item.errorDetail ?? 'No backend detail captured.'),
        '',
        'Runtime:',
        runtimeLines,
        runtimeStatus?.message ? `Runtime message: ${runtimeStatus.message}` : ''
      ]
        .filter((line) => line !== '')
        .join('\n')
    );
  }

  async copyDiagnostics(item: QueueItem): Promise<void> {
    await this.options.ui.clipboard.writeText(this.buildDiagnostics(item));
  }

  async exportDiagnostics(): Promise<void> {
    const { commands } = this.options;
    const { state } = this;
    state.diagnosticsError = null;
    state.diagnosticsMessage = null;
    const destination = await this.options.ui.dialogs.save({
      defaultPath: 'nuclear-downloader-diagnostics.jsonl',
      filters: [{ name: 'JSON Lines', extensions: ['jsonl'] }]
    });
    if (!commands.isActive() || !destination) return;
    try {
      await commands.invoke('export_diagnostics', { destination });
      if (!commands.isActive()) return;
      state.diagnosticsMessage = 'Diagnostics exported successfully.';
    } catch (error) {
      if (!commands.isActive()) return;
      state.diagnosticsError = normalizeAppError(error);
    }
  }

  async clearDiagnostics(): Promise<void> {
    const { commands } = this.options;
    const { state } = this;
    state.diagnosticsError = null;
    state.diagnosticsMessage = null;
    if (!this.options.ui.confirm('Clear all local Nuclear Downloader diagnostics logs?')) return;
    try {
      await commands.invoke('clear_diagnostics');
      if (!commands.isActive()) return;
      state.diagnosticsMessage = 'Local diagnostics were cleared.';
    } catch (error) {
      if (!commands.isActive()) return;
      state.diagnosticsError = normalizeAppError(error);
    }
  }
}

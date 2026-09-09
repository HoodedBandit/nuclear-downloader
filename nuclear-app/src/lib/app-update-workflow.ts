import type { UpdateCheckResult } from './bindings/UpdateCheckResult';
import { normalizeAppError } from './frontend-errors';
import type { UpdateInstallProgressPayload } from './frontend-types';
import type { OperationWorkflow } from './frontend-workflow-ports';
import type { StartupSubsystemState } from './startup-state';

export interface AppUpdateWorkflowState {
  appVersion: string | null;
  checkState: 'idle' | 'checking';
  info: UpdateCheckResult | null;
  modalOpen: boolean;
  error: string | null;
  installProgress: UpdateInstallProgressPayload | null;
  installRunning: boolean;
}

export interface AppUpdateWorkflowDependencies extends OperationWorkflow {
  getVersion: () => Promise<string>;
  runtimeUpdateRunning: () => boolean;
  hasUpdateBlockingWork: () => boolean;
  setAppVersionStartup: (state: StartupSubsystemState) => void;
  setUpdateCheckStartup: (state: StartupSubsystemState) => void;
  reportStartupIssue: (subsystem: string, error: unknown) => void;
  appendStartupIssue: (message: string) => void;
}

export function createAppUpdateWorkflowState(): AppUpdateWorkflowState {
  return {
    appVersion: null,
    checkState: 'idle',
    info: null,
    modalOpen: false,
    error: null,
    installProgress: null,
    installRunning: false
  };
}

export class AppUpdateWorkflowController {
  constructor(
    readonly state: AppUpdateWorkflowState,
    private readonly dependencies: AppUpdateWorkflowDependencies
  ) {}

  async initializeAppVersion(): Promise<void> {
    try {
      const version = await this.dependencies.getVersion();
      if (!this.dependencies.isActive()) return;
      this.state.appVersion = version;
      this.dependencies.setAppVersionStartup('ready');
    } catch (error) {
      if (!this.dependencies.isActive()) return;
      this.state.appVersion = null;
      this.dependencies.reportStartupIssue('App version', error);
      this.dependencies.setAppVersionStartup('degraded');
    }
  }

  async initializeUpdateCheck(): Promise<void> {
    const succeeded = await this.check({ openModal: false, showErrors: false });
    if (!this.dependencies.isActive()) return;
    if (!succeeded)
      this.dependencies.appendStartupIssue(
        'App update check was unavailable; downloads remain usable.'
      );
    this.dependencies.setUpdateCheckStartup(succeeded ? 'ready' : 'degraded');
  }

  async check(options: { openModal: boolean; showErrors: boolean }): Promise<boolean> {
    if (this.state.checkState === 'checking') {
      if (options.openModal) this.state.modalOpen = true;
      return false;
    }
    this.state.checkState = 'checking';
    if (options.openModal) this.state.modalOpen = true;
    if (options.showErrors) this.state.error = null;
    if (!this.state.installRunning) this.state.installProgress = null;
    try {
      const result = await this.dependencies.invoke('check_app_update');
      if (!this.dependencies.isActive()) return false;
      this.state.info = result;
      this.state.appVersion = result.currentVersion;
      return true;
    } catch (error) {
      if (!this.dependencies.isActive()) return false;
      if (options.showErrors) this.state.error = normalizeAppError(error);
      return false;
    } finally {
      if (this.dependencies.isActive()) this.state.checkState = 'idle';
    }
  }

  openModal(): void {
    this.state.modalOpen = true;
  }
  closeModal(): void {
    if (!this.state.installRunning) this.state.modalOpen = false;
  }

  async install(): Promise<void> {
    const targetVersion = this.state.info?.latestVersion;
    if (!targetVersion || !this.state.info?.hasUpdate || this.state.installRunning) return;
    if (this.dependencies.runtimeUpdateRunning()) {
      this.state.error = 'Wait for the downloader runtime update to finish.';
      this.state.modalOpen = true;
      return;
    }
    if (this.dependencies.hasUpdateBlockingWork()) {
      this.state.error = 'Finish or cancel queued downloads before installing the app update.';
      this.state.modalOpen = true;
      return;
    }
    this.state.error = null;
    this.state.modalOpen = true;
    this.state.installRunning = true;
    this.state.installProgress = {
      status: 'downloading',
      version: targetVersion,
      downloadedBytes: 0,
      totalBytes: null,
      message: 'Preparing update download...'
    };
    try {
      const result = await this.dependencies.invoke('begin_app_update', {
        expectedVersion: targetVersion
      });
      if (!this.dependencies.isActive()) return;
      const operation = await this.dependencies.waitForOperation(result.operationId);
      if (!this.dependencies.isActive()) return;
      if (operation.state === 'failed')
        throw operation.error ?? new Error('Application update failed.');
    } catch (error) {
      if (!this.dependencies.isActive()) return;
      this.state.error = normalizeAppError(error);
    } finally {
      if (this.dependencies.isActive()) this.state.installRunning = false;
    }
  }

  applyProgress(progress: UpdateInstallProgressPayload): void {
    this.state.installProgress = progress;
    if (progress.status === 'error') {
      this.state.installRunning = false;
      this.state.error = progress.message ?? 'Update installation failed.';
    }
  }

  downloadPercent(): number {
    const progress = this.state.installProgress;
    if (!progress) return 0;
    if (progress.status === 'launching') return 100;
    if (!progress.totalBytes || progress.totalBytes <= 0) return 0;
    return Math.max(0, Math.min(100, (progress.downloadedBytes / progress.totalBytes) * 100));
  }
}

export function formatPublishedAt(value: string | null): string {
  if (!value) return 'Unknown';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit'
  }).format(date);
}

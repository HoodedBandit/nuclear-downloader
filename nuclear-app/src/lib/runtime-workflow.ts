import type { RuntimeReadiness } from './bindings/RuntimeReadiness';
import type { DownloaderRuntimeStatus } from './bindings/DownloaderRuntimeStatus';
import type { DownloaderRuntimeUpdateCheck } from './bindings/DownloaderRuntimeUpdateCheck';
import type { DownloaderToolStatus } from './bindings/DownloaderToolStatus';
import { normalizeAppError } from './frontend-errors';
import type { DownloaderRuntimeUpdateProgressPayload } from './frontend-types';
import type { OperationWorkflow } from './frontend-workflow-ports';
import {
  runtimeAllowsDownloads,
  runtimeStartupSubsystemState,
  type StartupSubsystemState
} from './startup-state';

export interface RuntimeWorkflowState {
  status: DownloaderRuntimeStatus | null;
  updateCheck: DownloaderRuntimeUpdateCheck | null;
  checkState: 'checking' | 'idle';
  updateRunning: boolean;
  updateProgress: DownloaderRuntimeUpdateProgressPayload | null;
  error: string | null;
}

export interface RuntimeWorkflowDependencies extends OperationWorkflow {
  appUpdateRunning: () => boolean;
  hasUpdateBlockingWork: () => boolean;
  backendReadiness: () => RuntimeReadiness | null;
  setStartupSubsystem: (state: StartupSubsystemState) => void;
  reportStartupIssue: (subsystem: string, error: unknown) => void;
}

export function createRuntimeWorkflowState(): RuntimeWorkflowState {
  return {
    status: null,
    updateCheck: null,
    checkState: 'checking',
    updateRunning: false,
    updateProgress: null,
    error: null
  };
}

export class RuntimeWorkflowController {
  constructor(
    readonly state: RuntimeWorkflowState,
    private readonly dependencies: RuntimeWorkflowDependencies
  ) {}

  async initialize(): Promise<void> {
    await this.refresh();
    if (!this.dependencies.isActive()) return;
    if (!this.state.status) {
      this.dependencies.reportStartupIssue('Downloader runtime', this.state.error ?? 'Unavailable');
    }
    void this.checkForUpdate();
  }

  async refresh(): Promise<void> {
    if (!this.dependencies.isActive()) return;
    this.state.checkState = 'checking';
    this.state.error = null;
    try {
      const status = await this.dependencies.invoke('check_downloader_runtime');
      if (!this.dependencies.isActive()) return;
      this.state.status = status;
      this.dependencies.setStartupSubsystem(runtimeStartupSubsystemState(status.state));
    } catch (error) {
      if (!this.dependencies.isActive()) return;
      this.state.status = null;
      this.state.error = normalizeAppError(error);
      this.dependencies.setStartupSubsystem(runtimeStartupSubsystemState(null));
    } finally {
      if (this.dependencies.isActive()) this.state.checkState = 'idle';
    }
  }

  async checkForUpdate(): Promise<void> {
    try {
      const updateCheck = await this.dependencies.invoke('check_runtime_update');
      if (!this.dependencies.isActive()) return;
      this.state.updateCheck = updateCheck;
    } catch {
      if (!this.dependencies.isActive()) return;
      this.state.updateCheck = null;
    }
  }

  async update(): Promise<void> {
    if (this.dependencies.appUpdateRunning()) {
      this.state.error = 'Wait for the app update operation to finish.';
      return;
    }
    if (this.dependencies.hasUpdateBlockingWork()) {
      this.state.error = 'Finish or cancel queued downloads before updating the runtime.';
      return;
    }

    this.state.updateRunning = true;
    this.state.error = null;
    this.state.updateProgress = {
      status: 'checking',
      version: this.state.updateCheck?.latestRuntimeVersion ?? null,
      downloadedBytes: 0,
      totalBytes: null,
      message: 'Checking downloader runtime release...'
    };

    try {
      const result = await this.dependencies.invoke('begin_runtime_update');
      if (!this.dependencies.isActive()) return;
      const operation = await this.dependencies.waitForOperation(result.operationId);
      if (!this.dependencies.isActive()) return;
      if (operation.state === 'failed') {
        throw operation.error ?? new Error('Downloader runtime update failed.');
      }
    } catch (error) {
      if (!this.dependencies.isActive()) return;
      this.state.error = normalizeAppError(error);
    } finally {
      if (this.dependencies.isActive()) {
        this.state.updateRunning = false;
        await this.refresh();
        if (this.dependencies.isActive()) await this.checkForUpdate();
      }
    }
  }

  applyProgress(progress: DownloaderRuntimeUpdateProgressPayload): void {
    this.state.updateProgress = progress;
    if (progress.status === 'error') {
      this.state.updateRunning = false;
      this.state.error = progress.message ?? 'Downloader runtime update failed.';
    }
  }

  canDownload(): boolean {
    return runtimeAllowsDownloads(
      this.state.status?.state ?? null,
      this.dependencies.backendReadiness()
    );
  }

  getUpdatePercent(): number {
    const progress = this.state.updateProgress;
    if (!progress) return 0;
    if (progress.status === 'complete') return 100;
    if (!progress.totalBytes || progress.totalBytes <= 0) return 0;
    return Math.max(0, Math.min(100, (progress.downloadedBytes / progress.totalBytes) * 100));
  }

  getTool(name: string): DownloaderToolStatus | null {
    return this.state.status?.tools.find((tool) => tool.name === name) ?? null;
  }

  badgeClass(): string {
    if (!this.state.status || this.state.checkState === 'checking') return 'neutral';
    if (this.state.status.state === 'ready') return 'ok';
    if (this.state.status.state === 'ready_with_warnings') return 'warn';
    return 'err';
  }

  missingRequiredTools(): string[] {
    return (
      this.state.status?.tools
        .filter((tool) => tool.required && !tool.available)
        .map((tool) => tool.name) ?? []
    );
  }

  badgeText(): string {
    if (!this.state.status || this.state.checkState === 'checking') return 'Runtime checking';
    const ytDlp = this.getTool('yt-dlp');
    const deno = this.getTool('deno');
    const missingRequired = this.missingRequiredTools();
    const parts = [
      `Runtime ${this.state.status.state}`,
      missingRequired.length > 0 ? `Missing ${missingRequired.join(', ')}` : null,
      runtimeToolBadgeLabel(ytDlp, 'yt-dlp'),
      runtimeToolBadgeLabel(deno, 'Deno') ?? 'No Deno'
    ].filter(Boolean);
    return parts.join(' | ');
  }

  badgeTitle(): string {
    if (!this.state.status) return this.state.error ?? '';
    const toolLines = this.state.status.tools.map((tool) => {
      const state = tool.available
        ? (tool.version ?? 'available')
        : `missing${tool.error ? `: ${tool.error}` : ''}`;
      const source = [tool.source, tool.path].filter(Boolean).join(' ');
      return `${tool.name}: ${state} (${source})`;
    });
    return [this.state.status.message, ...toolLines].filter(Boolean).join('\n');
  }
}

export function compactRuntimeToolVersion(tool: DownloaderToolStatus): string | null {
  const version = tool.version?.trim();
  if (!version) return null;
  const firstLine = version.split(/\r?\n/)[0]?.trim() ?? '';
  if (!firstLine) return null;
  if (tool.name === 'deno') return firstLine.match(/^deno\s+([^\s]+)/i)?.[1] ?? firstLine;
  if (tool.name === 'ffmpeg')
    return firstLine.match(/^ffmpeg version\s+([^\s]+)/i)?.[1] ?? firstLine;
  if (tool.name === 'ffprobe')
    return firstLine.match(/^ffprobe version\s+([^\s]+)/i)?.[1] ?? firstLine;
  return firstLine.split(/\s+/)[0] ?? firstLine;
}

export function runtimeToolBadgeLabel(
  tool: DownloaderToolStatus | null,
  displayName: string
): string | null {
  if (!tool || !tool.available) return null;
  const version = compactRuntimeToolVersion(tool);
  return version ? `${displayName} ${version}` : displayName;
}

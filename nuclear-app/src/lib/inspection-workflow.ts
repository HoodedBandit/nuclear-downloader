import type { QueueItemRecord } from './bindings/QueueItemRecord';
import type { UrlInspection } from './bindings/UrlInspection';
import type { VideoInfo } from './bindings/VideoInfo';
import { normalizeAppError, normalizeDownloadError } from './frontend-errors';
import type {
  CookieConfig,
  OutputFormat,
  PlaylistModal,
  PlaylistModalEntry,
  QueueItem
} from './frontend-types';
import type { OperationWorkflow } from './frontend-workflow-ports';
import { resolveAvailableFormat, resolveAvailableQuality } from './queue-logic';

const PLAYLIST_PAGE_SIZE = 100;
const INSPECTION_TIMEOUT_MS = 5 * 60 * 1000;

export interface InspectionState {
  urlInput: string;
  urlError: string;
  playlistLoading: boolean;
  activeInspectionId: string | null;
  cancelRequested: boolean;
  playlistModal: PlaylistModal | null;
  playlistPage: number;
}

export function createInspectionState(): InspectionState {
  return {
    urlInput: '',
    urlError: '',
    playlistLoading: false,
    activeInspectionId: null,
    cancelRequested: false,
    playlistModal: null,
    playlistPage: 0
  };
}

export interface InspectionSettings {
  canStartDownloads: boolean;
  startupState: 'loading' | 'ready' | 'degraded' | 'error';
  runtimeCanDownload: boolean;
  outputDirError: string | null;
  globalFormat: OutputFormat;
  globalQuality: string;
  outputDir: string;
}

export interface InspectionQueuePort {
  getItems: () => readonly QueueItem[];
  retainMetadata: (
    url: string,
    metadata: Pick<QueueItem, 'duration' | 'channel' | 'thumbnail'>
  ) => () => void;
}

export interface InspectionWorkflowDependencies extends OperationWorkflow {
  getSettings: () => InspectionSettings;
  readCookie: () => CookieConfig | null;
  readCompat: () => string | null;
  queue: InspectionQueuePort;
  setQueueActionError: (message: string) => void;
}

interface CompletedInspection {
  operationId: string;
  inspection: UrlInspection;
}

export class InspectionWorkflow {
  constructor(
    readonly state: InspectionState,
    private readonly dependencies: InspectionWorkflowDependencies
  ) {}

  private active(): boolean {
    return this.dependencies.isActive();
  }

  private queueUrls(): Set<string> {
    return new Set(this.dependencies.queue.getItems().map((item) => item.url));
  }

  private async inspect(
    url: string,
    cookieConfig: CookieConfig | null
  ): Promise<CompletedInspection> {
    const result = await this.dependencies.invoke('begin_inspection', {
      input: { url, cookieConfig, compatConfigPath: this.dependencies.readCompat() }
    });
    if (!this.active()) throw this.dependencies.unloadedError;
    this.state.activeInspectionId = result.operationId;
    const operation = await this.dependencies.waitForOperation(
      result.operationId,
      INSPECTION_TIMEOUT_MS
    );
    if (!this.active()) throw this.dependencies.unloadedError;
    if (operation.state === 'cancelled') throw new Error('URL inspection was cancelled.');
    if (operation.state !== 'completed' || !operation.inspectionResult) {
      throw operation.error ?? new Error('URL inspection did not produce a result.');
    }
    return { operationId: result.operationId, inspection: operation.inspectionResult };
  }

  private async admit(
    info: VideoInfo,
    inspectionOperationId: string,
    cookieConfig: CookieConfig | null
  ): Promise<QueueItemRecord> {
    const discardMetadata = this.dependencies.queue.retainMetadata(info.url, {
      duration: info.duration,
      channel: info.channel,
      thumbnail: info.thumbnail
    });
    const availableQualities = ['best', ...info.available_qualities];
    const settings = this.dependencies.getSettings();
    try {
      return await this.dependencies.invoke('add_inspection_result_to_queue', {
        input: {
          inspectionOperationId,
          format: resolveAvailableFormat(settings.globalFormat, info.has_audio, 'mp4'),
          quality: resolveAvailableQuality(settings.globalQuality, availableQualities),
          outputDir: settings.outputDir,
          cookieConfig,
          filenameOverride: null,
          compatConfigPath: this.dependencies.readCompat()
        }
      });
    } catch (error) {
      discardMetadata();
      if (!this.active()) throw error;
      await this.dependencies
        .invoke('dismiss_operation', { operationId: inspectionOperationId })
        .catch(() => undefined);
      throw error;
    }
  }

  async addToQueue(): Promise<void> {
    if (this.state.playlistLoading) return;
    this.state.urlError = '';
    const url = this.state.urlInput.trim();
    if (!url) return;

    const settings = this.dependencies.getSettings();
    if (!settings.canStartDownloads) {
      this.state.urlError =
        settings.startupState === 'error'
          ? 'Work is disabled because renderer event delivery could not be initialized. Restart the app.'
          : !settings.runtimeCanDownload
            ? 'Downloader runtime is not ready.'
            : (settings.outputDirError ?? 'Choose a validated output folder before adding work.');
      return;
    }

    let parsedUrl: URL;
    try {
      parsedUrl = new URL(url);
    } catch {
      this.state.urlError = 'Please enter a valid URL (must start with http:// or https://)';
      return;
    }
    if (parsedUrl.protocol !== 'http:' && parsedUrl.protocol !== 'https:') {
      this.state.urlError = 'Please enter a valid URL (must start with http:// or https://)';
      return;
    }
    if (this.queueUrls().has(url)) {
      this.state.urlError = 'URL already in queue';
      return;
    }

    const cookieConfig = this.dependencies.readCookie();
    this.state.cancelRequested = false;
    this.state.playlistLoading = true;
    try {
      const completed = await this.inspect(url, cookieConfig);
      if (!this.active()) return;
      if (this.state.cancelRequested) {
        await this.dependencies.invoke('dismiss_operation', { operationId: completed.operationId });
        if (!this.active()) return;
        return;
      }

      this.state.urlInput = '';
      if (completed.inspection.kind === 'playlist') {
        const info = completed.inspection.playlist;
        this.state.playlistPage = 0;
        this.state.playlistModal = {
          info,
          inspectionOperationId: completed.operationId,
          url,
          cookieConfig: cookieConfig ? { ...cookieConfig } : null,
          entries: info.entries.map((entry) => ({ ...entry, selected: true }))
        };
        return;
      }
      await this.admit(completed.inspection.video, completed.operationId, cookieConfig);
    } catch (error) {
      if (!this.active()) return;
      const message = normalizeDownloadError(normalizeAppError(error));
      if (!this.state.cancelRequested && !message.toLowerCase().includes('cancelled')) {
        this.state.urlError = 'Failed to inspect URL: ' + message;
      }
    } finally {
      if (this.active()) {
        this.state.activeInspectionId = null;
        this.state.playlistLoading = false;
        this.state.cancelRequested = false;
      }
    }
  }

  async cancelInspection(): Promise<void> {
    const operationId = this.state.activeInspectionId;
    if (!operationId) return;
    this.state.cancelRequested = true;
    try {
      await this.dependencies.invoke('cancel_operation', { operationId });
    } catch (error) {
      if (!this.active()) return;
      this.state.urlError =
        'Failed to cancel inspection: ' + normalizeDownloadError(normalizeAppError(error));
    }
  }

  closePlaylist(): void {
    const operationId = this.state.playlistModal?.inspectionOperationId;
    this.state.playlistModal = null;
    this.state.playlistPage = 0;
    if (operationId) {
      void this.dependencies.invoke('dismiss_operation', { operationId }).catch((error) => {
        if (!this.active()) return;
        this.dependencies.setQueueActionError(
          `Could not dismiss completed inspection: ${normalizeAppError(error)}`
        );
      });
    }
  }

  pageCount(): number {
    return Math.max(
      1,
      Math.ceil((this.state.playlistModal?.entries.length ?? 0) / PLAYLIST_PAGE_SIZE)
    );
  }

  visibleEntries(): Array<{ entry: PlaylistModalEntry; index: number }> {
    if (!this.state.playlistModal) return [];
    const start = this.state.playlistPage * PLAYLIST_PAGE_SIZE;
    return this.state.playlistModal.entries
      .slice(start, start + PLAYLIST_PAGE_SIZE)
      .map((entry, offset) => ({ entry, index: start + offset }));
  }

  changePage(delta: number): void {
    this.state.playlistPage = Math.min(
      this.pageCount() - 1,
      Math.max(0, this.state.playlistPage + delta)
    );
  }

  toggleAll(checked: boolean): void {
    const modal = this.state.playlistModal;
    if (!modal) return;
    this.state.playlistModal = {
      ...modal,
      entries: modal.entries.map((entry) => ({ ...entry, selected: checked }))
    };
  }

  async addPlaylistSelection(): Promise<void> {
    const modal = this.state.playlistModal;
    if (!modal) return;
    const selectedEntries = modal.entries.filter((entry) => entry.selected);
    const queuedUrls = this.queueUrls();
    const cookieConfig = modal.cookieConfig ? { ...modal.cookieConfig } : null;
    this.state.urlInput = '';
    this.closePlaylist();
    this.state.playlistLoading = true;
    this.state.cancelRequested = false;
    const failures: string[] = [];

    try {
      for (const entry of selectedEntries) {
        if (this.state.cancelRequested) break;
        if (queuedUrls.has(entry.url)) continue;
        try {
          const completed = await this.inspect(entry.url, cookieConfig);
          if (!this.active()) return;
          if (completed.inspection.kind !== 'video') {
            await this.dependencies.invoke('dismiss_operation', {
              operationId: completed.operationId
            });
            if (!this.active()) return;
            throw new Error('A selected playlist entry unexpectedly resolved to another playlist.');
          }
          await this.admit(completed.inspection.video, completed.operationId, cookieConfig);
          if (!this.active()) return;
          queuedUrls.add(entry.url);
        } catch (error) {
          if (!this.active()) return;
          if (this.state.cancelRequested) break;
          failures.push(`${entry.title ?? entry.id}: ${normalizeAppError(error)}`);
        }
      }
    } finally {
      if (this.active()) {
        this.state.activeInspectionId = null;
        this.state.playlistLoading = false;
        this.state.cancelRequested = false;
      }
    }
    if (failures.length > 0) {
      this.state.urlError = `Some playlist entries could not be added. ${failures.slice(0, 3).join(' ')}`;
    }
  }
}

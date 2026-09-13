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
import type { MediaSelection } from './bindings/MediaSelection';
import { mediaIdentityKey } from './media-identity';
import type { PlaylistAdmissionResult } from './bindings/PlaylistAdmissionResult';
import type { PlaylistEntry } from './bindings/PlaylistEntry';
import type { PlaylistMetadataLease } from './playlist-metadata-owner';

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
    metadata: Pick<QueueItem, 'duration' | 'channel' | 'thumbnail'>,
    selection?: MediaSelection | null
  ) => () => void;
  retainPlaylistMetadata: (
    entries: readonly {
      identity: string;
      metadata: Pick<QueueItem, 'duration' | 'channel' | 'thumbnail'>;
    }[]
  ) => PlaylistMetadataLease;
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
  private playlistAdmission: {
    inspectionOperationId: string;
    selectionKey: string;
    requestId: string;
    settings: Pick<InspectionSettings, 'globalFormat' | 'globalQuality' | 'outputDir'>;
    compatConfigPath: string | null;
  } | null = null;

  constructor(
    readonly state: InspectionState,
    private readonly dependencies: InspectionWorkflowDependencies
  ) {}

  private active(): boolean {
    return this.dependencies.isActive();
  }

  private queueIdentities(): Set<string> {
    return new Set(this.dependencies.queue.getItems().map(mediaIdentityKey));
  }

  private async inspect(
    url: string,
    cookieConfig: CookieConfig | null,
    selection?: MediaSelection | null
  ): Promise<CompletedInspection> {
    const input = {
      url,
      cookieConfig,
      compatConfigPath: this.dependencies.readCompat(),
      ...(selection ? { selection } : {})
    };
    const result = await this.dependencies.invoke('begin_inspection', {
      input
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
    const discardMetadata = this.dependencies.queue.retainMetadata(
      info.url,
      { duration: info.duration, channel: info.channel, thumbnail: info.thumbnail },
      info.selection
    );
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
    if (this.queueIdentities().has(mediaIdentityKey({ url }))) {
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
    if (this.state.playlistLoading) return;
    const operationId = this.state.playlistModal?.inspectionOperationId;
    this.state.playlistModal = null;
    this.state.playlistPage = 0;
    this.playlistAdmission = null;
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
    if (this.state.playlistLoading) return;
    const modal = this.state.playlistModal;
    if (!modal) return;
    const entryIndices = modal.entries.flatMap((entry, index) => (entry.selected ? [index] : []));
    if (entryIndices.length === 0) return;
    this.state.urlError = '';
    const cookieConfig = modal.cookieConfig ? { ...modal.cookieConfig } : null;
    const selectionKey = entryIndices.join(',');
    let admission = this.playlistAdmission;
    if (
      !admission ||
      admission.inspectionOperationId !== modal.inspectionOperationId ||
      admission.selectionKey !== selectionKey
    ) {
      const settings = this.dependencies.getSettings();
      admission = {
        inspectionOperationId: modal.inspectionOperationId,
        selectionKey,
        requestId: crypto.randomUUID(),
        settings: {
          globalFormat: settings.globalFormat,
          globalQuality: settings.globalQuality,
          outputDir: settings.outputDir
        },
        compatConfigPath: this.dependencies.readCompat()
      };
      this.playlistAdmission = admission;
    }
    this.state.urlInput = '';
    this.state.playlistLoading = true;
    this.state.cancelRequested = false;
    let metadataLease: PlaylistMetadataLease | null = null;

    try {
      metadataLease = this.retainPlaylistMetadataLease(
        entryIndices.map((index) => modal.entries[index])
      );
      const result: PlaylistAdmissionResult = await this.dependencies.invoke(
        'add_inspection_result_to_queue',
        {
          input: {
            inspectionOperationId: modal.inspectionOperationId,
            format: admission.settings.globalFormat,
            quality: admission.settings.globalQuality,
            outputDir: admission.settings.outputDir,
            cookieConfig,
            filenameOverride: null,
            compatConfigPath: admission.compatConfigPath,
            playlist: { requestId: admission.requestId, entryIndices }
          }
        }
      );
      if (!this.active()) {
        metadataLease.discard();
        return;
      }
      if (result.kind !== 'playlist' || result.requestId !== admission.requestId) {
        throw new Error('Playlist admission returned an unexpected response.');
      }
      metadataLease.confirm(result.itemIds);
      this.state.playlistModal = null;
      this.state.playlistPage = 0;
      this.playlistAdmission = null;
    } catch (error) {
      metadataLease?.discard();
      if (!this.active()) return;
      this.state.urlError = 'Some playlist entries could not be added. ' + normalizeAppError(error);
    } finally {
      if (this.active()) {
        this.state.activeInspectionId = null;
        this.state.playlistLoading = false;
        this.state.cancelRequested = false;
      }
    }
  }

  private retainPlaylistMetadataLease(entries: readonly PlaylistEntry[]): PlaylistMetadataLease {
    const retained = entries.map((entry) => ({
      identity: mediaIdentityKey(entry),
      metadata: playlistEntryDisplayMetadata(entry)
    }));
    return this.dependencies.queue.retainPlaylistMetadata(retained);
  }
}

function playlistEntryDisplayMetadata(
  entry: PlaylistEntry
): Pick<QueueItem, 'duration' | 'channel' | 'thumbnail'> {
  const video = entry.video;
  return {
    duration: video?.duration ?? entry.duration,
    channel: video?.channel ?? null,
    thumbnail: video?.thumbnail ?? entry.thumbnail
  };
}

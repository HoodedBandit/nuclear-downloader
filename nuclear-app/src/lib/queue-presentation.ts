import { latestOperationForItem } from './backend-state';
import type { AppSnapshot } from './bindings/AppSnapshot';
import type { QueueItemRecord } from './bindings/QueueItemRecord';
import type { MediaSelection } from './bindings/MediaSelection';
import type { StateDelta } from './bindings/StateDelta';
import { normalizeDownloadError } from './frontend-errors';
import type { DownloadProgressPayload, QueueItem } from './frontend-types';
import { mediaIdentityKey } from './media-identity';
import { reduceOperationProgress, shouldIgnoreOperationProgress } from './operation-reducer';
import {
  PlaylistMetadataOwner,
  type PlaylistMetadataEntry,
  type PlaylistMetadataLease
} from './playlist-metadata-owner';
import {
  OPERATION_FIELDS,
  assignQueueFields,
  buildQueueSummary,
  displayConversionProgress,
  displayDownloadProgress,
  displayEta,
  displayProgress,
  inspectionDisplayMetadata,
  isActiveStatus,
  isTerminalStatus,
  projectQueueItem,
  projectionRecordChanged
} from './queue-presentation-helpers';
export {
  canEditFilename,
  canRetryItem,
  formatDuration,
  formatByteCount,
  getQueueItemDisplayTitle,
  getStatusLabel,
  isActiveStatus,
  isEditablePendingStatus,
  isTerminalStatus,
  roundedProgress,
  sanitizeFilenameDraft,
  shouldShowConversionProgress
} from './queue-presentation-helpers';

const DISPLAY_INTERVAL_MS = 500;
export const QUEUE_ROW_HEIGHT_PX = 88;

export interface QueuePresentationState {
  items: QueueItem[];
  backendSnapshot: AppSnapshot | null;
}

export interface QueuePresentationOptions {
  isActive: () => boolean;
  now?: () => number;
}

export function createQueuePresentationState(): QueuePresentationState {
  return {
    items: [],
    backendSnapshot: null
  };
}

export class QueuePresentationController {
  private readonly metadataByIdentity = new Map<string, RetainedMetadata[]>();
  private readonly playlistMetadataOwner: PlaylistMetadataOwner<QueueItemRecord>;
  private readonly displayUpdatedAt = new Map<string, { operationId: string; updatedAt: number }>();

  constructor(
    readonly state: QueuePresentationState,
    private readonly options: QueuePresentationOptions
  ) {
    this.playlistMetadataOwner = new PlaylistMetadataOwner(
      (itemId) => this.state.backendSnapshot?.queue.find((record) => record.id === itemId),
      (record) => mediaIdentityKey({ url: record.sourceUrl, selection: record.selection }),
      (itemId, metadata) => {
        const item = this.state.items.find((candidate) => candidate.id === itemId);
        if (!item) return false;
        item.duration ??= metadata.duration ?? null;
        item.channel ??= metadata.channel ?? null;
        item.thumbnail ??= metadata.thumbnail ?? null;
        return true;
      }
    );
  }

  getItems(): readonly QueueItem[] {
    return this.state.items;
  }

  retainMetadata(
    url: string,
    metadata: Pick<QueueItem, 'duration' | 'channel' | 'thumbnail'>,
    selection?: MediaSelection | null
  ): () => void {
    const key = mediaIdentityKey({ url, selection });
    const entry: RetainedMetadata = {
      metadata,
      existingIds: new Set(
        this.state.backendSnapshot?.queue
          .filter(
            (record) =>
              mediaIdentityKey({ url: record.sourceUrl, selection: record.selection }) === key
          )
          .map((record) => record.id) ?? []
      )
    };
    const entries = this.metadataByIdentity.get(key) ?? [];
    entries.push(entry);
    this.metadataByIdentity.set(key, entries);
    let retained = true;
    return () => {
      if (!retained) return;
      retained = false;
      const current = this.metadataByIdentity.get(key);
      if (!current) return;
      const next = current.filter((candidate) => candidate !== entry);
      if (next.length === 0) this.metadataByIdentity.delete(key);
      else this.metadataByIdentity.set(key, next);
    };
  }

  retainPlaylistMetadata(entries: readonly PlaylistMetadataEntry[]): PlaylistMetadataLease {
    return this.playlistMetadataOwner.retain(entries);
  }

  applySnapshot(snapshot: AppSnapshot, delta?: StateDelta): void {
    const previous = this.state.backendSnapshot;
    this.state.backendSnapshot = snapshot;
    try {
      if (!previous || !delta) {
        const existing = new Map(this.state.items.map((item) => [item.id, item]));
        this.state.items = snapshot.queue.map((record) =>
          this.project(snapshot, record, existing.get(record.id))
        );
        return;
      }
      switch (delta.kind) {
        case 'queue_item_upserted': {
          const prior = previous.queue.find((item) => item.id === delta.value.id);
          if (!projectionRecordChanged(prior, delta.value)) return;
          this.upsert(snapshot, delta.value);
          return;
        }
        case 'queue_items_removed': {
          const removed = new Set(delta.value);
          this.state.items = this.state.items.filter((item) => !removed.has(item.id));
          this.playlistMetadataOwner.remove(delta.value);
          return;
        }
        case 'operation_upserted':
          if (delta.value.queueItemId) this.projectOperation(snapshot, delta.value.queueItemId);
          return;
        case 'operation_removed': {
          const operation = previous.operations.find((item) => item.id === delta.value);
          if (operation?.queueItemId) this.projectOperation(snapshot, operation.queueItemId);
          return;
        }
        default:
          return;
      }
    } finally {
      this.playlistMetadataOwner.reconcile(delta === undefined);
      this.pruneDisplayOwnership();
    }
  }

  applyProgress(payload: DownloadProgressPayload): void {
    const index = this.state.items.findIndex((item) => item.downloadId === payload.download_id);
    if (index === -1) return;
    const item = this.state.items[index];
    if (shouldIgnoreOperationProgress(item, payload)) return;
    const statusChanged = item.status !== payload.status;
    const terminal = isTerminalStatus(payload.status);
    const refresh = this.shouldRefreshDisplay(item, payload, statusChanged);
    if (!refresh && !statusChanged && !terminal && !payload.error && !payload.filename) return;
    const reduced = reduceOperationProgress(item, payload);
    const next = {
      ...reduced,
      progress: displayProgress(item, payload, refresh),
      downloadProgress: terminal
        ? reduced.downloadProgress
        : displayDownloadProgress(item, payload, refresh),
      conversionProgress: terminal
        ? reduced.conversionProgress
        : displayConversionProgress(item, payload, refresh),
      eta: terminal ? reduced.eta : displayEta(item, payload, refresh),
      error: payload.error
        ? normalizeDownloadError(payload.error, payload.error_code ?? null)
        : null,
      errorCode: payload.error_code ?? null,
      errorDetail: payload.error_detail ?? null,
      filename: payload.filename ?? item.filename
    };
    assignQueueFields(item, next, OPERATION_FIELDS);
    if (terminal) this.clearProgressDisplayState(item.id);
    else if (refresh && isActiveStatus(payload.status) && item.downloadId)
      this.displayUpdatedAt.set(item.id, {
        operationId: item.downloadId,
        updatedAt: this.now()
      });
  }

  clearProgressDisplayState(id: string): void {
    this.displayUpdatedAt.delete(id);
  }
  dispose(): void {
    this.metadataByIdentity.clear();
    this.playlistMetadataOwner.dispose();
    this.displayUpdatedAt.clear();
  }
  replaceItem(id: string, mapper: (item: QueueItem) => QueueItem): void {
    const index = this.state.items.findIndex((item) => item.id === id);
    if (index !== -1) this.state.items[index] = mapper(this.state.items[index]);
  }
  summary() {
    return buildQueueSummary(this.state.items);
  }

  private now(): number {
    return (this.options.now ?? Date.now)();
  }
  private shouldRefreshDisplay(
    item: QueueItem,
    payload: DownloadProgressPayload,
    changed: boolean
  ): boolean {
    if (!isActiveStatus(payload.status)) return true;
    const current =
      payload.status === 'postprocessing' ? (item.conversionProgress ?? 0) : item.downloadProgress;
    return (
      changed ||
      current === 0 ||
      (payload.status === 'downloading' && item.eta === '') ||
      this.now() - this.lastDisplayUpdate(item) >= DISPLAY_INTERVAL_MS
    );
  }
  private lastDisplayUpdate(item: QueueItem): number {
    const owner = this.displayUpdatedAt.get(item.id);
    return owner?.operationId === item.downloadId ? owner.updatedAt : 0;
  }
  private upsert(snapshot: AppSnapshot, record: QueueItemRecord): void {
    const index = this.state.items.findIndex((item) => item.id === record.id);
    if (index === -1) this.state.items.push(this.project(snapshot, record, undefined));
    else this.state.items[index] = this.project(snapshot, record, this.state.items[index]);
  }
  private projectOperation(snapshot: AppSnapshot, id: string): void {
    const record = snapshot.queue.find((item) => item.id === id);
    if (!record) return;
    const index = this.state.items.findIndex((item) => item.id === id);
    if (index === -1) this.state.items.push(this.project(snapshot, record, undefined));
    else
      assignQueueFields(
        this.state.items[index],
        this.project(snapshot, record, this.state.items[index]),
        OPERATION_FIELDS
      );
  }
  private project(snapshot: AppSnapshot, record: QueueItemRecord, existing?: QueueItem): QueueItem {
    const operation = latestOperationForItem(snapshot, record.id, record.latestOperationId);
    const retainedMetadata = this.claimMetadata(record);
    const metadata = inspectionDisplayMetadata(record, operation) ?? retainedMetadata;
    return projectQueueItem(record, operation, existing, metadata);
  }

  private claimMetadata(
    record: QueueItemRecord
  ): Pick<QueueItem, 'duration' | 'channel' | 'thumbnail'> | undefined {
    const key = mediaIdentityKey({ url: record.sourceUrl, selection: record.selection });
    const entries = this.metadataByIdentity.get(key);
    if (!entries) return undefined;
    const index = entries.findIndex((entry) => !entry.existingIds.has(record.id));
    const entry = index === -1 ? undefined : entries.splice(index, 1)[0];
    for (const remaining of entries) remaining.existingIds.add(record.id);
    if (entries.length === 0) this.metadataByIdentity.delete(key);
    return entry?.metadata;
  }

  private pruneDisplayOwnership(): void {
    for (const [itemId, owner] of this.displayUpdatedAt) {
      const item = this.state.items.find((candidate) => candidate.id === itemId);
      if (!item || !isActiveStatus(item.status) || item.downloadId !== owner.operationId) {
        this.displayUpdatedAt.delete(itemId);
      }
    }
  }
}

interface RetainedMetadata {
  metadata: Pick<QueueItem, 'duration' | 'channel' | 'thumbnail'>;
  existingIds: Set<string>;
}

import { latestOperationForItem } from './backend-state';
import type { AppSnapshot } from './bindings/AppSnapshot';
import type { QueueItemRecord } from './bindings/QueueItemRecord';
import type { MediaSelection } from './bindings/MediaSelection';
import type { StateDelta } from './bindings/StateDelta';
import { normalizeAppError, normalizeDownloadError } from './frontend-errors';
import type { DownloadProgressPayload, QueueItem } from './frontend-types';
import { mediaIdentityKey } from './media-identity';
import { reduceOperationProgress, shouldIgnoreOperationProgress } from './operation-reducer';
import {
  PlaylistMetadataOwner,
  type PlaylistMetadataEntry,
  type PlaylistMetadataLease
} from './playlist-metadata-owner';
import { deriveSelectionState } from './queue-logic';
import {
  OPERATION_FIELDS,
  assignQueueFields,
  buildQueueSummary,
  displayConversionProgress,
  displayDownloadProgress,
  displayEta,
  displayProgress,
  getQueueItemDisplayTitle,
  inspectionDisplayMetadata,
  isActiveStatus,
  isEditablePendingStatus,
  isTerminalStatus,
  projectQueueItem,
  projectionRecordChanged,
  sanitizeFilenameDraft
} from './queue-presentation-helpers';
export {
  canEditFilename,
  canRetryItem,
  formatDuration,
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
export const QUEUE_ROW_HEIGHT_PX = 53;
const ROW_OVERSCAN = 8;

export interface QueuePresentationState {
  items: QueueItem[];
  backendSnapshot: AppSnapshot | null;
  editing: { itemId: string | null; draft: string; error: string };
  viewport: { scrollTop: number; height: number };
}

export interface QueuePresentationOptions {
  isActive: () => boolean;
  saveFilename: (itemId: string, filenameOverride: string | null) => Promise<void>;
  focusFilenameEditor?: () => void | Promise<void>;
  clearFilenameEditor?: () => void;
  now?: () => number;
}

export function createQueuePresentationState(): QueuePresentationState {
  return {
    items: [],
    backendSnapshot: null,
    editing: { itemId: null, draft: '', error: '' },
    viewport: { scrollTop: 0, height: 600 }
  };
}

export class QueuePresentationController {
  private readonly metadataByIdentity = new Map<string, RetainedMetadata[]>();
  private readonly playlistMetadataOwner: PlaylistMetadataOwner<QueueItemRecord>;
  private readonly displayUpdatedAt = new Map<string, { operationId: string; updatedAt: number }>();
  private filenameEditGeneration = 0;

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
  selectionState(): ReturnType<typeof deriveSelectionState> {
    return deriveSelectionState(this.state.items);
  }
  setSelected(id: string, selected: boolean): void {
    const item = this.state.items.find((candidate) => candidate.id === id);
    if (item) item.selected = selected;
  }
  setAllSelected(selected: boolean): void {
    this.state.items = this.state.items.map((item) => ({ ...item, selected }));
  }
  selectedItems(): QueueItem[] {
    return this.state.items.filter((item) => item.selected);
  }
  summary() {
    return buildQueueSummary(this.state.items);
  }

  setViewport(scrollTop: number, height = this.state.viewport.height): void {
    this.state.viewport.height = height;
    const max = Math.max(0, this.state.items.length * QUEUE_ROW_HEIGHT_PX - height);
    this.state.viewport.scrollTop = Math.min(Math.max(0, scrollTop), max);
  }
  window() {
    const calculatedStart = Math.max(
      0,
      Math.floor(this.state.viewport.scrollTop / QUEUE_ROW_HEIGHT_PX) - ROW_OVERSCAN
    );
    const start = Math.min(Math.max(0, this.state.items.length - 1), calculatedStart);
    const end = Math.min(
      this.state.items.length,
      Math.ceil(
        (this.state.viewport.scrollTop + this.state.viewport.height) / QUEUE_ROW_HEIGHT_PX
      ) + ROW_OVERSCAN
    );
    return {
      start,
      end,
      rows: this.state.items
        .slice(start, end)
        .map((item, offset) => ({ item, index: start + offset })),
      topSpacerHeight: start * QUEUE_ROW_HEIGHT_PX,
      bottomSpacerHeight: Math.max(0, (this.state.items.length - end) * QUEUE_ROW_HEIGHT_PX)
    };
  }

  async beginFilenameEdit(item: QueueItem): Promise<void> {
    if (!isEditablePendingStatus(item.status) || !item.infoLoaded) return;
    if (this.state.editing.itemId && this.state.editing.itemId !== item.id) {
      await this.commitFilenameEdit(this.state.editing.itemId);
      if (!this.options.isActive() || this.state.editing.itemId) return;
    }
    this.filenameEditGeneration += 1;
    this.state.editing.itemId = item.id;
    this.state.editing.draft = getQueueItemDisplayTitle(item);
    this.state.editing.error = '';
    await this.options.focusFilenameEditor?.();
  }

  async commitFilenameEdit(id: string | null = this.state.editing.itemId): Promise<void> {
    if (!id) return;
    if (this.state.editing.itemId !== id) return;
    const item = this.state.items.find((candidate) => candidate.id === id);
    if (!item) {
      this.cancelFilenameEdit();
      return;
    }
    const generation = this.filenameEditGeneration;
    const draft = this.state.editing.draft;
    const cleaned = sanitizeFilenameDraft(draft);
    if (!cleaned) {
      this.state.editing.error = 'Filename must contain at least one valid character.';
      return;
    }
    try {
      await this.options.saveFilename(id, cleaned !== item.title ? cleaned : null);
      if (!this.options.isActive()) return;
      if (this.ownsFilenameEdit(generation, id, draft)) this.cancelFilenameEdit();
    } catch (error) {
      if (this.options.isActive() && this.ownsFilenameEdit(generation, id, draft))
        this.state.editing.error = normalizeAppError(error);
    }
  }

  cancelFilenameEdit(): void {
    this.filenameEditGeneration += 1;
    this.state.editing.itemId = null;
    this.state.editing.draft = '';
    this.state.editing.error = '';
    this.options.clearFilenameEditor?.();
  }

  private ownsFilenameEdit(generation: number, id: string, draft: string): boolean {
    return (
      this.filenameEditGeneration === generation &&
      this.state.editing.itemId === id &&
      this.state.editing.draft === draft
    );
  }

  toggleDiagnostics(id: string): void {
    this.state.items = this.state.items.map((item) =>
      item.id === id ? { ...item, diagnosticsOpen: !item.diagnosticsOpen } : item
    );
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

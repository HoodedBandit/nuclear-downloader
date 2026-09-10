import { latestOperationForItem, publishedOutputPath, isTerminalOperation } from './backend-state';
import type { AppSnapshot } from './bindings/AppSnapshot';
import type { OperationSnapshot } from './bindings/OperationSnapshot';
import type { QueueItemRecord } from './bindings/QueueItemRecord';
import type { MediaSelection } from './bindings/MediaSelection';
import type { StateDelta } from './bindings/StateDelta';
import { normalizeAppError, normalizeDownloadError } from './frontend-errors';
import type {
  DownloadPhase,
  DownloadProgressPayload,
  DownloadStatus,
  QueueItem
} from './frontend-types';
import { mediaIdentityKey, sameMediaIdentity } from './media-identity';
import { audioFormats, supportedBrowsers, videoFormats } from './frontend-types';
import type { WorkflowCommands } from './frontend-workflow-ports';
import { reduceOperationProgress, shouldIgnoreOperationProgress } from './operation-reducer';
import { deriveSelectionState } from './queue-logic';

const DISPLAY_INTERVAL_MS = 500;
export const QUEUE_ROW_HEIGHT_PX = 53;
const ROW_OVERSCAN = 8;
const MAX_FILENAME_UNITS = 180;
const INVALID_FILENAME_CHARS = '<>:"/\\|?*';
const RESERVED_FILENAME_STEMS = new Set([
  'CON',
  'PRN',
  'AUX',
  'NUL',
  ...Array.from({ length: 9 }, (_, index) => `COM${index + 1}`),
  ...Array.from({ length: 9 }, (_, index) => `LPT${index + 1}`)
]);
const OPERATION_FIELDS = [
  'downloadId',
  'status',
  'progress',
  'downloadProgress',
  'conversionProgress',
  'phase',
  'speed',
  'eta',
  'error',
  'errorCode',
  'errorDetail',
  'filename'
] as const satisfies readonly (keyof QueueItem)[];

export interface QueuePresentationState {
  items: QueueItem[];
  backendSnapshot: AppSnapshot | null;
  editing: { itemId: string | null; draft: string; error: string };
  viewport: { scrollTop: number; height: number };
}

export interface QueuePresentationOptions extends WorkflowCommands {
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
  private readonly displayUpdatedAt = new Map<string, { operationId: string; updatedAt: number }>();
  private filenameEditGeneration = 0;

  constructor(
    readonly state: QueuePresentationState,
    private readonly options: QueuePresentationOptions
  ) {}

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
    assignFields(item, next, OPERATION_FIELDS);
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
    const start = Math.max(
      0,
      Math.floor(this.state.viewport.scrollTop / QUEUE_ROW_HEIGHT_PX) - ROW_OVERSCAN
    );
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
    if (!isEditablePendingStatus(item.status)) return;
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
      await this.options.invoke('update_queue_item', {
        itemId: id,
        input: { filenameOverride: cleaned !== item.title ? cleaned : null }
      });
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
      assignFields(
        this.state.items[index],
        this.project(snapshot, record, this.state.items[index]),
        OPERATION_FIELDS
      );
  }
  private project(snapshot: AppSnapshot, record: QueueItemRecord, existing?: QueueItem): QueueItem {
    const metadata = this.claimMetadata(record);
    const operation = latestOperationForItem(snapshot, record.id, record.latestOperationId);
    const status = queueStatus(record, operation);
    const terminal = isTerminalStatus(status);
    const operationProgress = clamp(operation?.progress ?? 0);
    const same = existing?.downloadId === operation?.id;
    const progress =
      same && isActiveStatus(status)
        ? Math.max(existing?.progress ?? 0, operationProgress)
        : status === 'completed'
          ? 100
          : operationProgress;
    const downloadProgress =
      status === 'postprocessing' || status === 'completed'
        ? 100
        : same && status === 'downloading'
          ? Math.max(existing?.downloadProgress ?? 0, operationProgress)
          : operationProgress;
    const phase = terminal ? null : operationPhase(operation);
    const conversionProgress =
      status === 'completed' && record.format === 'webm'
        ? 100
        : phase === 'conversion'
          ? operationProgress
          : terminal
            ? null
            : (existing?.conversionProgress ?? null);
    const cookie = record.cookieConfig;
    const browser = supportedBrowsers.includes(
      cookie?.browser as (typeof supportedBrowsers)[number]
    )
      ? cookie?.browser
      : 'firefox';
    const format = [...videoFormats, ...audioFormats].includes(record.format as QueueItem['format'])
      ? record.format
      : 'mp4';
    const interrupted = record.state === 'interrupted';
    return {
      id: record.id,
      downloadId: operation && !isTerminalOperation(operation) ? operation.id : null,
      url: record.sourceUrl,
      ...(record.selection ? { selection: record.selection } : {}),
      title: record.title,
      customFilename: record.filenameOverride,
      duration: existing?.duration ?? metadata?.duration ?? null,
      channel: existing?.channel ?? metadata?.channel ?? null,
      thumbnail: existing?.thumbnail ?? metadata?.thumbnail ?? null,
      infoLoaded: true,
      hasAudio: record.hasAudio,
      status,
      quality: record.quality,
      format: format as QueueItem['format'],
      cookieConfig: cookie
        ? {
            enabled: cookie.enabled,
            mode: cookie.mode === 'file' ? 'file' : 'browser',
            browser: browser as NonNullable<QueueItem['cookieConfig']>['browser'],
            cookie_file: cookie.cookie_file
          }
        : null,
      availableQualities: [
        'best',
        ...record.availableQualities.filter((quality) => quality !== 'best')
      ],
      progress,
      downloadProgress,
      conversionProgress,
      phase,
      speed: terminal ? '' : (existing?.speed ?? ''),
      eta: terminal ? '' : (existing?.eta ?? ''),
      error:
        operation?.error?.summary ??
        (interrupted ? 'The previous app session ended before this attempt completed.' : null),
      errorCode: operation?.error?.code ?? (interrupted ? 'interrupted' : null),
      errorDetail: operationErrorDetail(operation),
      diagnosticsOpen: existing?.diagnosticsOpen ?? false,
      filename: publishedOutputPath(operation) ?? (same ? (existing?.filename ?? null) : null),
      selected: existing?.selected ?? false
    };
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

export function isActiveStatus(status: DownloadStatus): boolean {
  return status === 'downloading' || status === 'postprocessing' || status === 'cancelling';
}
export function isTerminalStatus(status: DownloadStatus): boolean {
  return status === 'completed' || status === 'error' || status === 'cancelled';
}
export function isEditablePendingStatus(status: DownloadStatus): boolean {
  return status === 'ready';
}
export function canEditFilename(item: QueueItem): boolean {
  return isEditablePendingStatus(item.status);
}
export function getQueueItemDisplayTitle(item: QueueItem): string {
  return item.customFilename ?? item.title;
}
export function canRetryItem(item: QueueItem): boolean {
  return item.status === 'error' || item.status === 'cancelled';
}
export function shouldShowConversionProgress(item: QueueItem): boolean {
  return (
    item.format === 'webm' &&
    (item.status === 'postprocessing' ||
      item.conversionProgress !== null ||
      item.status === 'completed')
  );
}
export function getStatusLabel(item: QueueItem): string {
  if (isTerminalStatus(item.status) || item.status === 'cancelling') return item.status;
  if (item.phase === 'waiting_conversion') return 'waiting to convert';
  if (item.status === 'postprocessing') return 'converting';
  return item.status;
}
export function roundedProgress(value: number | null): number {
  return Math.round(clamp(value ?? 0));
}
export function formatDuration(seconds: number | null | undefined): string {
  if (!seconds) return '--:--';
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  return h > 0
    ? `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
    : `${m}:${String(s).padStart(2, '0')}`;
}
export function sanitizeFilenameDraft(value: string): string {
  let cleaned = value.trim();
  if (!cleaned) return '';
  cleaned = Array.from(cleaned, (character) =>
    character.charCodeAt(0) <= 0x1f || INVALID_FILENAME_CHARS.includes(character) ? '_' : character
  ).join('');
  for (const extension of [...videoFormats, ...audioFormats]) {
    const suffix = `.${extension}`;
    if (cleaned.toLowerCase().endsWith(suffix)) {
      cleaned = cleaned.slice(0, -suffix.length);
      break;
    }
  }
  cleaned = cleaned.trim().replace(/[. ]+$/g, '');
  if (!cleaned) return '';
  const dot = cleaned.indexOf('.');
  const stem = (dot === -1 ? cleaned : cleaned.slice(0, dot)).toUpperCase();
  if (RESERVED_FILENAME_STEMS.has(stem))
    cleaned = dot === -1 ? `${cleaned}_` : `${cleaned.slice(0, dot)}_${cleaned.slice(dot)}`;
  let bounded = '';
  let units = 0;
  for (const character of cleaned) {
    if (units + character.length > MAX_FILENAME_UNITS) break;
    bounded += character;
    units += character.length;
  }
  return bounded.trim().replace(/[. ]+$/g, '');
}

function queueStatus(record: QueueItemRecord, operation: OperationSnapshot | null): DownloadStatus {
  if (operation) {
    if (operation.state === 'completed') return 'completed';
    if (operation.state === 'failed' || operation.state === 'interrupted') return 'error';
    if (operation.state === 'cancelled') return 'cancelled';
    if (operation.state === 'cancelling') return 'cancelling';
    if (operation.state === 'queued' || operation.state === 'starting') return 'queued';
    if (operation.state === 'running')
      return operation.phase === 'conversion' || operation.phase === 'postprocess'
        ? 'postprocessing'
        : 'downloading';
  }
  return (
    {
      inert: 'ready',
      queued: 'queued',
      running: 'downloading',
      completed: 'completed',
      cancelled: 'cancelled',
      failed: 'error',
      interrupted: 'error'
    } as const
  )[record.state];
}
function operationPhase(operation: OperationSnapshot | null): DownloadPhase | null {
  const phase = operation?.phase;
  return phase === 'download' ||
    phase === 'postprocess' ||
    phase === 'waiting_conversion' ||
    phase === 'conversion' ||
    phase === 'complete'
    ? phase
    : null;
}
function operationErrorDetail(operation: OperationSnapshot | null): string | null {
  if (!operation?.error) return null;
  return [operation.error.detail, `Correlation ID: ${operation.error.correlationId}`]
    .filter(Boolean)
    .join('\n');
}
function clamp(value: number): number {
  return Math.min(100, Math.max(0, value));
}
function assignFields<K extends keyof QueueItem>(
  target: QueueItem,
  source: QueueItem,
  fields: readonly K[]
): void {
  for (const field of fields)
    if (!Object.is(target[field], source[field]))
      (target as Record<keyof QueueItem, unknown>)[field] = source[field];
}
function projectionRecordChanged(
  previous: QueueItemRecord | undefined,
  next: QueueItemRecord
): boolean {
  if (!previous) return true;
  return (
    !sameMediaIdentity(
      { url: previous.sourceUrl, selection: previous.selection },
      { url: next.sourceUrl, selection: next.selection }
    ) ||
    previous.title !== next.title ||
    previous.hasAudio !== next.hasAudio ||
    previous.format !== next.format ||
    previous.quality !== next.quality ||
    previous.outputDir !== next.outputDir ||
    previous.filenameOverride !== next.filenameOverride ||
    previous.compatConfigPath !== next.compatConfigPath ||
    previous.state !== next.state ||
    previous.latestOperationId !== next.latestOperationId ||
    previous.availableQualities.length !== next.availableQualities.length ||
    previous.availableQualities.some(
      (quality, index) => quality !== next.availableQualities[index]
    ) ||
    JSON.stringify(previous.cookieConfig) !== JSON.stringify(next.cookieConfig)
  );
}
function displayProgress(
  item: QueueItem,
  payload: DownloadProgressPayload,
  refresh: boolean
): number {
  if (payload.status === 'completed') return 100;
  if (payload.status === 'postprocessing') {
    const conversion = payload.phase === 'conversion' || payload.conversion_progress != null;
    const value = payload.conversion_progress ?? (conversion ? payload.progress : null);
    if (item.format === 'webm' && conversion && value !== null)
      return refresh ? Math.max(item.progress, clamp(value)) : item.progress;
    return 100;
  }
  if (payload.status !== 'downloading') return clamp(payload.progress);
  return refresh ? Math.max(item.progress, clamp(payload.progress)) : item.progress;
}
function displayDownloadProgress(
  item: QueueItem,
  payload: DownloadProgressPayload,
  refresh: boolean
): number {
  if (payload.status === 'completed' || payload.status === 'postprocessing') return 100;
  if (payload.status !== 'downloading') return item.downloadProgress;
  const value = payload.download_progress ?? payload.progress;
  return refresh ? Math.max(item.downloadProgress, clamp(value)) : item.downloadProgress;
}
function displayConversionProgress(
  item: QueueItem,
  payload: DownloadProgressPayload,
  refresh: boolean
): number | null {
  if (payload.status === 'completed' && item.format === 'webm') return 100;
  if (payload.status !== 'postprocessing') return item.conversionProgress;
  const conversion = payload.phase === 'conversion' || payload.conversion_progress != null;
  if (!conversion || (item.format !== 'webm' && payload.conversion_progress == null))
    return item.conversionProgress;
  const value = payload.conversion_progress ?? payload.progress;
  const current = item.conversionProgress ?? 0;
  return refresh ? Math.max(current, clamp(value)) : current;
}
function displayEta(item: QueueItem, payload: DownloadProgressPayload, refresh: boolean): string {
  if (payload.status !== 'downloading') return '';
  const next = payload.eta ?? '';
  return next === '' || !refresh ? item.eta : next;
}
function buildQueueSummary(items: QueueItem[]) {
  const counts = { total: items.length, ready: 0, downloading: 0, completed: 0, failed: 0 };
  let hasReady = false,
    hasSelectedReady = false,
    hasActive = false,
    hasCompleted = false,
    hasSelected = false;
  for (const item of items) {
    if (item.status === 'ready') {
      counts.ready += 1;
      hasReady = true;
      if (item.selected) hasSelectedReady = true;
    }
    if (isActiveStatus(item.status)) {
      counts.downloading += 1;
      hasActive = true;
    }
    if (item.status === 'completed') {
      counts.completed += 1;
      hasCompleted = true;
    } else if (item.status === 'cancelled') hasCompleted = true;
    else if (item.status === 'error') counts.failed += 1;
    if (item.selected) hasSelected = true;
  }
  return { counts, hasReady, hasSelectedReady, hasActive, hasCompleted, hasSelected };
}

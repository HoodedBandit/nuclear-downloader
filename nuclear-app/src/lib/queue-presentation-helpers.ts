import { isTerminalOperation, publishedOutputPath } from './backend-state';
import type { OperationSnapshot } from './bindings/OperationSnapshot';
import type { QueueItemRecord } from './bindings/QueueItemRecord';
import type {
  DownloadPhase,
  DownloadProgressPayload,
  DownloadStatus,
  QueueItem
} from './frontend-types';
import { audioFormats, supportedBrowsers, videoFormats } from './frontend-types';
import { sameMediaIdentity } from './media-identity';

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

export const OPERATION_FIELDS = [
  'downloadId',
  'status',
  'duration',
  'channel',
  'thumbnail',
  'infoLoaded',
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
  return (item.status === 'ready' || item.status === 'queued') && item.infoLoaded;
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
  return Math.round(clampProgress(value ?? 0));
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

export function projectQueueItem(
  record: QueueItemRecord,
  operation: OperationSnapshot | null,
  existing?: QueueItem,
  metadata?: Pick<QueueItem, 'duration' | 'channel' | 'thumbnail'>
): QueueItem {
  const status = queueStatus(record, operation);
  const terminal = isTerminalStatus(status);
  const operationProgress = clampProgress(operation?.progress ?? 0);
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
  const browser = supportedBrowsers.includes(cookie?.browser as (typeof supportedBrowsers)[number])
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
    infoLoaded: recordPreparation(record) !== 'pending',
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
    filename: publishedOutputPath(operation) ?? (same ? (existing?.filename ?? null) : null),
    selected: existing?.selected ?? false
  };
}

export function inspectionDisplayMetadata(
  record: QueueItemRecord,
  operation: OperationSnapshot | null
) {
  if (
    operation?.kind !== 'inspection' ||
    operation.state !== 'completed' ||
    operation.queueItemId !== record.id ||
    operation.inspectionResult?.kind !== 'video'
  )
    return undefined;
  const video = operation.inspectionResult.video;
  const currentAttempt = record.latestOperationId === operation.id;
  const justCompletedCurrentAttempt =
    record.latestOperationId === null &&
    recordPreparation(record) !== 'pending' &&
    sameMediaIdentity(
      { url: record.sourceUrl, selection: record.selection },
      { url: video.url, selection: video.selection }
    );
  if (!currentAttempt && !justCompletedCurrentAttempt) return undefined;
  return { duration: video.duration, channel: video.channel, thumbnail: video.thumbnail };
}

export function assignQueueFields<K extends keyof QueueItem>(
  target: QueueItem,
  source: QueueItem,
  fields: readonly K[]
): void {
  for (const field of fields)
    if (!Object.is(target[field], source[field]))
      (target as Record<keyof QueueItem, unknown>)[field] = source[field];
}

export function projectionRecordChanged(
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
    recordPreparation(previous) !== recordPreparation(next) ||
    previous.availableQualities.length !== next.availableQualities.length ||
    previous.availableQualities.some(
      (quality, index) => quality !== next.availableQualities[index]
    ) ||
    JSON.stringify(previous.cookieConfig) !== JSON.stringify(next.cookieConfig)
  );
}

export function displayProgress(
  item: QueueItem,
  payload: DownloadProgressPayload,
  refresh: boolean
): number {
  if (payload.status === 'completed') return 100;
  if (payload.status === 'postprocessing') {
    const conversion = payload.phase === 'conversion' || payload.conversion_progress != null;
    const value = payload.conversion_progress ?? (conversion ? payload.progress : null);
    if (item.format === 'webm' && conversion && value !== null)
      return refresh ? Math.max(item.progress, clampProgress(value)) : item.progress;
    return 100;
  }
  if (payload.status !== 'downloading') return clampProgress(payload.progress);
  return refresh ? Math.max(item.progress, clampProgress(payload.progress)) : item.progress;
}
export function displayDownloadProgress(
  item: QueueItem,
  payload: DownloadProgressPayload,
  refresh: boolean
): number {
  if (payload.status === 'completed' || payload.status === 'postprocessing') return 100;
  if (payload.status !== 'downloading') return item.downloadProgress;
  const value = payload.download_progress ?? payload.progress;
  return refresh ? Math.max(item.downloadProgress, clampProgress(value)) : item.downloadProgress;
}
export function displayConversionProgress(
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
  return refresh ? Math.max(current, clampProgress(value)) : current;
}
export function displayEta(
  item: QueueItem,
  payload: DownloadProgressPayload,
  refresh: boolean
): string {
  if (payload.status !== 'downloading') return '';
  const next = payload.eta ?? '';
  return next === '' || !refresh ? item.eta : next;
}
export function buildQueueSummary(items: QueueItem[]) {
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

function queueStatus(record: QueueItemRecord, operation: OperationSnapshot | null): DownloadStatus {
  if (operation?.kind === 'inspection') {
    if (operation.state === 'completed') return 'ready';
    if (operation.state === 'failed' || operation.state === 'interrupted') return 'error';
    if (operation.state === 'cancelled') return 'cancelled';
    if (operation.state === 'cancelling') return 'cancelling';
    return 'fetching';
  }
  if (recordPreparation(record) === 'pending') {
    if (record.state === 'failed' || record.state === 'interrupted') return 'error';
    if (record.state === 'cancelled') return 'cancelled';
    return 'fetching';
  }
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
function recordPreparation(record: QueueItemRecord): 'pending' | null | undefined {
  return record.preparation;
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
function clampProgress(value: number): number {
  return Math.min(100, Math.max(0, value));
}

export function formatByteCount(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unitIndex = 0;

  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }

  const digits = value >= 10 || unitIndex === 0 ? 0 : 1;
  return `${value.toFixed(digits)} ${units[unitIndex]}`;
}

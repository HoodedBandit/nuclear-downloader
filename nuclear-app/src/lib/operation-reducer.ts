import type { QueueLifecycleStatus } from './queue-logic';

export type DownloadPhase =
  'download' | 'postprocess' | 'waiting_conversion' | 'conversion' | 'complete';

export interface OperationProjection {
  id: string;
  downloadId: string | null;
  format: string;
  status: QueueLifecycleStatus;
  progress: number;
  downloadProgress: number;
  conversionProgress: number | null;
  phase: DownloadPhase | null;
  speed: string;
  eta: string;
  error: string | null;
  errorCode: string | null;
  errorDetail: string | null;
  filename: string | null;
}

export interface OperationProgress {
  download_id: string;
  status: QueueLifecycleStatus;
  progress: number;
  phase?: DownloadPhase | null;
  download_progress?: number | null;
  conversion_progress?: number | null;
  speed: string | null;
  eta: string | null;
  error: string | null;
  error_code?: string | null;
  error_detail?: string | null;
  filename: string | null;
}

export function isTerminalOperationStatus(status: QueueLifecycleStatus): boolean {
  return status === 'completed' || status === 'error' || status === 'cancelled';
}

export function clampProgress(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.min(100, Math.max(0, value));
}

/**
 * Applies backend progress to a renderer projection. Terminal state is
 * authoritative: it clears transient phase, speed and ETA fields so stale
 * progress events can never make a completed/cancelled row look active.
 */
export function reduceOperationProgress<T extends OperationProjection>(
  item: T,
  payload: OperationProgress
): T {
  const terminal = isTerminalOperationStatus(payload.status);
  const completed = payload.status === 'completed';
  const downloading = payload.status === 'downloading';
  const postprocessing = payload.status === 'postprocessing';
  const nextDownloadProgress =
    completed || postprocessing
      ? 100
      : terminal
        ? clampProgress(payload.download_progress ?? payload.progress)
        : downloading
          ? Math.max(
              item.downloadProgress,
              clampProgress(payload.download_progress ?? payload.progress)
            )
          : item.downloadProgress;
  const conversionReported = payload.phase === 'conversion' || payload.conversion_progress != null;
  const nextConversionProgress =
    completed && item.format === 'webm'
      ? 100
      : postprocessing && conversionReported
        ? Math.max(
            item.conversionProgress ?? 0,
            clampProgress(payload.conversion_progress ?? payload.progress)
          )
        : terminal
          ? null
          : item.conversionProgress;
  const nextProgress = completed
    ? 100
    : postprocessing && !(item.format === 'webm' && conversionReported)
      ? 100
      : downloading
        ? Math.max(item.progress, clampProgress(payload.progress))
        : clampProgress(payload.progress);

  return {
    ...item,
    status: payload.status,
    downloadId: terminal ? null : item.downloadId,
    progress: nextProgress,
    downloadProgress: nextDownloadProgress,
    conversionProgress: nextConversionProgress,
    phase: terminal ? null : (payload.phase ?? item.phase),
    speed: terminal ? '' : (payload.speed ?? ''),
    eta: terminal || !downloading ? '' : (payload.eta ?? item.eta),
    error: payload.error,
    errorCode: payload.error_code ?? null,
    errorDetail: payload.error_detail ?? null,
    filename: payload.filename ?? item.filename
  };
}

export function reduceQueueProgress<T extends OperationProjection>(
  items: T[],
  payload: OperationProgress
): T[] {
  const index = items.findIndex((item) => item.downloadId === payload.download_id);
  if (index === -1) return items;

  const next = items.slice();
  next[index] = reduceOperationProgress(next[index], payload);
  return next;
}

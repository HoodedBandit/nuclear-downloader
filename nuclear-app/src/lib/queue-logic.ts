export type QueueLifecycleStatus =
  | 'fetching'
  | 'ready'
  | 'queued'
  | 'downloading'
  | 'postprocessing'
  | 'cancelling'
  | 'completed'
  | 'error'
  | 'cancelled';

export type SelectionState = 'none' | 'some' | 'all';

const AUDIO_ONLY_FORMATS = new Set(['mp3', 'flac', 'wav', 'aac', 'opus']);

export function isUpdateBlockingStatus(status: QueueLifecycleStatus): boolean {
  return (
    status === 'fetching' ||
    status === 'queued' ||
    status === 'downloading' ||
    status === 'postprocessing' ||
    status === 'cancelling'
  );
}

export function resolveAvailableQuality(
  requestedQuality: string,
  availableQualities: string[]
): string {
  return availableQualities.includes(requestedQuality) ? requestedQuality : 'best';
}

export function isAudioOnlyFormat(format: string): boolean {
  return AUDIO_ONLY_FORMATS.has(format);
}

export function resolveAvailableFormat<T extends string>(
  requestedFormat: T,
  hasAudio: boolean | null,
  fallbackFormat: T
): T {
  return hasAudio === false && isAudioOnlyFormat(requestedFormat)
    ? fallbackFormat
    : requestedFormat;
}

export function deriveSelectionState(items: readonly { selected: boolean }[]): SelectionState {
  if (items.length === 0) return 'none';

  let selectedCount = 0;
  for (const item of items) {
    if (item.selected) selectedCount += 1;
  }

  if (selectedCount === 0) return 'none';
  return selectedCount === items.length ? 'all' : 'some';
}

export function canStartWork(options: {
  runtimeReady: boolean;
  outputDirectoryReady: boolean;
  maintenanceActive: boolean;
}): boolean {
  return options.runtimeReady && options.outputDirectoryReady && !options.maintenanceActive;
}

export function redactDiagnosticText(value: string): string {
  return value
    .replace(/\bhttps?:\/\/[^\s<>"']+/giu, '[url redacted]')
    .replace(/(?:\b[A-Za-z]:\\|\\\\)[^\r\n]*/gu, '[path redacted]')
    .replace(/(--cookies(?:-from-browser)?\s+)(?:"[^"]*"|'[^']*'|\S+)/giu, '$1[redacted]');
}

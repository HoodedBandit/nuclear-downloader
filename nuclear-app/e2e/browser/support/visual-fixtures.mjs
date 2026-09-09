import {
  IDS,
  childVideo,
  initialSnapshot,
  operation,
  queueItem,
  video
} from './renderer-fixture.mjs';

const FIXED_TIME_MS = Date.parse('2026-08-17T12:00:00Z');

function appError(code, summary) {
  return {
    code,
    summary,
    detail: `${summary} Deterministic visual fixture detail.`,
    retryable: true,
    correlationId: `visual-${code}`
  };
}

function queueAndOperation(state, overrides = {}) {
  const item = {
    ...queueItem(IDS.item, video, FIXED_TIME_MS),
    state,
    latestOperationId: IDS.download,
    updatedAtMs: FIXED_TIME_MS,
    ...overrides.item
  };
  const activeOperation = operation(IDS.download, 'download', state, {
    queueItemId: IDS.item,
    createdAtMs: FIXED_TIME_MS,
    updatedAtMs: FIXED_TIME_MS,
    ...overrides.operation
  });
  return { ...structuredClone(initialSnapshot), queue: [item], operations: [activeOperation] };
}

export const visualStates = {
  empty: () => structuredClone(initialSnapshot),
  populated: () => ({
    ...structuredClone(initialSnapshot),
    queue: [queueItem(IDS.item, video, FIXED_TIME_MS)]
  }),
  downloading: () =>
    queueAndOperation('running', {
      operation: { state: 'running', progress: 42, phase: 'download' }
    }),
  converting: () =>
    queueAndOperation('running', {
      item: { format: 'webm' },
      operation: { state: 'running', progress: 68, phase: 'conversion' }
    }),
  cancelled: () =>
    queueAndOperation('cancelled', {
      operation: {
        state: 'cancelled',
        progress: 37,
        phase: 'download',
        finishedAtMs: FIXED_TIME_MS
      }
    }),
  failed: () =>
    queueAndOperation('failed', {
      operation: {
        state: 'failed',
        progress: 23,
        phase: 'download',
        finishedAtMs: FIXED_TIME_MS,
        error: appError('fixture_download_failed', 'The fixture download failed.')
      }
    }),
  interrupted: () =>
    queueAndOperation('interrupted', {
      operation: {
        state: 'interrupted',
        progress: 57,
        phase: 'download',
        finishedAtMs: FIXED_TIME_MS,
        error: appError('fixture_interrupted', 'The fixture download was interrupted.')
      }
    }),
  'persistence-degraded': () => ({
    ...structuredClone(initialSnapshot),
    queue: [queueItem(IDS.item, video, FIXED_TIME_MS)],
    persistenceHealth: {
      degraded: true,
      error: appError('fixture_persistence', 'Fixture persistence is degraded.')
    }
  }),
  'playlist-modal': () => structuredClone(initialSnapshot),
  'update-modal': () => structuredClone(initialSnapshot)
};

export const playlistInspection = operation(IDS.playlistInspection, 'inspection', 'completed', {
  inspectionResult: {
    kind: 'playlist',
    playlist: {
      title: 'Fixture Playlist',
      channel: 'Fixture Channel',
      entry_count: 2,
      truncated: false,
      entries: [
        {
          id: 'child-1',
          title: 'Playlist Child One',
          duration: 15,
          url: childVideo.url,
          thumbnail: null
        },
        {
          id: 'child-2',
          title: 'Playlist Child Two',
          duration: 20,
          url: 'https://fixture.test/child-2',
          thumbnail: null
        }
      ]
    }
  }
});

export const visualFixedTimeMs = FIXED_TIME_MS;

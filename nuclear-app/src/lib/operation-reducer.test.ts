import { describe, expect, it } from 'vitest';
import {
  reduceOperationProgress,
  reduceQueueProgress,
  type OperationProjection,
  type OperationProgress
} from './operation-reducer';

function item(index = 0): OperationProjection {
  return {
    id: `item-${index}`,
    downloadId: `operation-${index}`,
    format: 'webm',
    status: 'downloading',
    progress: 42,
    downloadProgress: 42,
    conversionProgress: null,
    phase: 'waiting_conversion',
    speed: '1 MiB/s',
    eta: '10s',
    error: null,
    errorCode: null,
    errorDetail: null,
    filename: null
  };
}

function progress(
  status: OperationProgress['status'],
  overrides: Partial<OperationProgress> = {}
): OperationProgress {
  return {
    download_id: 'operation-0',
    status,
    progress: 50,
    speed: null,
    eta: null,
    error: null,
    filename: null,
    ...overrides
  };
}

describe('operation projection reducer', () => {
  it.each(['completed', 'error', 'cancelled'] as const)(
    'makes terminal %s state override stale phase data',
    (status) => {
      const next = reduceOperationProgress(
        item(),
        progress(status, {
          phase: 'waiting_conversion',
          speed: 'stale',
          eta: 'stale'
        })
      );

      expect(next.status).toBe(status);
      expect(next.downloadId).toBeNull();
      expect(next.phase).toBeNull();
      expect(next.speed).toBe('');
      expect(next.eta).toBe('');
    }
  );

  it('retains monotonic progress while an operation is active', () => {
    const next = reduceOperationProgress(
      item(),
      progress('downloading', { progress: 20, download_progress: 20 })
    );
    expect(next.progress).toBe(42);
    expect(next.downloadProgress).toBe(42);
  });

  it('meets the 1,000-row reducer p95 budget under the acceptance event load', () => {
    let rows = Array.from({ length: 1_000 }, (_, index) => item(index));
    const samples: number[] = [];

    // Five active items, five events per second, for sixty seconds.
    for (let eventIndex = 0; eventIndex < 5 * 5 * 60; eventIndex += 1) {
      const operationIndex = eventIndex % 5;
      const started = performance.now();
      rows = reduceQueueProgress(rows, {
        ...progress('downloading', {
          download_id: `operation-${operationIndex}`,
          progress: eventIndex % 100,
          download_progress: eventIndex % 100
        })
      });
      samples.push(performance.now() - started);
    }

    samples.sort((left, right) => left - right);
    const p95 = samples[Math.floor(samples.length * 0.95)];
    expect(p95).toBeLessThan(5);
  });
});

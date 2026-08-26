import { describe, expect, it } from 'vitest';
import type { AppSnapshot } from './bindings/AppSnapshot';
import type { QueueItemRecord } from './bindings/QueueItemRecord';
import type { StateDelta } from './bindings/StateDelta';
import {
  SchemaVersionError,
  applyStateDelta,
  latestOperationForItem,
  validateAppSnapshot
} from './backend-state';

const item = (id: string): QueueItemRecord => ({
  schemaVersion: 1,
  id,
  sourceUrl: `https://example.test/${id}`,
  title: id,
  availableQualities: ['720p'],
  hasAudio: true,
  cookieConfig: null,
  format: 'mp4',
  quality: 'best',
  outputDir: 'C:\\Downloads',
  filenameOverride: null,
  compatConfigPath: null,
  state: 'inert',
  latestOperationId: null,
  createdAtMs: 1,
  updatedAtMs: 1
});

const snapshot = (): AppSnapshot => ({
  schemaVersion: 1,
  queue: [item('one')],
  operations: [],
  runtimeReadiness: 'ready',
  maintenanceActive: false,
  draining: false,
  latestSequence: 10
});

describe('backend state contracts', () => {
  it('fails closed for unknown snapshot or nested schema versions', () => {
    expect(() => validateAppSnapshot({ ...snapshot(), schemaVersion: 2 })).toThrow(
      SchemaVersionError
    );
    expect(() =>
      validateAppSnapshot({ ...snapshot(), queue: [{ ...item('one'), schemaVersion: 9 }] })
    ).toThrow(SchemaVersionError);
  });

  it('applies queue updates and removals without mutating the snapshot', () => {
    const initial = snapshot();
    const upsert = {
      schemaVersion: 1,
      sequence: 11,
      emittedAtMs: 2,
      kind: 'queue_item_upserted',
      value: { ...item('one'), title: 'updated' }
    } satisfies StateDelta;
    const updated = applyStateDelta(initial, upsert);
    expect(updated.queue[0].title).toBe('updated');
    expect(initial.queue[0].title).toBe('one');

    const removed = applyStateDelta(updated, {
      schemaVersion: 1,
      sequence: 12,
      emittedAtMs: 3,
      kind: 'queue_items_removed',
      value: ['one']
    });
    expect(removed.queue).toEqual([]);
    expect(removed.latestSequence).toBe(12);
  });

  it('selects the backend-declared latest operation', () => {
    const operation = {
      schemaVersion: 1,
      id: 'operation',
      queueItemId: 'one',
      kind: 'download' as const,
      state: 'running' as const,
      progress: 10,
      phase: 'download',
      sequence: 1,
      createdAtMs: 1,
      updatedAtMs: 2,
      finishedAtMs: null,
      error: null,
      inspectionResult: null,
      correlationId: 'correlation'
    };
    const value = { ...snapshot(), operations: [operation] };
    expect(latestOperationForItem(value, 'one', 'operation')).toBe(operation);
  });
});

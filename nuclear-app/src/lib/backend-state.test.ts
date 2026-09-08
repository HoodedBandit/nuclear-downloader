import { describe, expect, it } from 'vitest';
import type { AppSnapshot } from './bindings/AppSnapshot';
import type { OperationSnapshot } from './bindings/OperationSnapshot';
import type { QueueItemRecord } from './bindings/QueueItemRecord';
import type { StateDelta } from './bindings/StateDelta';
import {
  SchemaVersionError,
  applyStateDelta,
  latestOperationForItem,
  publishedOutputPath,
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
  persistenceHealth: { degraded: false, error: null },
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
      publishedOutput: null,
      intendedTerminalOutcome: null,
      correlationId: 'correlation'
    };
    const value = { ...snapshot(), operations: [operation] };
    expect(latestOperationForItem(value, 'one', 'operation')).toBe(operation);
  });

  it('normalizes legacy persistence and published-output fields to healthy defaults', () => {
    const current = snapshot();
    const { persistenceHealth: _persistenceHealth, ...legacySnapshot } = current;
    const legacyOperation = {
      schemaVersion: 1,
      id: 'legacy-operation',
      queueItemId: 'one',
      kind: 'download' as const,
      state: 'completed' as const,
      progress: 100,
      phase: 'complete',
      sequence: 10,
      createdAtMs: 1,
      updatedAtMs: 2,
      finishedAtMs: 2,
      error: null,
      inspectionResult: null,
      correlationId: 'legacy-correlation'
    };

    const normalized = validateAppSnapshot({
      ...legacySnapshot,
      operations: [legacyOperation]
    } as unknown as AppSnapshot);

    expect(normalized.persistenceHealth).toEqual({ degraded: false, error: null });
    expect(normalized.operations[0].publishedOutput).toBeNull();
    expect(normalized.operations[0].intendedTerminalOutcome).toBeNull();
  });

  it('applies persistence degradation and recovery deltas', () => {
    const failure = {
      code: 'journal_write_failed',
      summary: 'Queue history could not be saved.',
      detail: null,
      retryable: true,
      correlationId: 'persistence-correlation'
    };
    const degraded = applyStateDelta(snapshot(), {
      schemaVersion: 1,
      sequence: 11,
      emittedAtMs: 11,
      kind: 'persistence_health_changed',
      value: { degraded: true, error: failure }
    } satisfies StateDelta);
    expect(degraded.persistenceHealth).toEqual({ degraded: true, error: failure });

    const recovered = applyStateDelta(degraded, {
      schemaVersion: 1,
      sequence: 12,
      emittedAtMs: 12,
      kind: 'persistence_health_changed',
      value: { degraded: false, error: null }
    } satisfies StateDelta);
    expect(recovered.persistenceHealth).toEqual({ degraded: false, error: null });
  });

  it('recovers the published output path from an authoritative operation snapshot', () => {
    const operation = {
      schemaVersion: 1,
      id: 'completed-operation',
      queueItemId: 'one',
      kind: 'download' as const,
      state: 'completed' as const,
      progress: 100,
      phase: 'complete',
      sequence: 10,
      createdAtMs: 1,
      updatedAtMs: 2,
      finishedAtMs: 2,
      error: null,
      inspectionResult: null,
      publishedOutput: { path: 'C:\\Downloads\\Fixture.mp4', recordedAtMs: 2 },
      intendedTerminalOutcome: null,
      correlationId: 'completed-correlation'
    } satisfies OperationSnapshot;

    expect(publishedOutputPath(operation)).toBe('C:\\Downloads\\Fixture.mp4');
  });
});

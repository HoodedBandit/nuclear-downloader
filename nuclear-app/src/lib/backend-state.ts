import type { AppSnapshot } from './bindings/AppSnapshot';
import type { OperationSnapshot } from './bindings/OperationSnapshot';
import type { StateDelta } from './bindings/StateDelta';

export const APP_SCHEMA_VERSION = 1;

export class SchemaVersionError extends Error {
  constructor(public readonly received: number) {
    super(`Unsupported app state schema ${received}; expected ${APP_SCHEMA_VERSION}.`);
    this.name = 'SchemaVersionError';
  }
}

function requireSchema(value: { schemaVersion: number }): void {
  if (value.schemaVersion !== APP_SCHEMA_VERSION) {
    throw new SchemaVersionError(value.schemaVersion);
  }
}

export function validateAppSnapshot(snapshot: AppSnapshot): AppSnapshot {
  requireSchema(snapshot);
  snapshot.queue.forEach(requireSchema);
  snapshot.operations.forEach(requireSchema);
  return snapshot;
}

export function validateStateDelta(delta: StateDelta): StateDelta {
  requireSchema(delta);
  if (delta.kind === 'queue_item_upserted' || delta.kind === 'operation_upserted') {
    requireSchema(delta.value);
  }
  return delta;
}

function upsertById<T extends { id: string }>(items: readonly T[], next: T): T[] {
  const index = items.findIndex((item) => item.id === next.id);
  if (index === -1) return [...items, next];
  const result = items.slice();
  result[index] = next;
  return result;
}

export function applyStateDelta(snapshot: AppSnapshot, unchecked: StateDelta): AppSnapshot {
  const delta = validateStateDelta(unchecked);
  let next: AppSnapshot = { ...snapshot, latestSequence: delta.sequence };

  switch (delta.kind) {
    case 'queue_item_upserted':
      next = { ...next, queue: upsertById(snapshot.queue, delta.value) };
      break;
    case 'queue_items_removed': {
      const removed = new Set(delta.value);
      next = { ...next, queue: snapshot.queue.filter((item) => !removed.has(item.id)) };
      break;
    }
    case 'operation_upserted':
      next = { ...next, operations: upsertById(snapshot.operations, delta.value) };
      break;
    case 'operation_removed':
      next = {
        ...next,
        operations: snapshot.operations.filter((operation) => operation.id !== delta.value)
      };
      break;
    case 'runtime_readiness_changed':
      next = { ...next, runtimeReadiness: delta.value };
      break;
    case 'maintenance_changed':
      next = {
        ...next,
        maintenanceActive: delta.value.active,
        draining: delta.value.draining
      };
      break;
  }

  return next;
}

export function latestOperationForItem(
  snapshot: AppSnapshot,
  itemId: string,
  preferredId: string | null
): OperationSnapshot | null {
  if (preferredId) {
    const preferred = snapshot.operations.find((operation) => operation.id === preferredId);
    return preferred ?? null;
  }

  return (
    snapshot.operations
      .filter((operation) => operation.queueItemId === itemId)
      .sort((left, right) => right.updatedAtMs - left.updatedAtMs)[0] ?? null
  );
}

export function isTerminalOperation(operation: OperationSnapshot): boolean {
  return (
    operation.state === 'completed' ||
    operation.state === 'failed' ||
    operation.state === 'cancelled' ||
    operation.state === 'interrupted'
  );
}

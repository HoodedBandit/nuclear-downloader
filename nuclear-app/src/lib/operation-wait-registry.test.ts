import { afterEach, describe, expect, it, vi } from 'vitest';
import type { OperationSnapshot } from '$lib/bindings/OperationSnapshot';
import { OperationWaitRegistry } from './operation-wait-registry';

function operation(id: string, state: OperationSnapshot['state']): OperationSnapshot {
  return {
    schemaVersion: 1,
    id,
    queueItemId: null,
    kind: 'inspection',
    state,
    progress: 0,
    phase: null,
    sequence: 1,
    createdAtMs: 1,
    updatedAtMs: 1,
    finishedAtMs: state === 'completed' ? 1 : null,
    error: null,
    correlationId: id,
    inspectionResult: null
  };
}

describe('OperationWaitRegistry', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('returns an existing terminal operation without registering a waiter', async () => {
    const registry = new OperationWaitRegistry();
    const completed = operation('op-1', 'completed');

    await expect(registry.wait('op-1', [completed], 1_000)).resolves.toBe(completed);
    expect(registry.size).toBe(0);
  });

  it('settles and removes a waiter when a terminal snapshot arrives', async () => {
    const registry = new OperationWaitRegistry();
    const result = registry.wait('op-1', [operation('op-1', 'running')], 1_000);

    registry.settle([operation('op-1', 'completed')]);

    await expect(result).resolves.toMatchObject({ id: 'op-1', state: 'completed' });
    expect(registry.size).toBe(0);
  });

  it('recovers a terminal operation from an authoritative refresh when its event was missed', async () => {
    vi.useFakeTimers();
    const registry = new OperationWaitRegistry();
    let operations: OperationSnapshot[] = [operation('op-1', 'running')];
    const refresh = vi.fn(async () => {
      operations = [operation('op-1', 'completed')];
    });

    const result = registry.waitWithRefresh('op-1', () => operations, 5_000, refresh, 250);
    await vi.advanceTimersByTimeAsync(250);

    await expect(result).resolves.toMatchObject({ id: 'op-1', state: 'completed' });
    expect(refresh).toHaveBeenCalledOnce();
    expect(registry.size).toBe(0);
  });

  it('rejects the operation waiter when authoritative reconciliation fails', async () => {
    vi.useFakeTimers();
    const registry = new OperationWaitRegistry();
    const result = registry.waitWithRefresh(
      'op-1',
      () => [operation('op-1', 'running')],
      5_000,
      async () => {
        throw new Error('snapshot unavailable');
      },
      250
    );
    const rejection = expect(result).rejects.toThrow('snapshot unavailable');
    await vi.advanceTimersByTimeAsync(250);

    await rejection;
    expect(registry.size).toBe(0);
  });

  it('rejects and removes a waiter at its deadline', async () => {
    vi.useFakeTimers();
    const registry = new OperationWaitRegistry();
    const result = registry.wait('op-1', [], 500);
    const rejection = expect(result).rejects.toThrow('Timed out waiting for operation op-1');

    await vi.advanceTimersByTimeAsync(500);

    await rejection;
    expect(registry.size).toBe(0);
  });

  it('rejects every pending waiter when the state stream fails', async () => {
    const registry = new OperationWaitRegistry();
    const first = registry.wait('op-1', [], 1_000);
    const second = registry.wait('op-2', [], 1_000);
    const firstRejection = expect(first).rejects.toThrow('state stream failed');
    const secondRejection = expect(second).rejects.toThrow('state stream failed');

    registry.rejectAll(new Error('state stream failed'));

    await Promise.all([firstRejection, secondRejection]);
    expect(registry.size).toBe(0);
  });
});

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
    inspectionResult: null,
    publishedOutput: null,
    intendedTerminalOutcome: null
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

  it('clears refresh delays immediately when the state stream fails', async () => {
    vi.useFakeTimers();
    const registry = new OperationWaitRegistry();
    const refresh = vi.fn(async () => {});
    const result = registry.waitWithRefresh('op-1', () => [], 5_000, refresh);
    const rejection = expect(result).rejects.toThrow('state stream failed');

    registry.rejectAll(new Error('state stream failed'));

    await rejection;
    expect(vi.getTimerCount()).toBe(0);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(refresh).not.toHaveBeenCalled();
  });

  it('does not schedule refreshes for an already terminal operation', async () => {
    vi.useFakeTimers();
    const registry = new OperationWaitRegistry();
    const completed = operation('op-1', 'completed');
    const refresh = vi.fn(async () => {});

    await expect(registry.waitWithRefresh('op-1', () => [completed], 5_000, refresh)).resolves.toBe(
      completed
    );

    expect(vi.getTimerCount()).toBe(0);
    expect(refresh).not.toHaveBeenCalled();
  });

  it('settles the previous waiter when a new wait already has terminal evidence', async () => {
    vi.useFakeTimers();
    const registry = new OperationWaitRegistry();
    const refresh = vi.fn(async () => {});
    const first = registry.waitWithRefresh('op-1', () => [], 5_000, refresh);
    const completed = operation('op-1', 'completed');

    await expect(registry.wait('op-1', [completed], 5_000)).resolves.toBe(completed);

    expect(registry.size).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
    await expect(first).resolves.toBe(completed);
    expect(refresh).not.toHaveBeenCalled();
  });

  it('does not reject a replacement waiter when an old refresh fails', async () => {
    vi.useFakeTimers();
    const registry = new OperationWaitRegistry();
    let failRefresh!: (error: Error) => void;
    const refresh = vi.fn(() => new Promise<void>((_, reject) => (failRefresh = reject)));
    const first = registry.waitWithRefresh('op-1', () => [], 5_000, refresh, 250);
    const firstRejection = expect(first).rejects.toThrow('was already being awaited');
    await vi.advanceTimersByTimeAsync(250);
    const second = registry.wait('op-1', [], 5_000);
    const secondOutcome = second.then(
      (value) => value,
      (error: unknown) => error
    );
    await firstRejection;

    failRefresh(new Error('obsolete refresh failed'));
    await vi.advanceTimersByTimeAsync(0);
    registry.settle([operation('op-1', 'completed')]);

    await expect(secondOutcome).resolves.toMatchObject({ id: 'op-1', state: 'completed' });
    expect(vi.getTimerCount()).toBe(0);
  });

  it('does not read or settle a replacement waiter from an old refresh', async () => {
    vi.useFakeTimers();
    const registry = new OperationWaitRegistry();
    let finishRefresh!: () => void;
    const refresh = vi.fn(() => new Promise<void>((resolve) => (finishRefresh = resolve)));
    const readOperations = vi.fn(() => [] as OperationSnapshot[]);
    const first = registry.waitWithRefresh('op-1', readOperations, 5_000, refresh, 250);
    const firstRejection = expect(first).rejects.toThrow('was already being awaited');
    await vi.advanceTimersByTimeAsync(250);
    const second = registry.wait('op-1', [], 5_000);
    await firstRejection;
    readOperations.mockReturnValue([operation('op-1', 'completed')]);

    finishRefresh();
    await vi.advanceTimersByTimeAsync(0);

    expect(readOperations).toHaveBeenCalledOnce();
    expect(registry.size).toBe(1);
    registry.settle([operation('op-1', 'cancelled')]);
    await expect(second).resolves.toMatchObject({ state: 'cancelled' });
    expect(vi.getTimerCount()).toBe(0);
  });

  it('closes current and future waits on disposal without retaining timers', async () => {
    vi.useFakeTimers();
    const registry = new OperationWaitRegistry();
    const refresh = vi.fn(async () => {});
    const result = registry.waitWithRefresh('op-1', () => [], 5_000, refresh);
    const rejection = expect(result).rejects.toThrow('renderer disposed');

    registry.dispose(new Error('renderer disposed'));
    registry.dispose(new Error('second disposal'));

    await rejection;
    await expect(registry.wait('op-2', [], 5_000)).rejects.toThrow('renderer disposed');
    const readOperations = vi.fn(() => []);
    await expect(registry.waitWithRefresh('op-3', readOperations, 5_000, refresh)).rejects.toThrow(
      'renderer disposed'
    );
    expect(readOperations).not.toHaveBeenCalled();
    expect(registry.size).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
    expect(refresh).not.toHaveBeenCalled();
  });

  it.each(['resolve', 'reject'] as const)(
    'ignores a refresh that completes with %s after disposal',
    async (outcome) => {
      vi.useFakeTimers();
      const registry = new OperationWaitRegistry();
      let finishRefresh!: () => void;
      let failRefresh!: (error: Error) => void;
      const refresh = () =>
        new Promise<void>((resolve, reject) => {
          finishRefresh = resolve;
          failRefresh = reject;
        });
      const readOperations = vi.fn(() => []);
      const result = registry.waitWithRefresh('op-1', readOperations, 5_000, refresh, 250);
      const rejection = expect(result).rejects.toThrow('renderer disposed');
      await vi.advanceTimersByTimeAsync(250);

      registry.dispose(new Error('renderer disposed'));
      await rejection;
      if (outcome === 'resolve') finishRefresh();
      else failRefresh(new Error('late failure'));
      await vi.advanceTimersByTimeAsync(0);

      expect(readOperations).toHaveBeenCalledOnce();
      expect(registry.size).toBe(0);
      expect(vi.getTimerCount()).toBe(0);
    }
  );

  it('cancels a replaced deadline while keeping the replacement deadline', async () => {
    vi.useFakeTimers();
    const registry = new OperationWaitRegistry();
    const first = registry.wait('op-1', [], 250);
    const firstRejection = expect(first).rejects.toThrow('was already being awaited');
    const second = registry.wait('op-1', [], 1_000);
    await firstRejection;

    await vi.advanceTimersByTimeAsync(250);
    expect(registry.size).toBe(1);
    expect(vi.getTimerCount()).toBe(1);
    registry.settle([operation('op-1', 'completed')]);
    await expect(second).resolves.toMatchObject({ state: 'completed' });
    expect(vi.getTimerCount()).toBe(0);
  });
});

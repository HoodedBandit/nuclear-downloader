import { describe, expect, it, vi } from 'vitest';
import type { AppSnapshot } from './bindings/AppSnapshot';
import type { StateDelta } from './bindings/StateDelta';
import { AppStateController } from './app-state-controller';

const subscribeNoResync = (): Promise<() => void> => Promise.resolve(() => undefined);

const snapshot = (sequence: number, maintenanceActive = false): AppSnapshot => ({
  schemaVersion: 1,
  queue: [],
  operations: [],
  runtimeReadiness: 'ready',
  maintenanceActive,
  draining: false,
  persistenceHealth: { degraded: false, error: null },
  latestSequence: sequence
});

const maintenance = (sequence: number, active: boolean): StateDelta => ({
  schemaVersion: 1,
  sequence,
  emittedAtMs: sequence,
  kind: 'maintenance_changed',
  value: { active, draining: false }
});

describe('AppStateController', () => {
  it('subscribes before requesting the initial snapshot and applies buffered events', async () => {
    const order: string[] = [];
    let handler: ((delta: StateDelta) => void) | null = null;
    const published: AppSnapshot[] = [];
    const controller = new AppStateController(
      async () => {
        order.push('snapshot');
        handler?.(maintenance(11, true));
        return snapshot(10);
      },
      (value) => published.push(value)
    );

    await controller.start(async (next) => {
      order.push('subscribe');
      handler = next;
      return () => undefined;
    }, subscribeNoResync);

    expect(order).toEqual(['subscribe', 'snapshot']);
    expect(published.at(-1)?.latestSequence).toBe(11);
    expect(published.at(-1)?.maintenanceActive).toBe(true);
  });

  it('discards stale deltas and refetches exactly once on a gap', async () => {
    const load = vi
      .fn<() => Promise<AppSnapshot>>()
      .mockResolvedValueOnce(snapshot(20))
      .mockResolvedValueOnce(snapshot(22, true));
    const published: AppSnapshot[] = [];
    const controller = new AppStateController(load, (value) => published.push(value));
    await controller.start(async () => () => undefined, subscribeNoResync);

    await controller.accept(maintenance(20, true));
    expect(published.at(-1)?.maintenanceActive).toBe(false);
    await controller.accept(maintenance(22, true));

    expect(load).toHaveBeenCalledTimes(2);
    expect(published.at(-1)).toEqual(snapshot(22, true));
  });

  it('refetches a gap observed while the initial snapshot is loading', async () => {
    let handler: ((delta: StateDelta) => void) | null = null;
    const load = vi
      .fn<() => Promise<AppSnapshot>>()
      .mockImplementationOnce(async () => {
        handler?.(maintenance(12, true));
        return snapshot(10);
      })
      .mockResolvedValueOnce(snapshot(12, true));
    const published: AppSnapshot[] = [];
    const controller = new AppStateController(load, (value) => published.push(value));

    await controller.start(async (next) => {
      handler = next;
      return () => undefined;
    }, subscribeNoResync);

    expect(load).toHaveBeenCalledTimes(2);
    expect(published).toEqual([snapshot(12, true)]);
  });

  it('resnapshots until the backend resync sequence is reached', async () => {
    const load = vi
      .fn<() => Promise<AppSnapshot>>()
      .mockResolvedValueOnce(snapshot(5))
      .mockResolvedValueOnce(snapshot(7, true));
    const published: AppSnapshot[] = [];
    const controller = new AppStateController(load, (value) => published.push(value));
    await controller.start(async () => () => undefined, subscribeNoResync);

    await controller.acceptResync({ latestSequence: 7 });

    expect(load).toHaveBeenCalledTimes(2);
    expect(published.at(-1)).toEqual(snapshot(7, true));
  });

  it('does not publish a stale snapshot when resync arrives during its load', async () => {
    let resync: ((request: { latestSequence: number }) => void) | null = null;
    const load = vi
      .fn<() => Promise<AppSnapshot>>()
      .mockImplementationOnce(async () => {
        resync?.({ latestSequence: 12 });
        return snapshot(10);
      })
      .mockResolvedValueOnce(snapshot(12, true));
    const published: AppSnapshot[] = [];
    const controller = new AppStateController(load, (value) => published.push(value));

    await controller.start(
      async () => () => undefined,
      async (handler) => {
        resync = handler;
        return () => undefined;
      }
    );

    expect(load).toHaveBeenCalledTimes(2);
    expect(published).toEqual([snapshot(12, true)]);
  });

  it('honors a newer resync target raised during a retry', async () => {
    let controller: AppStateController | null = null;
    const load = vi
      .fn<() => Promise<AppSnapshot>>()
      .mockResolvedValueOnce(snapshot(1))
      .mockImplementationOnce(async () => {
        void controller?.acceptResync({ latestSequence: 5 });
        return snapshot(3);
      })
      .mockResolvedValueOnce(snapshot(5, true));
    const published: AppSnapshot[] = [];
    controller = new AppStateController(load, (value) => published.push(value));
    await controller.start(async () => () => undefined, subscribeNoResync);

    await controller.acceptResync({ latestSequence: 3 });

    expect(load).toHaveBeenCalledTimes(3);
    expect(published).toEqual([snapshot(1), snapshot(5, true)]);
  });

  it('uses buffered deltas to satisfy a resync target without another fetch', async () => {
    let stateHandler: ((delta: StateDelta) => void) | null = null;
    const load = vi
      .fn<() => Promise<AppSnapshot>>()
      .mockResolvedValueOnce(snapshot(0))
      .mockImplementationOnce(async () => {
        stateHandler?.(maintenance(2, true));
        return snapshot(1);
      });
    const published: AppSnapshot[] = [];
    const controller = new AppStateController(load, (value) => published.push(value));
    await controller.start(async (handler) => {
      stateHandler = handler;
      return () => undefined;
    }, subscribeNoResync);

    await controller.acceptResync({ latestSequence: 2 });

    expect(load).toHaveBeenCalledTimes(2);
    expect(published.at(-1)?.latestSequence).toBe(2);
    expect(published.at(-1)?.maintenanceActive).toBe(true);
  });

  it('fails closed on a future schema without publishing it', async () => {
    const published: AppSnapshot[] = [];
    const unlistenState = vi.fn();
    const unlistenResync = vi.fn();
    const controller = new AppStateController(
      async () => ({ ...snapshot(1), schemaVersion: 2 }),
      (value) => published.push(value)
    );
    await expect(
      controller.start(
        async () => unlistenState,
        async () => unlistenResync
      )
    ).rejects.toThrow('Unsupported app state schema');
    expect(published).toEqual([]);
    expect(unlistenState).toHaveBeenCalledOnce();
    expect(unlistenResync).toHaveBeenCalledOnce();
  });
});

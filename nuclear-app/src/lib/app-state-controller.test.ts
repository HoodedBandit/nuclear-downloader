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
  it('disposes a subscription that resolves after stop without continuing startup', async () => {
    let resolveSubscription!: (unlisten: () => void) => void;
    const unlisten = vi.fn();
    const subscribeResync = vi.fn(async () => () => undefined);
    const load = vi.fn(async () => snapshot(1));
    const controller = new AppStateController(load, vi.fn());
    const starting = controller.start(
      () => new Promise((resolve) => (resolveSubscription = resolve)),
      subscribeResync
    );

    controller.stop();
    resolveSubscription(unlisten);
    await starting;

    expect(unlisten).toHaveBeenCalledOnce();
    expect(subscribeResync).not.toHaveBeenCalled();
    expect(load).not.toHaveBeenCalled();
  });

  it('disposes both subscriptions when stop occurs while the second is pending', async () => {
    let resolveResyncSubscription!: (unlisten: () => void) => void;
    const unlistenState = vi.fn();
    const unlistenResync = vi.fn();
    const load = vi.fn(async () => snapshot(1));
    const controller = new AppStateController(load, vi.fn());
    const starting = controller.start(
      async () => unlistenState,
      () => new Promise((resolve) => (resolveResyncSubscription = resolve))
    );
    await vi.waitFor(() => expect(resolveResyncSubscription).toBeTypeOf('function'));

    controller.stop();
    resolveResyncSubscription(unlistenResync);
    await starting;

    expect(unlistenState).toHaveBeenCalledOnce();
    expect(unlistenResync).toHaveBeenCalledOnce();
    expect(load).not.toHaveBeenCalled();
  });

  it('retains a second subscription rejection while cleaning the first subscription', async () => {
    const unlistenState = vi.fn();
    const controller = new AppStateController(async () => snapshot(1), vi.fn());

    await expect(
      controller.start(
        async () => unlistenState,
        async () => {
          throw new Error('resync subscription failed');
        }
      )
    ).rejects.toThrow('resync subscription failed');

    expect(unlistenState).toHaveBeenCalledOnce();
    expect(controller.current()).toBeNull();
  });

  it('buffers resync during registration and loads only after both subscriptions resolve', async () => {
    let resolveResyncSubscription!: (unlisten: () => void) => void;
    let resyncHandler: ((request: { latestSequence: number }) => void) | null = null;
    const load = vi.fn(async () => snapshot(7));
    const controller = new AppStateController(load, vi.fn());
    const starting = controller.start(
      async () => () => undefined,
      (handler) => {
        resyncHandler = handler;
        return new Promise((resolve) => (resolveResyncSubscription = resolve));
      }
    );

    await vi.waitFor(() => expect(resyncHandler).not.toBeNull());
    (resyncHandler as unknown as (request: { latestSequence: number }) => void)({
      latestSequence: 7
    });
    await Promise.resolve();
    expect(load).not.toHaveBeenCalled();

    resolveResyncSubscription(() => undefined);
    await starting;
    expect(load).toHaveBeenCalledOnce();
    expect(controller.current()?.latestSequence).toBe(7);
  });

  it('ignores old handlers and a stale load after a newer start', async () => {
    let oldHandler: ((delta: StateDelta) => void) | null = null;
    let resolveOldLoad!: (value: AppSnapshot) => void;
    const published: AppSnapshot[] = [];
    const errors: unknown[] = [];
    const load = vi
      .fn<() => Promise<AppSnapshot>>()
      .mockImplementationOnce(() => new Promise((resolve) => (resolveOldLoad = resolve)))
      .mockResolvedValueOnce(snapshot(20));
    const controller = new AppStateController(
      load,
      (value) => published.push(value),
      (error) => {
        errors.push(error);
      }
    );
    const oldStart = controller.start(async (handler) => {
      oldHandler = handler;
      return () => undefined;
    }, subscribeNoResync);
    await vi.waitFor(() => expect(load).toHaveBeenCalledOnce());

    await controller.start(async () => () => undefined, subscribeNoResync);
    (oldHandler as unknown as (delta: StateDelta) => void)(maintenance(21, true));
    resolveOldLoad(snapshot(1, true));
    await oldStart;

    expect(controller.current()).toEqual(snapshot(20));
    expect(published).toEqual([snapshot(20)]);
    expect(errors).toEqual([]);
  });

  it('ignores a stale load failure after a newer start succeeds', async () => {
    let rejectOldLoad!: (error: Error) => void;
    const errors: unknown[] = [];
    const load = vi
      .fn<() => Promise<AppSnapshot>>()
      .mockImplementationOnce(() => new Promise((_, reject) => (rejectOldLoad = reject)))
      .mockResolvedValueOnce(snapshot(30));
    const controller = new AppStateController(load, vi.fn(), (error) => errors.push(error));
    const oldStart = controller.start(async () => () => undefined, subscribeNoResync);
    await vi.waitFor(() => expect(load).toHaveBeenCalledOnce());

    await controller.start(async () => () => undefined, subscribeNoResync);
    rejectOldLoad(new Error('stale snapshot failure'));

    await expect(oldStart).resolves.toBeUndefined();
    expect(controller.current()).toEqual(snapshot(30));
    expect(errors).toEqual([]);
  });

  it('does not let an old load finalizer clear a newer pending reload slot', async () => {
    let resolveOldLoad!: (value: AppSnapshot) => void;
    let resolveNewLoad!: (value: AppSnapshot) => void;
    const load = vi
      .fn<() => Promise<AppSnapshot>>()
      .mockImplementationOnce(() => new Promise((resolve) => (resolveOldLoad = resolve)))
      .mockImplementationOnce(() => new Promise((resolve) => (resolveNewLoad = resolve)));
    const controller = new AppStateController(load, vi.fn());
    const oldStart = controller.start(async () => () => undefined, subscribeNoResync);
    await vi.waitFor(() => expect(load).toHaveBeenCalledTimes(1));
    const newStart = controller.start(async () => () => undefined, subscribeNoResync);
    await vi.waitFor(() => expect(load).toHaveBeenCalledTimes(2));

    resolveOldLoad(snapshot(1));
    await oldStart;
    const deduplicatedReload = controller.reload();
    expect(load).toHaveBeenCalledTimes(2);

    resolveNewLoad(snapshot(2));
    await Promise.all([newStart, deduplicatedReload]);
    expect(load).toHaveBeenCalledTimes(2);
    expect(controller.current()).toEqual(snapshot(2));
  });

  it('preserves buffered deltas when a pending reload is requested again', async () => {
    let resolveReload!: (value: AppSnapshot) => void;
    const load = vi
      .fn<() => Promise<AppSnapshot>>()
      .mockResolvedValueOnce(snapshot(1))
      .mockImplementationOnce(() => new Promise((resolve) => (resolveReload = resolve)));
    const controller = new AppStateController(load, vi.fn());
    await controller.start(async () => () => undefined, subscribeNoResync);

    const firstReload = controller.reload();
    await vi.waitFor(() => expect(load).toHaveBeenCalledTimes(2));
    await controller.accept(maintenance(2, true));
    const coalescedReload = controller.reload();
    resolveReload(snapshot(1));
    await Promise.all([firstReload, coalescedReload]);

    expect(load).toHaveBeenCalledTimes(2);
    expect(controller.current()).toEqual(snapshot(2, true));
  });

  it('disposes each completed generation exactly once across repeated start and stop', async () => {
    const listeners = [vi.fn(), vi.fn(), vi.fn(), vi.fn()];
    let index = 0;
    const subscribe = async () => listeners[index++];
    const controller = new AppStateController(async () => snapshot(index), vi.fn());

    await controller.start(subscribe, subscribe);
    await controller.start(subscribe, subscribe);
    controller.stop();
    controller.stop();

    for (const unlisten of listeners) expect(unlisten).toHaveBeenCalledOnce();
  });

  it('defers a public reload until listener registration completes', async () => {
    let resolveResyncSubscription!: (unlisten: () => void) => void;
    const load = vi.fn(async () => snapshot(4));
    const controller = new AppStateController(load, vi.fn());
    const starting = controller.start(
      async () => () => undefined,
      () => new Promise((resolve) => (resolveResyncSubscription = resolve))
    );
    await vi.waitFor(() => expect(resolveResyncSubscription).toBeTypeOf('function'));

    await controller.reload();
    expect(load).not.toHaveBeenCalled();
    resolveResyncSubscription(() => undefined);
    await starting;

    expect(load).toHaveBeenCalledOnce();
  });

  it('does not begin a queued reload after stop in the same turn', async () => {
    const load = vi.fn(async () => snapshot(1));
    const controller = new AppStateController(load, vi.fn());
    await controller.start(async () => () => undefined, subscribeNoResync);
    load.mockClear();

    const reloading = controller.reload();
    controller.stop();
    await reloading;

    expect(load).not.toHaveBeenCalled();
  });

  it('releases every listener and retains the primary startup failure', async () => {
    const firstUnlisten = vi.fn(() => {
      throw new Error('first cleanup failed');
    });
    const secondUnlisten = vi.fn();
    const cleanupErrors: unknown[] = [];
    const controller = new AppStateController(
      async () => {
        throw new Error('snapshot failed');
      },
      vi.fn(),
      (error) => cleanupErrors.push(error)
    );

    await expect(
      controller.start(
        async () => firstUnlisten,
        async () => secondUnlisten
      )
    ).rejects.toThrow('snapshot failed');
    expect(firstUnlisten).toHaveBeenCalledOnce();
    expect(secondUnlisten).toHaveBeenCalledOnce();
    expect(cleanupErrors).toHaveLength(1);
  });

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

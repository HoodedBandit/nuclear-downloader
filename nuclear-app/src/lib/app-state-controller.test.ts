import { describe, expect, it, vi } from 'vitest';
import type { AppSnapshot } from './bindings/AppSnapshot';
import type { StateDelta } from './bindings/StateDelta';
import { AppStateController } from './app-state-controller';

const snapshot = (sequence: number, maintenanceActive = false): AppSnapshot => ({
  schemaVersion: 1,
  queue: [],
  operations: [],
  runtimeReadiness: 'ready',
  maintenanceActive,
  draining: false,
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
    });

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
    await controller.start(async () => () => undefined);

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
    });

    expect(load).toHaveBeenCalledTimes(2);
    expect(published).toEqual([snapshot(12, true)]);
  });

  it('fails closed on a future schema without publishing it', async () => {
    const published: AppSnapshot[] = [];
    const unlisten = vi.fn();
    const controller = new AppStateController(
      async () => ({ ...snapshot(1), schemaVersion: 2 }),
      (value) => published.push(value)
    );
    await expect(controller.start(async () => unlisten)).rejects.toThrow(
      'Unsupported app state schema'
    );
    expect(published).toEqual([]);
    expect(unlisten).toHaveBeenCalledOnce();
  });
});

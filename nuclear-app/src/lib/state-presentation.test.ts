import { describe, expect, it, vi } from 'vitest';
import { AppStateController } from './app-state-controller';
import type { AppSnapshot } from './bindings/AppSnapshot';
import type { QueueItemRecord } from './bindings/QueueItemRecord';
import type { StateDelta } from './bindings/StateDelta';
import { createQueuePresentationState, QueuePresentationController } from './queue-presentation';

const record: QueueItemRecord = {
  schemaVersion: 1,
  id: 'item-1',
  sourceUrl: 'https://fixture.test/video',
  title: 'Video',
  availableQualities: ['1080p'],
  hasAudio: true,
  cookieConfig: null,
  format: 'mp4',
  quality: 'best',
  outputDir: 'C:\\output',
  filenameOverride: 'Current filename',
  compatConfigPath: null,
  state: 'inert',
  latestOperationId: null,
  createdAtMs: 1,
  updatedAtMs: 10
};

function snapshot(sequence = 10): AppSnapshot {
  return {
    schemaVersion: 1,
    queue: [{ ...record }],
    operations: [],
    runtimeReadiness: 'ready',
    maintenanceActive: false,
    draining: false,
    persistenceHealth: { degraded: false, error: null },
    latestSequence: sequence
  };
}

async function setup(load = vi.fn(async () => snapshot())) {
  const state = createQueuePresentationState();
  const presentation = new QueuePresentationController(state, {
    invoke: async () => {
      throw new Error('This projection test must not invoke a command.');
    },
    isActive: () => true,
    unloadedError: new Error('unloaded')
  });
  const publish = vi.fn((value: AppSnapshot, delta?: StateDelta) =>
    presentation.applySnapshot(value, delta)
  );
  const controller = new AppStateController(load, publish);
  await controller.start(
    async () => () => undefined,
    async () => () => undefined
  );
  publish.mockClear();
  return { controller, state, load, publish };
}

describe('authoritative state publication to queue presentation', () => {
  it('publishes a current edit immediately without requesting another snapshot', async () => {
    const { controller, state, load, publish } = await setup();
    await controller.accept({
      schemaVersion: 1,
      sequence: 11,
      emittedAtMs: 11,
      kind: 'queue_item_upserted',
      value: { ...record, filenameOverride: 'New filename', updatedAtMs: 11 }
    });

    expect(controller.current()?.latestSequence).toBe(11);
    expect(state.items[0].customFilename).toBe('New filename');
    expect(load).toHaveBeenCalledOnce();
    expect(publish).toHaveBeenCalledOnce();
    controller.stop();
  });

  it('does not let an older upsert overwrite a filename already loaded from a snapshot', async () => {
    const { controller, state, publish } = await setup();
    await controller.accept({
      schemaVersion: 1,
      sequence: 9,
      emittedAtMs: 9,
      kind: 'queue_item_upserted',
      value: { ...record, filenameOverride: 'Old filename', updatedAtMs: 9 }
    });

    expect(controller.current()?.queue[0].filenameOverride).toBe('Current filename');
    expect(state.items[0].customFilename).toBe('Current filename');
    expect(publish).not.toHaveBeenCalled();
    controller.stop();
  });

  it('does not let a duplicate removal delete a row restored by the current snapshot', async () => {
    const { controller, state, publish } = await setup();
    await controller.accept({
      schemaVersion: 1,
      sequence: 10,
      emittedAtMs: 10,
      kind: 'queue_items_removed',
      value: [record.id]
    });

    expect(controller.current()?.queue).toHaveLength(1);
    expect(state.items.map((item) => item.id)).toEqual([record.id]);
    expect(publish).not.toHaveBeenCalled();
    controller.stop();
  });

  it('keeps buffered edits unpublished until reload installs the matching authoritative state', async () => {
    let resolveReload!: (value: AppSnapshot) => void;
    const load = vi
      .fn<() => Promise<AppSnapshot>>()
      .mockResolvedValueOnce(snapshot())
      .mockImplementationOnce(() => new Promise((resolve) => (resolveReload = resolve)));
    const { controller, state, publish } = await setup(load);
    const reloading = controller.reload();
    await vi.waitFor(() => expect(load).toHaveBeenCalledTimes(2));
    await controller.accept({
      schemaVersion: 1,
      sequence: 11,
      emittedAtMs: 11,
      kind: 'queue_item_upserted',
      value: { ...record, filenameOverride: 'New filename', updatedAtMs: 11 }
    });

    expect(state.items[0].customFilename).toBe('Current filename');
    expect(publish).not.toHaveBeenCalled();
    resolveReload(snapshot());
    await reloading;

    expect(controller.current()?.latestSequence).toBe(11);
    expect(controller.current()?.queue[0].filenameOverride).toBe('New filename');
    expect(state.items[0].customFilename).toBe('New filename');
    expect(publish).toHaveBeenCalledOnce();
    controller.stop();
  });
});

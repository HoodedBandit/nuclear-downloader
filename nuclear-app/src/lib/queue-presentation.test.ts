import { describe, expect, it, vi } from 'vitest';
import type { AppSnapshot } from './bindings/AppSnapshot';
import type { OperationSnapshot } from './bindings/OperationSnapshot';
import type { QueueItemRecord } from './bindings/QueueItemRecord';
import type { DownloadProgressPayload } from './frontend-types';
import {
  QueuePresentationController,
  createQueuePresentationState,
  sanitizeFilenameDraft
} from './queue-presentation';

function record(id = 'item-1'): QueueItemRecord {
  return {
    schemaVersion: 1,
    id,
    sourceUrl: `https://fixture.test/${id}`,
    title: `Title ${id}`,
    availableQualities: ['1080p'],
    hasAudio: true,
    cookieConfig: null,
    format: 'mp4',
    quality: 'best',
    outputDir: 'C:\\output',
    filenameOverride: null,
    compatConfigPath: null,
    state: 'inert',
    latestOperationId: null,
    createdAtMs: 1,
    updatedAtMs: 1
  };
}

function operation(state: OperationSnapshot['state'], progress = 0): OperationSnapshot {
  return {
    schemaVersion: 1,
    id: 'operation-1',
    queueItemId: 'item-1',
    kind: 'download',
    state,
    progress,
    phase: state === 'running' ? 'download' : null,
    sequence: 1,
    createdAtMs: 1,
    updatedAtMs: 1,
    finishedAtMs: state === 'completed' ? 2 : null,
    error: null,
    inspectionResult: null,
    publishedOutput: null,
    intendedTerminalOutcome: null,
    correlationId: 'correlation-1'
  };
}

function snapshot(queue = [record()], operations: OperationSnapshot[] = []): AppSnapshot {
  return {
    schemaVersion: 1,
    queue,
    operations,
    runtimeReadiness: 'ready',
    maintenanceActive: false,
    draining: false,
    persistenceHealth: { degraded: false, error: null },
    latestSequence: 1
  };
}

function setup(now = vi.fn(() => 1_000)) {
  const state = createQueuePresentationState();
  const invoke = vi.fn(async () => undefined);
  const focus = vi.fn(async () => undefined);
  const controller = new QueuePresentationController(state, {
    invoke: invoke as never,
    isActive: () => true,
    unloadedError: new Error('unloaded'),
    focusFilenameEditor: focus,
    now
  });
  return { state, invoke, focus, controller, now };
}

function progress(
  value: number,
  overrides: Partial<DownloadProgressPayload> = {}
): DownloadProgressPayload {
  return {
    download_id: 'operation-1',
    status: 'downloading',
    progress: value,
    download_progress: value,
    conversion_progress: null,
    phase: 'download',
    speed: '1 MiB/s',
    eta: '10s',
    error: null,
    error_code: null,
    error_detail: null,
    filename: null,
    ...overrides
  };
}

describe('QueuePresentationController', () => {
  it('projects retained metadata and preserves row-owned selection across snapshots', () => {
    const { controller, state } = setup();
    controller.retainMetadata('https://fixture.test/item-1', {
      duration: 65,
      channel: 'Fixture Channel',
      thumbnail: 'fixture.jpg'
    });
    controller.applySnapshot(snapshot());
    controller.setSelected('item-1', true);
    controller.toggleDiagnostics('item-1');
    controller.applySnapshot(snapshot([{ ...record(), updatedAtMs: 2 }]));

    expect(state.items[0]).toMatchObject({
      duration: 65,
      channel: 'Fixture Channel',
      thumbnail: 'fixture.jpg',
      selected: true,
      diagnosticsOpen: true
    });
  });

  it('ignores a projection-irrelevant delta and applies terminal operation state', () => {
    const { controller, state } = setup();
    const initial = snapshot();
    controller.applySnapshot(initial);
    const identity = state.items[0];
    const unchanged = { ...record(), updatedAtMs: 2 };
    controller.applySnapshot(snapshot([unchanged]), {
      schemaVersion: 1,
      sequence: 2,
      emittedAtMs: 2,
      kind: 'queue_item_upserted',
      value: unchanged
    });
    expect(state.items[0]).toBe(identity);

    const completed = operation('completed', 100);
    const completedRecord = {
      ...record(),
      state: 'completed' as const,
      latestOperationId: completed.id
    };
    controller.applySnapshot(snapshot([completedRecord], [completed]), {
      schemaVersion: 1,
      sequence: 3,
      emittedAtMs: 3,
      kind: 'operation_upserted',
      value: completed
    });
    expect(state.items[0]).toMatchObject({ status: 'completed', progress: 100, downloadId: null });
  });

  it('throttles display-only progress while retaining errors and terminal transitions', () => {
    const now = vi.fn(() => 1_000);
    const { controller, state } = setup(now);
    const running = operation('running', 0);
    const queued = { ...record(), state: 'running' as const, latestOperationId: running.id };
    controller.applySnapshot(snapshot([queued], [running]));
    controller.applyProgress(progress(10));
    now.mockReturnValue(1_100);
    controller.applyProgress(progress(20, { eta: '9s' }));
    expect(state.items[0].progress).toBe(10);
    controller.applyProgress(progress(20, { error: 'failed', status: 'error' }));
    expect(state.items[0]).toMatchObject({ status: 'error', error: 'failed' });
  });

  it('refreshes at 500ms and immediately while downloading ETA is empty', () => {
    const now = vi.fn(() => 1_000);
    const { controller, state } = setup(now);
    const running = operation('running');
    controller.applySnapshot(
      snapshot([{ ...record(), state: 'running', latestOperationId: running.id }], [running])
    );
    controller.applyProgress(progress(10));
    now.mockReturnValue(1_499);
    controller.applyProgress(progress(20));
    expect(state.items[0].progress).toBe(10);
    now.mockReturnValue(1_500);
    controller.applyProgress(progress(20));
    expect(state.items[0].progress).toBe(20);

    state.items[0].eta = '';
    now.mockReturnValue(1_501);
    controller.applyProgress(progress(30, { eta: '8s' }));
    expect(state.items[0]).toMatchObject({ progress: 30, eta: '8s' });
  });

  it('ignores progress for another operation and active progress after terminal state', () => {
    const { controller, state } = setup();
    const running = operation('running');
    controller.applySnapshot(
      snapshot([{ ...record(), state: 'running', latestOperationId: running.id }], [running])
    );
    const before = { ...state.items[0] };
    controller.applyProgress({ ...progress(50), download_id: 'another-operation' });
    expect(state.items[0]).toEqual(before);
    controller.applyProgress(progress(100, { status: 'completed' }));
    controller.applyProgress(progress(20));
    expect(state.items[0]).toMatchObject({ status: 'completed', progress: 100, downloadId: null });
  });

  it('projects WebM download and conversion phases independently', () => {
    const { controller, state } = setup();
    const running = operation('running');
    controller.applySnapshot(
      snapshot(
        [{ ...record(), format: 'webm', state: 'running', latestOperationId: running.id }],
        [running]
      )
    );
    controller.applyProgress(progress(40));
    controller.applyProgress(
      progress(20, { status: 'postprocessing', phase: 'conversion', conversion_progress: 20 })
    );
    expect(state.items[0]).toMatchObject({
      status: 'postprocessing',
      progress: 40,
      downloadProgress: 100,
      conversionProgress: 20,
      phase: 'conversion'
    });
  });

  it('preserves unaffected row identity and selection across upserts and removals', () => {
    const { controller, state } = setup();
    const first = record('first');
    const second = record('second');
    controller.applySnapshot(snapshot([first, second]));
    controller.setSelected('second', true);
    const retained = state.items[1];
    const changed = { ...first, title: 'Changed first', updatedAtMs: 2 };
    controller.applySnapshot(snapshot([changed, second]), {
      schemaVersion: 1,
      sequence: 2,
      emittedAtMs: 2,
      kind: 'queue_item_upserted',
      value: changed
    });
    expect(state.items[1]).toBe(retained);
    expect(state.items[1].selected).toBe(true);
    controller.applySnapshot(snapshot([second]), {
      schemaVersion: 1,
      sequence: 3,
      emittedAtMs: 3,
      kind: 'queue_items_removed',
      value: ['first']
    });
    expect(state.items).toEqual([retained]);
  });

  it('projects published filenames and interrupted failure diagnostics', () => {
    const { controller, state } = setup();
    const completed = {
      ...operation('completed', 100),
      publishedOutput: { path: 'C:\\output\\done.mp4', recordedAtMs: 2 }
    };
    controller.applySnapshot(
      snapshot([{ ...record(), state: 'completed', latestOperationId: completed.id }], [completed])
    );
    expect(state.items[0].filename).toBe('C:\\output\\done.mp4');
    const interrupted = {
      ...operation('interrupted'),
      error: {
        code: 'stopped',
        summary: 'Stopped during shutdown.',
        detail: 'safe detail',
        retryable: true,
        correlationId: 'correlation-1'
      },
      intendedTerminalOutcome: { state: 'failed' as const, error: null }
    };
    controller.applySnapshot(
      snapshot(
        [{ ...record(), state: 'interrupted', latestOperationId: interrupted.id }],
        [interrupted]
      )
    );
    expect(state.items[0]).toMatchObject({
      status: 'error',
      error: 'Stopped during shutdown.',
      errorCode: 'stopped',
      errorDetail: 'safe detail\nCorrelation ID: correlation-1'
    });
  });

  it('preserves filename sanitizing, command payload, and focus', async () => {
    const { controller, state, invoke, focus } = setup();
    controller.applySnapshot(snapshot());
    await controller.beginFilenameEdit(state.items[0]);
    expect(focus).toHaveBeenCalledOnce();
    state.editing.draft = 'CON.txt';
    await controller.commitFilenameEdit();
    expect(invoke).toHaveBeenCalledWith('update_queue_item', {
      itemId: 'item-1',
      input: { filenameOverride: 'CON_.txt' }
    });
    expect(state.editing.itemId).toBeNull();
    expect(sanitizeFilenameDraft('   ')).toBe('');
  });

  it('retains filename draft on rejection or disposal and validates blank input', async () => {
    const state = createQueuePresentationState();
    let active = true;
    const invoke = vi
      .fn()
      .mockRejectedValueOnce(new Error('rename rejected'))
      .mockImplementationOnce(async () => {
        active = false;
      });
    const clear = vi.fn();
    const controller = new QueuePresentationController(state, {
      invoke: invoke as never,
      isActive: () => active,
      unloadedError: new Error('unloaded'),
      clearFilenameEditor: clear
    });
    controller.applySnapshot(snapshot());
    await controller.beginFilenameEdit(state.items[0]);
    state.editing.draft = '   ';
    await controller.commitFilenameEdit();
    expect(state.editing.error).toBe('Filename must contain at least one valid character.');
    state.editing.draft = 'Rejected.mp4';
    await controller.commitFilenameEdit();
    expect(state.editing).toEqual({
      itemId: 'item-1',
      draft: 'Rejected.mp4',
      error: 'rename rejected'
    });
    state.editing.draft = 'Disposed.mp4';
    await controller.commitFilenameEdit();
    expect(state.editing).toEqual({
      itemId: 'item-1',
      draft: 'Disposed.mp4',
      error: 'rename rejected'
    });
    expect(clear).not.toHaveBeenCalled();
  });

  it('derives selection summaries and exact virtual row spacers', () => {
    const { controller } = setup();
    controller.applySnapshot(
      snapshot(Array.from({ length: 40 }, (_, index) => record(`item-${index}`)))
    );
    controller.setAllSelected(true);
    controller.setViewport(530, 530);

    expect(controller.selectionState()).toBe('all');
    expect(controller.summary()).toMatchObject({ hasSelected: true, hasSelectedReady: true });
    expect(controller.window()).toMatchObject({
      start: 2,
      end: 28,
      topSpacerHeight: 106,
      bottomSpacerHeight: 636
    });
  });
});

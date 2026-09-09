import { describe, expect, it, vi } from 'vitest';
import type { invokeCommand } from './ipc-client';
import type { QueueItem } from './frontend-types';
import {
  createQueueActionState,
  QueueActionsController,
  type QueueActionDependencies
} from './queue-actions';

function item(overrides: Partial<QueueItem> = {}): QueueItem {
  return {
    id: 'one',
    downloadId: null,
    url: 'https://example.test/one',
    title: 'One',
    customFilename: null,
    duration: null,
    channel: null,
    thumbnail: null,
    infoLoaded: true,
    hasAudio: true,
    status: 'ready',
    quality: 'best',
    format: 'mp4',
    cookieConfig: null,
    availableQualities: ['best', '1080p'],
    progress: 0,
    downloadProgress: 0,
    conversionProgress: null,
    phase: null,
    speed: '',
    eta: '',
    error: null,
    errorCode: null,
    errorDetail: null,
    diagnosticsOpen: false,
    filename: null,
    selected: false,
    ...overrides
  };
}

function setup(initialItems: QueueItem[] = [item()]) {
  const items = initialItems;
  let active = true;
  let canStart = true;
  let editingTitleId: string | null = null;
  let globalQuality = '1080p';
  let globalFormat: QueueItem['format'] = 'mp3';
  const invokeMock = vi.fn();
  const clearProgressDisplayState = vi.fn();
  const cancelFilenameEdit = vi.fn();
  const reloadAppSnapshot = vi.fn(async () => undefined);
  const dependencies: QueueActionDependencies = {
    invoke: invokeMock as unknown as typeof invokeCommand,
    isActive: () => active,
    unloadedError: new Error('unloaded'),
    getItems: () => items,
    replaceItem: (itemId, replace) => {
      const index = items.findIndex((candidate) => candidate.id === itemId);
      if (index === -1) return;
      items[index] = replace(items[index]);
    },
    getCanStartDownloads: () => canStart,
    getGlobalQuality: () => globalQuality,
    getGlobalFormat: () => globalFormat,
    clearProgressDisplayState,
    getEditingTitleId: () => editingTitleId,
    cancelFilenameEdit,
    reloadAppSnapshot
  };
  const state = createQueueActionState();
  const controller = new QueueActionsController(state, dependencies);
  return {
    controller,
    state,
    invokeMock,
    clearProgressDisplayState,
    cancelFilenameEdit,
    reloadAppSnapshot,
    getItems: () => items,
    setActive: (value: boolean) => (active = value),
    setCanStart: (value: boolean) => (canStart = value),
    setEditingTitleId: (value: string | null) => (editingTitleId = value),
    setGlobalQuality: (value: string) => (globalQuality = value),
    setGlobalFormat: (value: QueueItem['format']) => (globalFormat = value)
  };
}

describe('QueueActionsController', () => {
  it('deduplicates enqueue payloads and preserves front/normal priority', async () => {
    const test = setup();
    test.invokeMock.mockResolvedValue([]);
    await test.controller.enqueueItems(['one', 'one', 'two'], true);
    await test.controller.enqueueItems(['two']);
    expect(test.invokeMock.mock.calls).toEqual([
      ['enqueue_queue_items', { itemIds: ['one', 'two'], priority: 'front' }],
      ['enqueue_queue_items', { itemIds: ['two'], priority: 'normal' }]
    ]);
  });

  it('filters all and selected downloads and honors guards', async () => {
    const test = setup([
      item({ id: 'ready' }),
      item({ id: 'selected', selected: true }),
      item({ id: 'done', status: 'completed', selected: true })
    ]);
    test.invokeMock.mockResolvedValue([]);
    await test.controller.downloadAll();
    await test.controller.downloadSelected();
    test.setCanStart(false);
    await test.controller.downloadItem(item({ id: 'blocked' }));
    expect(test.invokeMock.mock.calls).toEqual([
      ['enqueue_queue_items', { itemIds: ['ready', 'selected'], priority: 'normal' }],
      ['enqueue_queue_items', { itemIds: ['selected'], priority: 'normal' }]
    ]);
  });

  it('optimistically cancels and rolls back the exact status on failure', async () => {
    const original = item({
      status: 'postprocessing',
      downloadId: 'operation-1',
      error: 'old',
      errorCode: 'old_code',
      errorDetail: 'old detail'
    });
    const test = setup([original]);
    let rejectCancel!: (reason: unknown) => void;
    test.invokeMock.mockImplementation(
      () => new Promise((_resolve, reject) => (rejectCancel = reject))
    );
    const pending = test.controller.cancelItem(original);
    expect(test.getItems()[0]).toMatchObject({ status: 'cancelling', error: null });
    rejectCancel({ safe_summary: 'backend refused', code: 'busy' });
    await pending;
    expect(test.getItems()[0]).toMatchObject({
      status: 'postprocessing',
      error: 'Cancellation failed: backend refused (busy)',
      errorCode: 'cancel_failed',
      diagnosticsOpen: true
    });
  });

  it('does not overwrite a newer item status when cancellation fails', async () => {
    const original = item({ status: 'downloading', downloadId: 'operation-1' });
    const test = setup([original]);
    let rejectCancel!: (reason: unknown) => void;
    test.invokeMock.mockImplementation(
      () => new Promise((_resolve, reject) => (rejectCancel = reject))
    );
    const pending = test.controller.cancelItem(original);
    test.getItems()[0] = { ...test.getItems()[0], status: 'completed' };
    rejectCancel(new Error('late'));
    await pending;
    expect(test.getItems()[0].status).toBe('completed');
  });

  it('does not report late cancellation failures after disposal', async () => {
    const original = item({ status: 'downloading', downloadId: 'operation-1' });
    const test = setup([original]);
    test.invokeMock.mockRejectedValue(new Error('late'));
    test.setActive(false);
    await test.controller.cancelItem(original);
    expect(test.getItems()[0].status).toBe('cancelling');
    expect(test.state.queueActionError).toBeNull();
  });

  it('reports cancel-all timeout counts and failures exactly', async () => {
    const test = setup();
    test.invokeMock.mockResolvedValueOnce({ idle: false, remainingOperationIds: ['a'] });
    await test.controller.cancelAll();
    expect(test.state.cancelAllError).toBe(
      'Cancellation timed out with 1 operation still stopping. New work remains paused.'
    );
    test.invokeMock.mockRejectedValueOnce(new Error('drain failed'));
    await test.controller.cancelAll();
    expect(test.state.cancelAllError).toBe('Cancel all did not drain cleanly: drain failed');
  });

  it('retries only retryable items and clears their display progress first', async () => {
    const test = setup();
    test.invokeMock.mockResolvedValue([]);
    await test.controller.retryItem(item({ id: 'bad', status: 'error' }));
    await test.controller.retryItem(item({ id: 'ready', status: 'ready' }));
    expect(test.clearProgressDisplayState).toHaveBeenCalledWith('bad');
    expect(test.invokeMock).toHaveBeenCalledOnce();
    expect(test.invokeMock).toHaveBeenCalledWith('enqueue_queue_items', {
      itemIds: ['bad'],
      priority: 'front'
    });
  });

  it('removes only selected inactive rows and closes their active title editor', async () => {
    const test = setup([
      item({ id: 'ready', selected: true }),
      item({ id: 'active', selected: true, status: 'downloading' }),
      item({ id: 'other', selected: false })
    ]);
    test.setEditingTitleId('ready');
    test.invokeMock.mockResolvedValue(undefined);
    await test.controller.removeSelected();
    expect(test.invokeMock).toHaveBeenCalledWith('remove_queue_items', { itemIds: ['ready'] });
    expect(test.cancelFilenameEdit).toHaveBeenCalledOnce();
  });

  it('clears completed and cancelled rows with the original payload ordering', async () => {
    const test = setup([
      item({ id: 'done', status: 'completed' }),
      item({ id: 'failed', status: 'error' }),
      item({ id: 'cancelled', status: 'cancelled' })
    ]);
    test.invokeMock.mockResolvedValue(undefined);
    await test.controller.clearCompleted();
    expect(test.invokeMock).toHaveBeenCalledWith('remove_queue_items', {
      itemIds: ['done', 'cancelled']
    });
  });

  it('reloads the snapshot after a settings failure and suppresses reload errors', async () => {
    const test = setup();
    test.invokeMock.mockRejectedValue({ safeSummary: 'save failed' });
    test.reloadAppSnapshot.mockRejectedValue(new Error('reload failed'));
    await expect(
      test.controller.updateQueueItemSettings(item(), { outputDir: 'C:\\Downloads' })
    ).resolves.toBeUndefined();
    expect(test.state.queueActionError).toBe('save failed');
    expect(test.reloadAppSnapshot).toHaveBeenCalledOnce();
  });

  it('applies dynamic global settings concurrently to editable rows', async () => {
    const test = setup([
      item({ id: 'video', hasAudio: true, availableQualities: ['best', '1080p'] }),
      item({ id: 'silent', hasAudio: false, availableQualities: ['best'] }),
      item({ id: 'done', status: 'completed' })
    ]);
    test.invokeMock.mockResolvedValue(undefined);
    await test.controller.applyGlobalQuality();
    test.setGlobalFormat('mp3');
    await test.controller.applyGlobalFormat();
    expect(test.invokeMock.mock.calls).toEqual([
      ['update_queue_item', { itemId: 'video', input: { quality: '1080p' } }],
      ['update_queue_item', { itemId: 'silent', input: { quality: 'best' } }],
      ['update_queue_item', { itemId: 'video', input: { format: 'mp3' } }],
      ['update_queue_item', { itemId: 'silent', input: { format: 'mp4' } }]
    ]);
  });
});

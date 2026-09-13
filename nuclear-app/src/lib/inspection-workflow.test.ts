import { describe, expect, it, vi } from 'vitest';
import type { OperationSnapshot } from './bindings/OperationSnapshot';
import type { UrlInspection } from './bindings/UrlInspection';
import type { WorkflowCommands } from './frontend-workflow-ports';
import {
  createInspectionState,
  InspectionWorkflow,
  type InspectionSettings,
  type InspectionWorkflowDependencies
} from './inspection-workflow';

const video = (url = 'https://example.com/video') => ({
  id: 'video-1',
  title: 'Video',
  duration: 42,
  channel: 'Channel',
  thumbnail: 'thumb',
  url,
  available_qualities: ['1080p'],
  has_audio: true
});

const operation = (inspectionResult: UrlInspection, id = 'operation-1'): OperationSnapshot => ({
  schemaVersion: 1,
  id,
  queueItemId: null,
  kind: 'inspection',
  state: 'completed',
  progress: 100,
  phase: null,
  sequence: 1,
  createdAtMs: 1,
  updatedAtMs: 2,
  finishedAtMs: 2,
  error: null,
  inspectionResult,
  publishedOutput: null,
  intendedTerminalOutcome: null,
  correlationId: 'correlation'
});

type SetupOverrides = Omit<Partial<InspectionWorkflowDependencies>, 'queue'> & {
  queue?: Partial<InspectionWorkflowDependencies['queue']>;
};

function setup(overrides: SetupOverrides = {}) {
  const state = createInspectionState();
  const settings: InspectionSettings = {
    canStartDownloads: true,
    startupState: 'ready',
    runtimeCanDownload: true,
    outputDirError: null,
    globalFormat: 'mp4',
    globalQuality: 'best',
    outputDir: 'C:\\Downloads'
  };
  let active = true;
  const invoke = vi.fn(async (command: string, _arguments?: unknown): Promise<unknown> => {
    if (command === 'begin_inspection') return { operationId: 'operation-1' };
    if (command === 'add_inspection_result_to_queue') return { id: 'queue-1' };
    return undefined;
  });
  const waitForOperation = vi.fn(
    async (_operationId: string, _timeoutMs?: number): Promise<OperationSnapshot> =>
      operation({ kind: 'video', video: video() })
  );
  const { queue: queueOverrides, ...dependencyOverrides } = overrides;
  const dependencies: InspectionWorkflowDependencies = {
    getSettings: () => settings,
    readCookie: () => null,
    readCompat: () => null,
    queue: {
      getItems: () => [],
      retainMetadata: vi.fn(() => () => undefined),
      retainPlaylistMetadata: vi.fn(() => ({ confirm: vi.fn(), discard: vi.fn() })),
      ...queueOverrides
    },
    setQueueActionError: vi.fn(),
    invoke: invoke as unknown as WorkflowCommands['invoke'],
    waitForOperation,
    isActive: () => active,
    unloadedError: new Error('Renderer was unloaded before the operation completed.'),
    ...dependencyOverrides
  };
  return {
    workflow: new InspectionWorkflow(state, dependencies),
    state,
    settings,
    invoke,
    waitForOperation,
    dependencies,
    dispose: () => {
      active = false;
    }
  };
}

describe('InspectionWorkflow', () => {
  it('captures cookies before inspection but reads mutable admission settings afterward', async () => {
    const cookie = {
      enabled: true,
      mode: 'browser' as const,
      browser: 'firefox' as const,
      cookie_file: null
    };
    const context = setup({ readCookie: vi.fn(() => cookie) });
    context.state.urlInput = ' https://example.com/video ';
    context.waitForOperation.mockImplementationOnce(async (_operationId, _timeoutMs) => {
      context.settings.globalFormat = 'mkv';
      context.settings.globalQuality = '1080p';
      context.settings.outputDir = 'D:\\Later';
      return operation({ kind: 'video', video: video() });
    });

    await context.workflow.addToQueue();

    expect(context.invoke.mock.calls.map(([command]) => command)).toEqual([
      'begin_inspection',
      'add_inspection_result_to_queue'
    ]);
    expect(context.invoke.mock.calls[0][1]).toEqual({
      input: { url: 'https://example.com/video', cookieConfig: cookie, compatConfigPath: null }
    });
    expect(context.invoke.mock.calls[1][1]).toMatchObject({
      input: { format: 'mkv', quality: '1080p', outputDir: 'D:\\Later', cookieConfig: cookie }
    });
    expect(context.dependencies.queue.retainMetadata).toHaveBeenCalledWith(
      'https://example.com/video',
      { duration: 42, channel: 'Channel', thumbnail: 'thumb' },
      undefined
    );
    expect(context.state.urlInput).toBe('');
    expect(context.state.playlistLoading).toBe(false);
  });

  it.each([
    ['not a url', 'Please enter a valid URL (must start with http:// or https://)'],
    ['file:///tmp/video', 'Please enter a valid URL (must start with http:// or https://)']
  ])('rejects invalid input %s before backend admission', async (input, message) => {
    const context = setup();
    context.state.urlInput = input;
    await context.workflow.addToQueue();
    expect(context.state.urlError).toBe(message);
    expect(context.invoke).not.toHaveBeenCalled();
  });

  it('rejects duplicate URLs and preserves the readiness error priority', async () => {
    const context = setup({
      queue: {
        getItems: () => [{ url: 'https://example.com/video' }] as never,
        retainMetadata: vi.fn(() => () => undefined)
      }
    });
    context.state.urlInput = 'https://example.com/video';
    await context.workflow.addToQueue();
    expect(context.state.urlError).toBe('URL already in queue');

    context.settings.canStartDownloads = false;
    context.settings.startupState = 'error';
    await context.workflow.addToQueue();
    expect(context.state.urlError).toBe(
      'Work is disabled because renderer event delivery could not be initialized. Restart the app.'
    );
  });

  it('allows reopening a parent URL when only a selected child identity is queued', async () => {
    const url = 'https://social.example/parent';
    const context = setup({
      queue: {
        getItems: () =>
          [
            {
              url,
              selection: { entryId: 'media-one', extractorKey: 'twitter', playlistIndex: 1 }
            }
          ] as never,
        retainMetadata: vi.fn(() => () => undefined)
      }
    });
    context.state.urlInput = url;

    await context.workflow.addToQueue();

    expect(context.invoke).toHaveBeenCalledWith('begin_inspection', {
      input: { url, cookieConfig: null, compatConfigPath: null }
    });
    expect(context.state.urlError).toBe('');
  });

  it('opens a selected playlist and exposes bounded 100-entry pages', async () => {
    const entries = Array.from({ length: 101 }, (_, index) => ({
      id: String(index),
      title: `Entry ${index}`,
      duration: null,
      url: `https://example.com/${index}`,
      thumbnail: null
    }));
    const context = setup({
      waitForOperation: vi.fn(async () =>
        operation({
          kind: 'playlist',
          playlist: { title: 'List', channel: null, entry_count: 101, truncated: false, entries }
        })
      )
    });
    context.state.urlInput = 'https://example.com/list';

    await context.workflow.addToQueue();
    expect(context.state.playlistModal?.entries.every((entry) => entry.selected)).toBe(true);
    expect(context.workflow.pageCount()).toBe(2);
    expect(context.workflow.visibleEntries()).toHaveLength(100);
    context.workflow.changePage(5);
    expect(context.state.playlistPage).toBe(1);
    expect(context.workflow.visibleEntries()).toHaveLength(1);
    context.workflow.toggleAll(false);
    expect(context.state.playlistModal?.entries.every((entry) => !entry.selected)).toBe(true);
  });

  it('dismisses a completed inspection when cancellation wins the continuation', async () => {
    const context = setup();
    context.state.urlInput = 'https://example.com/video';
    context.waitForOperation.mockImplementationOnce(async (_operationId, _timeoutMs) => {
      context.state.cancelRequested = true;
      return operation({ kind: 'video', video: video() });
    });
    await context.workflow.addToQueue();
    expect(context.invoke.mock.calls.map(([command]) => command)).toEqual([
      'begin_inspection',
      'dismiss_operation'
    ]);
  });

  it('dismisses an admission token after failure and reports the original normalized error', async () => {
    const discardMetadata = vi.fn();
    const context = setup({
      queue: {
        getItems: () => [],
        retainMetadata: vi.fn(() => discardMetadata)
      }
    });
    context.state.urlInput = 'https://example.com/video';
    context.invoke.mockImplementation(async (command: string, _arguments?: unknown) => {
      if (command === 'begin_inspection') return { operationId: 'operation-1' };
      if (command === 'add_inspection_result_to_queue') throw new Error('admission failed');
      return undefined;
    });
    await context.workflow.addToQueue();
    expect(context.invoke.mock.calls.map(([command]) => command)).toEqual([
      'begin_inspection',
      'add_inspection_result_to_queue',
      'dismiss_operation'
    ]);
    expect(context.state.urlError).toBe('Failed to inspect URL: admission failed');
    expect(discardMetadata).toHaveBeenCalledOnce();
  });

  it('discards failed admission metadata after renderer disposal', async () => {
    const discardMetadata = vi.fn();
    const context = setup({
      queue: { getItems: () => [], retainMetadata: vi.fn(() => discardMetadata) }
    });
    context.state.urlInput = 'https://example.com/video';
    context.invoke.mockImplementation(async (command: string) => {
      if (command === 'begin_inspection') return { operationId: 'operation-1' };
      if (command === 'add_inspection_result_to_queue') {
        context.dispose();
        throw new Error('disposed admission failed');
      }
      return undefined;
    });

    await context.workflow.addToQueue();
    expect(discardMetadata).toHaveBeenCalledOnce();
    expect(context.state.urlError).toBe('');
  });

  it('keeps degraded startup usable and surfaces an interrupted inspection error', async () => {
    const context = setup();
    context.settings.startupState = 'degraded';
    context.state.urlInput = 'https://example.com/video';
    context.waitForOperation.mockResolvedValueOnce({
      ...operation({ kind: 'video', video: video() }),
      state: 'interrupted',
      inspectionResult: null,
      error: {
        code: 'interrupted',
        summary: 'Inspection was interrupted.',
        detail: null,
        retryable: true,
        correlationId: 'correlation'
      }
    });

    await context.workflow.addToQueue();

    expect(context.invoke).toHaveBeenCalledWith('begin_inspection', expect.anything());
    expect(context.state.urlError).toBe(
      'Failed to inspect URL: Inspection was interrupted. (interrupted)'
    );
  });

  it('admits original selected indices in one batch and leaves duplicate handling to backend', async () => {
    const context = setup();
    const entries = Array.from({ length: 3 }, (_, index) => ({
      id: String(index),
      title: `Entry ${index}`,
      duration: null,
      url: index === 2 ? 'https://example.com/0' : `https://example.com/${index}`,
      thumbnail: null,
      ...(index === 2
        ? { video: { ...video('https://example.com/0'), duration: 84, channel: 'Full entry' } }
        : {}),
      selected: index !== 1
    }));
    context.state.playlistModal = {
      info: { title: 'List', channel: null, entry_count: 3, truncated: false, entries },
      inspectionOperationId: 'playlist-operation',
      url: 'https://example.com/list',
      cookieConfig: null,
      entries
    };
    let resolveAdmission!: (value: unknown) => void;
    context.invoke.mockImplementation((command: string) =>
      command === 'add_inspection_result_to_queue'
        ? new Promise((resolve) => (resolveAdmission = resolve))
        : Promise.resolve(undefined)
    );

    const adding = context.workflow.addPlaylistSelection();
    await vi.waitFor(() => expect(resolveAdmission).toBeTypeOf('function'));
    expect(context.state.playlistModal).not.toBeNull();
    context.workflow.closePlaylist();
    expect(context.state.playlistModal).not.toBeNull();
    const input = (context.invoke.mock.calls[0][1] as { input: { playlist: unknown } }).input;
    expect(input).toMatchObject({
      inspectionOperationId: 'playlist-operation',
      playlist: { entryIndices: [0, 2] }
    });
    expect((input.playlist as { requestId: string }).requestId).toMatch(/^[0-9a-f-]{36}$/i);
    resolveAdmission({
      kind: 'playlist',
      requestId: (input.playlist as { requestId: string }).requestId,
      itemIds: ['one', 'three'],
      skippedCount: 1
    });
    await adding;
    expect(context.state.playlistModal).toBeNull();
    expect(context.invoke).toHaveBeenCalledTimes(1);
    expect(context.dependencies.queue.retainPlaylistMetadata).toHaveBeenCalledWith([
      {
        identity: '["https://example.com/0"]',
        metadata: { duration: null, channel: null, thumbnail: null }
      },
      {
        identity: '["https://example.com/0"]',
        metadata: { duration: 84, channel: 'Full entry', thumbnail: 'thumb' }
      }
    ]);
    const lease = vi.mocked(context.dependencies.queue.retainPlaylistMetadata).mock.results[0]
      .value;
    expect(lease.confirm).toHaveBeenCalledWith(['one', 'three']);
    expect(context.waitForOperation).not.toHaveBeenCalled();
  });

  it('retries a failed playlist admission with the same request id and captured settings', async () => {
    const firstLease = { confirm: vi.fn(), discard: vi.fn() };
    const secondLease = { confirm: vi.fn(), discard: vi.fn() };
    const retainPlaylistMetadata = vi
      .fn()
      .mockReturnValueOnce(firstLease)
      .mockReturnValueOnce(secondLease);
    const context = setup({
      queue: { retainPlaylistMetadata }
    });
    const entries = [
      {
        id: '0',
        title: 'Entry',
        duration: null,
        url: 'https://example.com/0',
        thumbnail: null,
        selected: true
      }
    ];
    context.state.playlistModal = {
      info: { title: 'List', channel: null, entry_count: 1, truncated: false, entries },
      inspectionOperationId: 'playlist-operation',
      url: 'https://example.com/list',
      cookieConfig: null,
      entries
    };
    context.invoke
      .mockRejectedValueOnce(new Error('response lost'))
      .mockImplementationOnce(async (_command, args) => {
        const requestId = (args as { input: { playlist: { requestId: string } } }).input.playlist
          .requestId;
        return { kind: 'playlist', requestId, itemIds: ['one'], skippedCount: 0 };
      });

    await context.workflow.addPlaylistSelection();
    expect(context.state.playlistModal).not.toBeNull();
    expect(context.state.urlError).toBe('Some playlist entries could not be added. response lost');
    expect(firstLease.discard).toHaveBeenCalledOnce();
    context.settings.globalFormat = 'mp3';
    context.settings.outputDir = 'D:\\changed';
    await context.workflow.addPlaylistSelection();

    const inputs = context.invoke.mock.calls.map(
      ([, args]) => (args as { input: Record<string, unknown> }).input
    );
    expect(inputs[1]).toEqual(inputs[0]);
    expect(inputs[0]).toMatchObject({ format: 'mp4', outputDir: 'C:\\Downloads' });
    expect(context.state.urlError).toBe('');
    expect(retainPlaylistMetadata).toHaveBeenCalledTimes(2);
    expect(secondLease.discard).not.toHaveBeenCalled();
    expect(secondLease.confirm).toHaveBeenCalledWith(['one']);
  });

  it('does not publish a late playlist admission completion after renderer disposal', async () => {
    const lease = { confirm: vi.fn(), discard: vi.fn() };
    const context = setup({ queue: { retainPlaylistMetadata: vi.fn(() => lease) } });
    const entries = [
      {
        id: '0',
        title: 'Entry',
        duration: null,
        url: 'https://example.com/0',
        thumbnail: null,
        selected: true
      }
    ];
    context.state.playlistModal = {
      info: { title: 'List', channel: null, entry_count: 1, truncated: false, entries },
      inspectionOperationId: 'playlist-operation',
      url: 'https://example.com/list',
      cookieConfig: null,
      entries
    };
    let resolveAdmission!: (value: unknown) => void;
    context.invoke.mockImplementation(() => new Promise((resolve) => (resolveAdmission = resolve)));
    const adding = context.workflow.addPlaylistSelection();
    await vi.waitFor(() => expect(resolveAdmission).toBeTypeOf('function'));
    const requestId = (
      context.invoke.mock.calls[0][1] as { input: { playlist: { requestId: string } } }
    ).input.playlist.requestId;
    context.dispose();
    resolveAdmission({ kind: 'playlist', requestId, itemIds: ['one'], skippedCount: 0 });
    await adding;
    expect(context.state.playlistModal).not.toBeNull();
    expect(context.state.playlistLoading).toBe(true);
    expect(context.state.urlError).toBe('');
    expect(lease.confirm).not.toHaveBeenCalled();
    expect(lease.discard).toHaveBeenCalledOnce();
  });

  it('suppresses continuations and backend cleanup after disposal', async () => {
    let resolveBegin!: (value: { operationId: string }) => void;
    const context = setup();
    context.state.urlInput = 'https://example.com/video';
    context.invoke.mockImplementation((command: string, _arguments?: unknown) =>
      command === 'begin_inspection'
        ? new Promise((resolve) => {
            resolveBegin = resolve;
          })
        : Promise.resolve(undefined)
    );
    const adding = context.workflow.addToQueue();
    await vi.waitFor(() => expect(resolveBegin).toBeTypeOf('function'));
    context.dispose();
    resolveBegin({ operationId: 'late' });
    await adding;
    expect(context.waitForOperation).not.toHaveBeenCalled();
    expect(context.invoke).toHaveBeenCalledTimes(1);
    expect(context.state.playlistLoading).toBe(true);
    expect(context.state.urlError).toBe('');
  });

  it('cancels the active operation and does not publish a late cancellation error', async () => {
    const context = setup();
    context.state.activeInspectionId = 'operation-1';
    let rejectCancel!: (error: Error) => void;
    context.invoke.mockImplementation(
      (_command: string, _arguments?: unknown) =>
        new Promise((_, reject) => (rejectCancel = reject))
    );
    const cancelling = context.workflow.cancelInspection();
    await vi.waitFor(() => expect(rejectCancel).toBeTypeOf('function'));
    context.dispose();
    rejectCancel(new Error('late failure'));
    await cancelling;
    expect(context.state.cancelRequested).toBe(true);
    expect(context.state.urlError).toBe('');
  });
});

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

function setup(overrides: Partial<InspectionWorkflowDependencies> = {}) {
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
  const dependencies: InspectionWorkflowDependencies = {
    getSettings: () => settings,
    readCookie: () => null,
    readCompat: () => null,
    queue: { getItems: () => [], retainMetadata: vi.fn() },
    setQueueActionError: vi.fn(),
    invoke: invoke as unknown as WorkflowCommands['invoke'],
    waitForOperation,
    isActive: () => active,
    unloadedError: new Error('Renderer was unloaded before the operation completed.'),
    ...overrides
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
      { duration: 42, channel: 'Channel', thumbnail: 'thumb' }
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
        retainMetadata: vi.fn()
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
    const context = setup();
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

  it('queues playlist entries sequentially, skips duplicates, and summarizes three failures', async () => {
    const context = setup();
    const entries = Array.from({ length: 5 }, (_, index) => ({
      id: String(index),
      title: `Entry ${index}`,
      duration: null,
      url: `https://example.com/${index}`,
      thumbnail: null,
      selected: true
    }));
    context.state.playlistModal = {
      info: { title: 'List', channel: null, entry_count: 5, truncated: false, entries },
      inspectionOperationId: 'playlist-operation',
      url: 'https://example.com/list',
      cookieConfig: null,
      entries
    };
    context.waitForOperation.mockImplementation(async (id: string, _timeoutMs?: number) => {
      const index = Number(id.split('-').at(-1));
      if (index < 4) throw new Error(`failure ${index}`);
      return operation({ kind: 'video', video: video(`https://example.com/${index}`) }, id);
    });
    let next = 0;
    context.invoke.mockImplementation(async (command: string, _arguments?: unknown) => {
      if (command === 'begin_inspection') return { operationId: `operation-${next++}` };
      if (command === 'add_inspection_result_to_queue') return { id: 'queue' };
      return undefined;
    });

    await context.workflow.addPlaylistSelection();
    expect(context.state.urlError).toBe(
      'Some playlist entries could not be added. Entry 0: failure 0 Entry 1: failure 1 Entry 2: failure 2'
    );
    expect(
      context.invoke.mock.calls.filter(([command]) => command === 'begin_inspection')
    ).toHaveLength(5);
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

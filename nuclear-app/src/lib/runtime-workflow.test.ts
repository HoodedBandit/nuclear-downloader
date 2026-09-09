import { describe, expect, it, vi } from 'vitest';
import type { invokeCommand } from './ipc-client';
import {
  RuntimeWorkflowController,
  compactRuntimeToolVersion,
  createRuntimeWorkflowState,
  type RuntimeWorkflowDependencies
} from './runtime-workflow';

function dependencies(overrides: Partial<RuntimeWorkflowDependencies> = {}) {
  return {
    invoke: vi.fn() as unknown as typeof invokeCommand,
    waitForOperation: vi.fn(),
    isActive: () => true,
    unloadedError: new Error('unloaded'),
    appUpdateRunning: () => false,
    hasUpdateBlockingWork: () => false,
    backendReadiness: () => 'ready' as const,
    setStartupSubsystem: vi.fn(),
    reportStartupIssue: vi.fn(),
    ...overrides
  } satisfies RuntimeWorkflowDependencies;
}

const runtimeStatus = {
  state: 'ready' as const,
  runtimeVersion: '1',
  source: 'bundled',
  updateAvailable: false,
  latestRuntimeVersion: null,
  runtimeDir: null,
  pluginDir: 'plugins',
  message: null,
  tools: [
    {
      name: 'yt-dlp',
      required: true,
      available: true,
      version: '2026.1',
      path: 'yt-dlp',
      source: 'bundled',
      error: null
    }
  ]
};

describe('RuntimeWorkflowController', () => {
  it('refreshes runtime state and startup readiness', async () => {
    const deps = dependencies();
    vi.mocked(deps.invoke).mockResolvedValueOnce(runtimeStatus);
    const state = createRuntimeWorkflowState();
    await new RuntimeWorkflowController(state, deps).refresh();
    expect(state).toMatchObject({ status: runtimeStatus, checkState: 'idle', error: null });
    expect(deps.setStartupSubsystem).toHaveBeenCalledWith('ready');
  });

  it('keeps local runtime usable when the optional update check fails', async () => {
    const deps = dependencies();
    vi.mocked(deps.invoke).mockRejectedValueOnce(new Error('offline'));
    const state = createRuntimeWorkflowState();
    state.updateCheck = { updateAvailable: true, latestRuntimeVersion: 'old', message: null };
    await new RuntimeWorkflowController(state, deps).checkForUpdate();
    expect(state.updateCheck).toBeNull();
    expect(state.error).toBeNull();
  });

  it('reports initialization failure and starts the optional check without awaiting it', async () => {
    const updateCheck = Promise.resolve({
      updateAvailable: false,
      latestRuntimeVersion: null,
      message: null
    });
    const deps = dependencies();
    vi.mocked(deps.invoke)
      .mockRejectedValueOnce(new Error('missing'))
      .mockReturnValueOnce(updateCheck as never);
    const controller = new RuntimeWorkflowController(createRuntimeWorkflowState(), deps);
    await controller.initialize();
    expect(deps.reportStartupIssue).toHaveBeenCalledWith('Downloader runtime', 'missing');
    expect(deps.invoke).toHaveBeenNthCalledWith(2, 'check_runtime_update');
  });

  it('preserves blockers without beginning an update', async () => {
    const appBlocked = dependencies({ appUpdateRunning: () => true });
    const first = new RuntimeWorkflowController(createRuntimeWorkflowState(), appBlocked);
    await first.update();
    expect(first.state.error).toBe('Wait for the app update operation to finish.');
    expect(appBlocked.invoke).not.toHaveBeenCalled();

    const queueBlocked = dependencies({ hasUpdateBlockingWork: () => true });
    const second = new RuntimeWorkflowController(createRuntimeWorkflowState(), queueBlocked);
    await second.update();
    expect(second.state.error).toBe(
      'Finish or cancel queued downloads before updating the runtime.'
    );
  });

  it('waits for update then refreshes runtime before checking updates', async () => {
    const order: string[] = [];
    const deps = dependencies({
      invoke: (async (command: string) => {
        order.push(command);
        if (command === 'begin_runtime_update') return { operationId: 'op' };
        if (command === 'check_downloader_runtime') return runtimeStatus;
        return { updateAvailable: false, latestRuntimeVersion: null, message: null };
      }) as typeof invokeCommand,
      waitForOperation: async () => {
        order.push('wait');
        return { state: 'completed' } as never;
      }
    });
    const state = createRuntimeWorkflowState();
    await new RuntimeWorkflowController(state, deps).update();
    expect(order).toEqual([
      'begin_runtime_update',
      'wait',
      'check_downloader_runtime',
      'check_runtime_update'
    ]);
    expect(state.updateRunning).toBe(false);
  });

  it('owns progress errors and runtime presentation selectors', () => {
    const controller = new RuntimeWorkflowController(createRuntimeWorkflowState(), dependencies());
    controller.state.updateRunning = true;
    controller.applyProgress({
      status: 'error',
      version: null,
      downloadedBytes: 2,
      totalBytes: 4,
      message: null
    });
    expect(controller.state).toMatchObject({
      updateRunning: false,
      error: 'Downloader runtime update failed.'
    });
    expect(controller.getUpdatePercent()).toBe(50);
    expect(
      compactRuntimeToolVersion({
        ...runtimeStatus.tools[0],
        name: 'ffmpeg',
        version: 'ffmpeg version 7.1 build'
      })
    ).toBe('7.1');
  });
});

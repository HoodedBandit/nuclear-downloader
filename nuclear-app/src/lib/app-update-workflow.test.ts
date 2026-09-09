import { describe, expect, it, vi } from 'vitest';
import type { invokeCommand } from './ipc-client';
import {
  AppUpdateWorkflowController,
  createAppUpdateWorkflowState,
  type AppUpdateWorkflowDependencies
} from './app-update-workflow';

function dependencies(overrides: Partial<AppUpdateWorkflowDependencies> = {}) {
  return {
    invoke: vi.fn() as unknown as typeof invokeCommand,
    waitForOperation: vi.fn(),
    isActive: () => true,
    unloadedError: new Error('unloaded'),
    getVersion: vi.fn(async () => '0.6.0'),
    runtimeUpdateRunning: () => false,
    hasUpdateBlockingWork: () => false,
    setAppVersionStartup: vi.fn(),
    setUpdateCheckStartup: vi.fn(),
    reportStartupIssue: vi.fn(),
    appendStartupIssue: vi.fn(),
    ...overrides
  } satisfies AppUpdateWorkflowDependencies;
}

const updateInfo = {
  currentVersion: '0.6.0',
  hasUpdate: true,
  latestVersion: '0.7.0',
  notes: null,
  publishedAt: null,
  installerName: null
};

describe('AppUpdateWorkflowController', () => {
  it('initializes app version and degrades startup on failure', async () => {
    const good = dependencies();
    const goodController = new AppUpdateWorkflowController(createAppUpdateWorkflowState(), good);
    await goodController.initializeAppVersion();
    expect(goodController.state.appVersion).toBe('0.6.0');
    expect(good.setAppVersionStartup).toHaveBeenCalledWith('ready');

    const bad = dependencies({
      getVersion: vi.fn(async () => {
        throw new Error('version failed');
      })
    });
    await new AppUpdateWorkflowController(
      createAppUpdateWorkflowState(),
      bad
    ).initializeAppVersion();
    expect(bad.reportStartupIssue).toHaveBeenCalledWith('App version', expect.any(Error));
    expect(bad.setAppVersionStartup).toHaveBeenCalledWith('degraded');
  });

  it('checks updates with exact modal and state transitions', async () => {
    const deps = dependencies();
    vi.mocked(deps.invoke).mockResolvedValueOnce(updateInfo);
    const state = createAppUpdateWorkflowState();
    state.installProgress = {
      status: 'downloading',
      version: 'old',
      downloadedBytes: 1,
      totalBytes: 2,
      message: null
    };
    const result = await new AppUpdateWorkflowController(state, deps).check({
      openModal: true,
      showErrors: true
    });
    expect(result).toBe(true);
    expect(state).toMatchObject({
      modalOpen: true,
      checkState: 'idle',
      info: updateInfo,
      appVersion: '0.6.0',
      installProgress: null
    });
  });

  it('records the exact degraded startup issue when an initial check fails', async () => {
    const deps = dependencies();
    vi.mocked(deps.invoke).mockRejectedValueOnce(new Error('offline'));
    await new AppUpdateWorkflowController(
      createAppUpdateWorkflowState(),
      deps
    ).initializeUpdateCheck();
    expect(deps.appendStartupIssue).toHaveBeenCalledWith(
      'App update check was unavailable; downloads remain usable.'
    );
    expect(deps.setUpdateCheckStartup).toHaveBeenCalledWith('degraded');
  });

  it('preserves runtime and queue blockers', async () => {
    const runtimeBlocked = dependencies({ runtimeUpdateRunning: () => true });
    const first = new AppUpdateWorkflowController(
      { ...createAppUpdateWorkflowState(), info: updateInfo },
      runtimeBlocked
    );
    await first.install();
    expect(first.state).toMatchObject({
      modalOpen: true,
      error: 'Wait for the downloader runtime update to finish.'
    });

    const queueBlocked = dependencies({ hasUpdateBlockingWork: () => true });
    const second = new AppUpdateWorkflowController(
      { ...createAppUpdateWorkflowState(), info: updateInfo },
      queueBlocked
    );
    await second.install();
    expect(second.state.error).toBe(
      'Finish or cancel queued downloads before installing the app update.'
    );
  });

  it('sends expectedVersion, waits, and clears running in finally', async () => {
    const deps = dependencies();
    vi.mocked(deps.invoke).mockResolvedValueOnce({ operationId: 'update-op' });
    vi.mocked(deps.waitForOperation).mockResolvedValueOnce({ state: 'completed' } as never);
    const controller = new AppUpdateWorkflowController(
      { ...createAppUpdateWorkflowState(), info: updateInfo },
      deps
    );
    await controller.install();
    expect(deps.invoke).toHaveBeenCalledWith('begin_app_update', { expectedVersion: '0.7.0' });
    expect(deps.waitForOperation).toHaveBeenCalledWith('update-op');
    expect(controller.state.installRunning).toBe(false);
  });

  it('owns locked modal and progress error transitions', () => {
    const controller = new AppUpdateWorkflowController(
      createAppUpdateWorkflowState(),
      dependencies()
    );
    controller.openModal();
    controller.state.installRunning = true;
    controller.closeModal();
    expect(controller.state.modalOpen).toBe(true);
    controller.applyProgress({
      status: 'error',
      version: '0.7.0',
      downloadedBytes: 5,
      totalBytes: 10,
      message: null
    });
    expect(controller.state).toMatchObject({
      installRunning: false,
      error: 'Update installation failed.'
    });
    expect(controller.downloadPercent()).toBe(50);
  });
});

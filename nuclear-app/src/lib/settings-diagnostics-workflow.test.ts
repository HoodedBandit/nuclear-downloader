import { describe, expect, it, vi } from 'vitest';
import type { DownloaderRuntimeStatus } from './bindings/DownloaderRuntimeStatus';
import type { WorkflowCommands } from './frontend-workflow-ports';
import type { QueueItem } from './frontend-types';
import {
  createSettingsDiagnosticsState,
  getPathBasename,
  pickFirstPath,
  SettingsDiagnosticsWorkflow,
  type SettingsDiagnosticsWorkflowOptions
} from './settings-diagnostics-workflow';

function queueItem(overrides: Partial<QueueItem> = {}): QueueItem {
  return {
    id: 'item-1',
    downloadId: null,
    url: 'https://example.test/video',
    title: 'Video',
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
    availableQualities: ['best'],
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

function setup(overrides: Partial<SettingsDiagnosticsWorkflowOptions> = {}) {
  const state = createSettingsDiagnosticsState();
  const invoke = vi.fn();
  const open = vi.fn();
  const save = vi.fn();
  const updateQueueItemSettings = vi.fn().mockResolvedValue(undefined);
  const setSubsystem = vi.fn();
  const reportIssue = vi.fn();
  const writeText = vi.fn().mockResolvedValue(undefined);
  const options: SettingsDiagnosticsWorkflowOptions = {
    commands: {
      invoke: invoke as unknown as WorkflowCommands['invoke'],
      isActive: () => true,
      unloadedError: new Error('unloaded')
    },
    ui: {
      dialogs: { open, save },
      confirm: () => true,
      clipboard: { writeText }
    },
    queue: { getItems: () => [], updateQueueItemSettings },
    startup: { setSubsystem, reportIssue },
    getRuntimeStatus: () => null,
    getQueueItemDisplayTitle: (item) => item.customFilename ?? item.title,
    ...overrides
  };
  return {
    workflow: new SettingsDiagnosticsWorkflow(state, options),
    state,
    invoke,
    open,
    save,
    updateQueueItemSettings,
    setSubsystem,
    reportIssue,
    writeText
  };
}

describe('settings diagnostics workflow', () => {
  it('creates the exact settings defaults and snapshots cookies and config at read time', () => {
    const { workflow, state } = setup();
    expect(state).toEqual({
      outputDir: '',
      outputDirValidated: false,
      outputDirError: null,
      globalQuality: 'best',
      globalFormat: 'mp4',
      useCookies: false,
      cookieMode: 'browser',
      cookieBrowser: 'firefox',
      cookieFilePath: '',
      compatConfigPath: '',
      diagnosticsMessage: null,
      diagnosticsError: null
    });
    expect(workflow.getCookieConfigSnapshot()).toBeNull();
    state.useCookies = true;
    state.cookieMode = 'file';
    state.cookieFilePath = 'cookies.txt';
    state.compatConfigPath = '  yt-dlp.conf  ';
    const snapshot = workflow.getCookieConfigSnapshot();
    expect(snapshot).toEqual({
      enabled: true,
      mode: 'file',
      browser: 'firefox',
      cookie_file: 'cookies.txt'
    });
    state.cookieFilePath = 'changed.txt';
    expect(snapshot?.cookie_file).toBe('cookies.txt');
    expect(workflow.getCompatConfigSnapshot()).toBe('yt-dlp.conf');
  });

  it('validates and initializes the output directory with the original startup callbacks', async () => {
    const { workflow, state, invoke, setSubsystem } = setup();
    invoke.mockResolvedValueOnce('C:\\Downloads').mockResolvedValueOnce('C:\\Downloads');
    await workflow.initializeOutputDirectory();
    expect(invoke.mock.calls).toEqual([
      ['default_download_dir'],
      ['validate_output_directory', { path: 'C:\\Downloads' }]
    ]);
    expect(state.outputDir).toBe('C:\\Downloads');
    expect(state.outputDirValidated).toBe(true);
    expect(setSubsystem).toHaveBeenCalledWith('outputDirectory', 'ready');
  });

  it('reports discovery and validation failures with the existing messages', async () => {
    const first = setup();
    first.invoke.mockRejectedValueOnce(new Error('discovery failed'));
    await first.workflow.initializeOutputDirectory();
    expect(first.state.outputDirError).toBe('Choose a writable output folder before downloading.');
    expect(first.reportIssue).toHaveBeenCalledWith('Output folder discovery', expect.any(Error));
    expect(first.setSubsystem).toHaveBeenCalledWith('outputDirectory', 'error');

    const second = setup();
    expect(await second.workflow.validateOutputDirectory('  ')).toBe(false);
    expect(second.state.outputDirError).toBe('Choose a writable output folder before downloading.');
    expect(second.invoke).not.toHaveBeenCalled();
  });

  it('uses the exact dialog options and fans a validated output folder to ready items', async () => {
    const ready = queueItem();
    const active = queueItem({ id: 'item-2', status: 'downloading' });
    const update = vi.fn().mockResolvedValue(undefined);
    const configured = setup({
      queue: {
        getItems: () => [ready, active],
        updateQueueItemSettings: update
      }
    });
    configured.open.mockResolvedValueOnce(['D:\\Media']);
    configured.invoke.mockResolvedValueOnce('D:\\Media');
    await configured.workflow.browseOutputDir();
    expect(configured.open).toHaveBeenCalledWith({ directory: true });
    expect(update).toHaveBeenCalledOnce();
    expect(update).toHaveBeenCalledWith(ready, { outputDir: 'D:\\Media' });
  });

  it('browses cookie and compatibility config files with unchanged filters', async () => {
    const { workflow, state, open } = setup();
    open.mockResolvedValueOnce('cookies.txt').mockResolvedValueOnce(['yt-dlp.conf']);
    await workflow.browseCookieFile();
    await workflow.browseCompatConfigFile();
    expect(open.mock.calls).toEqual([
      [{ filters: [{ name: 'Cookie Files', extensions: ['txt'] }] }],
      [
        {
          multiple: false,
          filters: [{ name: 'yt-dlp config', extensions: ['conf', 'txt'] }]
        }
      ]
    ]);
    expect(state.cookieFilePath).toBe('cookies.txt');
    expect(state.compatConfigPath).toBe('yt-dlp.conf');
  });

  it('exports and clears diagnostics with unchanged UI text and command ordering', async () => {
    const { workflow, state, save, invoke } = setup();
    save.mockResolvedValueOnce('diagnostics.jsonl');
    invoke.mockResolvedValue(undefined);
    await workflow.exportDiagnostics();
    expect(save).toHaveBeenCalledWith({
      defaultPath: 'nuclear-downloader-diagnostics.jsonl',
      filters: [{ name: 'JSON Lines', extensions: ['jsonl'] }]
    });
    expect(invoke).toHaveBeenNthCalledWith(1, 'export_diagnostics', {
      destination: 'diagnostics.jsonl'
    });
    expect(state.diagnosticsMessage).toBe('Diagnostics exported successfully.');
    await workflow.clearDiagnostics();
    expect(invoke).toHaveBeenNthCalledWith(2, 'clear_diagnostics');
    expect(state.diagnosticsMessage).toBe('Local diagnostics were cleared.');
  });

  it('builds redacted diagnostics and copies them through the injected clipboard', async () => {
    const runtime = {
      tools: [{ name: 'yt-dlp', available: true, version: '1.2.3', source: 'bundled' }],
      message: 'ready'
    } as DownloaderRuntimeStatus;
    const { workflow, writeText } = setup({ getRuntimeStatus: () => runtime });
    const item = queueItem({ errorDetail: 'Source: https://private.example/video' });
    await workflow.copyDiagnostics(item);
    const copied = writeText.mock.calls[0][0] as string;
    expect(copied).toContain('Title: Video');
    expect(copied).toContain('yt-dlp: 1.2.3 (bundled)');
    expect(copied).not.toContain('private.example');
  });

  it('does not mutate state after the workflow becomes inactive', async () => {
    let active = true;
    const pending = Promise.resolve('C:\\Validated');
    const configured = setup({
      commands: {
        invoke: vi.fn(async () => {
          active = false;
          return pending;
        }) as unknown as WorkflowCommands['invoke'],
        isActive: () => active,
        unloadedError: new Error('unloaded')
      }
    });
    configured.state.outputDir = 'original';
    expect(await configured.workflow.validateOutputDirectory('candidate')).toBe(false);
    expect(configured.state.outputDir).toBe('original');
  });
});

describe('path helpers', () => {
  it('handles both path separators and all dialog result shapes', () => {
    expect(getPathBasename('C:\\folder\\cookies.txt')).toBe('cookies.txt');
    expect(getPathBasename('/folder/config.conf')).toBe('config.conf');
    expect(pickFirstPath(['a', 'b'])).toBe('a');
    expect(pickFirstPath([])).toBeNull();
    expect(pickFirstPath(null)).toBeNull();
  });
});

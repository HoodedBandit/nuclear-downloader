export const IDS = {
  videoInspection: '10000000-0000-4000-8000-000000000001',
  playlistInspection: '10000000-0000-4000-8000-000000000002',
  childInspection: '10000000-0000-4000-8000-000000000003',
  item: '20000000-0000-4000-8000-000000000001',
  playlistItem: '20000000-0000-4000-8000-000000000002',
  download: '30000000-0000-4000-8000-000000000001',
  runtime: '40000000-0000-4000-8000-000000000001',
  appUpdate: '50000000-0000-4000-8000-000000000001'
};

export const initialSnapshot = {
  schemaVersion: 1,
  queue: [],
  operations: [],
  runtimeReadiness: 'ready',
  maintenanceActive: false,
  draining: false,
  persistenceHealth: { degraded: false, error: null },
  latestSequence: 1
};

export const video = {
  id: 'fixture-video',
  title: 'Fixture Video',
  duration: 15,
  channel: 'Fixture Channel',
  thumbnail: null,
  url: 'https://fixture.test/video',
  available_qualities: ['1080p', '720p'],
  has_audio: true
};

export const childVideo = {
  ...video,
  id: 'playlist-child-1',
  title: 'Playlist Child One',
  url: 'https://fixture.test/child-1'
};

export function queueItem(id, info, now = 10) {
  return {
    schemaVersion: 1,
    id,
    sourceUrl: info.url,
    title: info.title,
    availableQualities: ['best', ...info.available_qualities],
    hasAudio: info.has_audio,
    cookieConfig: null,
    format: 'mp4',
    quality: 'best',
    outputDir: 'C:\\fixture-output',
    filenameOverride: null,
    compatConfigPath: null,
    state: 'inert',
    latestOperationId: null,
    createdAtMs: now,
    updatedAtMs: now
  };
}

export function operation(id, kind, state, overrides = {}) {
  return {
    schemaVersion: 1,
    id,
    queueItemId: null,
    kind,
    state,
    progress: state === 'completed' ? 100 : 0,
    phase: null,
    sequence: 1,
    createdAtMs: 10,
    updatedAtMs: 10,
    finishedAtMs: state === 'completed' || state === 'cancelled' ? 10 : null,
    error: null,
    inspectionResult: null,
    publishedOutput: null,
    intendedTerminalOutcome: null,
    correlationId: `correlation-${id}`,
    ...overrides
  };
}

export function applyDelta(snapshot, delta) {
  const next = { ...snapshot, latestSequence: delta.sequence };
  if (delta.kind === 'queue_item_upserted') {
    next.queue = [...snapshot.queue.filter((item) => item.id !== delta.value.id), delta.value];
  } else if (delta.kind === 'operation_upserted') {
    next.operations = [
      ...snapshot.operations.filter((item) => item.id !== delta.value.id),
      delta.value
    ];
  } else if (delta.kind === 'queue_items_removed') {
    next.queue = snapshot.queue.filter((item) => !delta.value.includes(item.id));
  } else if (delta.kind === 'operation_removed') {
    next.operations = snapshot.operations.filter((item) => item.id !== delta.value);
  } else if (delta.kind === 'runtime_readiness_changed') {
    next.runtimeReadiness = delta.value;
  } else if (delta.kind === 'maintenance_changed') {
    next.maintenanceActive = delta.value.active;
    next.draining = delta.value.draining;
  } else if (delta.kind === 'persistence_health_changed') {
    next.persistenceHealth = delta.value;
  }
  return next;
}

export async function waitForMockCalls(mock, count) {
  await browser.waitUntil(
    async () => {
      await mock.update();
      return mock.mock.calls.length >= count;
    },
    { timeout: 10_000, timeoutMsg: `Expected ${count} command calls.` }
  );
}

export async function registerRenderer(snapshot, oldMocks = [], beforeStartup) {
  for (const mock of oldMocks) await mock.mockRestore();

  await browser.execute((value) => {
    window.__NUCLEAR_E2E_SNAPSHOT__ = value;
    window.confirm = () => true;
  }, snapshot);

  const commands = [
    'get_app_snapshot',
    'check_downloader_runtime',
    'check_runtime_update',
    'default_download_dir',
    'validate_output_directory',
    'check_app_update',
    'begin_inspection',
    'add_inspection_result_to_queue',
    'update_queue_item',
    'remove_queue_items',
    'enqueue_queue_items',
    'cancel_operation',
    'cancel_all_downloads',
    'dismiss_operation',
    'begin_runtime_update',
    'begin_app_update',
    'export_diagnostics',
    'clear_diagnostics',
    'plugin:dialog|open',
    'plugin:dialog|save'
  ];
  const entries = await Promise.all(
    commands.map(async (command) => [command, await browser.tauri.mock(command)])
  );
  const mocks = Object.fromEntries(entries);

  await mocks.get_app_snapshot.mockImplementation(() => window.__NUCLEAR_E2E_SNAPSHOT__);
  await mocks.check_downloader_runtime.mockResolvedValue({
    state: 'ready',
    runtimeVersion: '2026.7.4',
    source: 'fixture',
    updateAvailable: false,
    latestRuntimeVersion: null,
    runtimeDir: 'C:\\fixture-runtime',
    pluginDir: 'C:\\fixture-plugins',
    message: null,
    tools: [
      {
        name: 'yt-dlp',
        required: true,
        available: true,
        version: 'fixture',
        path: null,
        source: 'fixture',
        error: null
      }
    ]
  });
  await mocks.check_runtime_update.mockResolvedValue({
    updateAvailable: true,
    latestRuntimeVersion: '2026.8.1',
    message: 'Signed fixture runtime is available.'
  });
  await mocks.default_download_dir.mockResolvedValue('C:\\fixture-output');
  await mocks.validate_output_directory.mockImplementation(({ path }) => path);
  await mocks.check_app_update.mockResolvedValue({
    currentVersion: '0.6.0',
    hasUpdate: true,
    latestVersion: '0.6.1',
    notes: 'Fixture release notes',
    publishedAt: '2026-08-17T12:00:00Z',
    installerName: 'Nuclear.Downloader_0.6.1_x64-setup.exe'
  });
  await mocks.begin_inspection.mockResolvedValue({ operationId: IDS.videoInspection });
  await mocks.add_inspection_result_to_queue.mockResolvedValue(queueItem(IDS.item, video));
  await mocks.update_queue_item.mockResolvedValue(undefined);
  await mocks.remove_queue_items.mockResolvedValue(undefined);
  await mocks.enqueue_queue_items.mockResolvedValue([{ operationId: IDS.download }]);
  await mocks.cancel_operation.mockResolvedValue(undefined);
  await mocks.cancel_all_downloads.mockResolvedValue({
    idle: false,
    remainingOperationIds: [IDS.download]
  });
  await mocks.dismiss_operation.mockResolvedValue(undefined);
  await mocks.begin_runtime_update.mockResolvedValue({ operationId: IDS.runtime });
  await mocks.begin_app_update.mockResolvedValue({ operationId: IDS.appUpdate });
  await mocks.export_diagnostics.mockResolvedValue(undefined);
  await mocks.clear_diagnostics.mockResolvedValue(undefined);
  await mocks['plugin:dialog|open'].mockResolvedValue(null);
  await mocks['plugin:dialog|save'].mockResolvedValue('C:\\fixture-output\\diagnostics.jsonl');

  await browser.waitUntil(
    () => browser.execute(() => typeof window.__NUCLEAR_WEBDRIVER_RELEASE_STARTUP__ === 'function'),
    { timeout: 10_000, timeoutMsg: 'WebDriver-only startup gate was not installed.' }
  );
  if (beforeStartup) await beforeStartup(mocks);
  await browser.execute(() => window.__NUCLEAR_WEBDRIVER_RELEASE_STARTUP__());
  await $('button=Add').waitForEnabled();
  return mocks;
}

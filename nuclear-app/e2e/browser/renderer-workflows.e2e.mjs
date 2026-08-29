import assert from 'node:assert/strict';

const IDS = {
  videoInspection: '10000000-0000-4000-8000-000000000001',
  playlistInspection: '10000000-0000-4000-8000-000000000002',
  childInspection: '10000000-0000-4000-8000-000000000003',
  item: '20000000-0000-4000-8000-000000000001',
  playlistItem: '20000000-0000-4000-8000-000000000002',
  download: '30000000-0000-4000-8000-000000000001',
  runtime: '40000000-0000-4000-8000-000000000001',
  appUpdate: '50000000-0000-4000-8000-000000000001'
};

const initialSnapshot = {
  schemaVersion: 1,
  queue: [],
  operations: [],
  runtimeReadiness: 'ready',
  maintenanceActive: false,
  draining: false,
  latestSequence: 1
};

const video = {
  id: 'fixture-video',
  title: 'Fixture Video',
  duration: 15,
  channel: 'Fixture Channel',
  thumbnail: null,
  url: 'https://fixture.test/video',
  available_qualities: ['1080p', '720p'],
  has_audio: true
};

const childVideo = {
  ...video,
  id: 'playlist-child-1',
  title: 'Playlist Child One',
  url: 'https://fixture.test/child-1'
};

function queueItem(id, info, now = 10) {
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

function operation(id, kind, state, overrides = {}) {
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
    correlationId: `correlation-${id}`,
    ...overrides
  };
}

function applyDelta(snapshot, delta) {
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
  }
  return next;
}

async function waitForMockCalls(mock, count) {
  await browser.waitUntil(
    async () => {
      await mock.update();
      return mock.mock.calls.length >= count;
    },
    { timeout: 10_000, timeoutMsg: `Expected ${count} command calls.` }
  );
}

async function registerRenderer(snapshot, oldMocks = []) {
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
    'enqueue_queue_items',
    'cancel_operation',
    'cancel_all_downloads',
    'dismiss_operation',
    'begin_runtime_update',
    'begin_app_update',
    'export_diagnostics',
    'clear_diagnostics',
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
  await mocks.validate_output_directory.mockResolvedValue('C:\\fixture-output');
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
  await mocks.enqueue_queue_items.mockResolvedValue([{ operationId: IDS.download }]);
  await mocks.cancel_operation.mockResolvedValue({
    operationId: IDS.download,
    state: 'cancelling'
  });
  await mocks.cancel_all_downloads.mockResolvedValue({
    idle: false,
    remainingOperationIds: [IDS.download]
  });
  await mocks.dismiss_operation.mockResolvedValue(null);
  await mocks.begin_runtime_update.mockResolvedValue({ operationId: IDS.runtime });
  await mocks.begin_app_update.mockResolvedValue({ operationId: IDS.appUpdate });
  await mocks.export_diagnostics.mockResolvedValue(null);
  await mocks.clear_diagnostics.mockResolvedValue(null);
  await mocks['plugin:dialog|save'].mockResolvedValue('C:\\fixture-output\\diagnostics.jsonl');

  await browser.waitUntil(
    () => browser.execute(() => typeof window.__NUCLEAR_WEBDRIVER_RELEASE_STARTUP__ === 'function'),
    { timeout: 10_000, timeoutMsg: 'WebDriver-only startup gate was not installed.' }
  );
  await browser.execute(() => window.__NUCLEAR_WEBDRIVER_RELEASE_STARTUP__());
  await $('button=Add').waitForEnabled();
  return mocks;
}

describe('renderer workflows with deterministic Tauri IPC', () => {
  it('covers queueing, cancellation, reload reconciliation, playlists, diagnostics, and updates', async () => {
    let snapshot = structuredClone(initialSnapshot);
    let mocks = await registerRenderer(snapshot);

    const sourceUrlInput = await $('#video-url');
    assert.equal(await sourceUrlInput.getAttribute('autocomplete'), 'off');
    assert.equal(await sourceUrlInput.getAttribute('autocapitalize'), 'none');
    assert.equal(await sourceUrlInput.getAttribute('spellcheck'), 'false');
    assert.equal(await sourceUrlInput.getAttribute('aria-autocomplete'), 'none');

    async function emit(kind, value) {
      const delta = {
        schemaVersion: 1,
        sequence: snapshot.latestSequence + 1,
        emittedAtMs: Date.now(),
        kind,
        value
      };
      snapshot = applyDelta(snapshot, delta);
      await browser.execute((next) => {
        window.__NUCLEAR_E2E_SNAPSHOT__ = next;
      }, snapshot);
      await browser.tauri.emitEvent('app-state-changed', delta);
    }

    await $('#video-url').setValue(video.url);
    await $('button=Add').click();
    await waitForMockCalls(mocks.begin_inspection, 1);
    const missedCompletionDelta = {
      schemaVersion: 1,
      sequence: snapshot.latestSequence + 1,
      emittedAtMs: Date.now(),
      kind: 'operation_upserted',
      value: operation(IDS.videoInspection, 'inspection', 'completed', {
        inspectionResult: { kind: 'video', video }
      })
    };
    snapshot = applyDelta(snapshot, missedCompletionDelta);
    await browser.execute((next) => {
      window.__NUCLEAR_E2E_SNAPSHOT__ = next;
    }, snapshot);
    // Deliberately omit app-state-changed. Add must reconcile the durable
    // snapshot instead of looking permanently stuck when one event is lost.
    await waitForMockCalls(mocks.add_inspection_result_to_queue, 1);
    assert.equal(
      mocks.add_inspection_result_to_queue.mock.calls[0][0].input.inspectionOperationId,
      IDS.videoInspection
    );
    await emit('queue_item_upserted', queueItem(IDS.item, video));
    await expect($('.queue')).toHaveText(expect.stringContaining('Fixture Video'));

    await $('button[aria-label="Download Fixture Video"]').click();
    await waitForMockCalls(mocks.enqueue_queue_items, 1);
    const runningItem = {
      ...queueItem(IDS.item, video),
      state: 'running',
      latestOperationId: IDS.download,
      updatedAtMs: 20
    };
    await emit('queue_item_upserted', runningItem);
    await emit(
      'operation_upserted',
      operation(IDS.download, 'download', 'running', {
        queueItemId: IDS.item,
        progress: 20,
        phase: 'download'
      })
    );
    await $('button=Cancel All').click();
    await waitForMockCalls(mocks.cancel_all_downloads, 1);
    await expect($('.actions')).toHaveText(
      expect.stringContaining('1 operation still stopping. New work remains paused.')
    );
    await $('button[aria-label="Cancel Fixture Video"]').click();
    await waitForMockCalls(mocks.cancel_operation, 1);
    await emit(
      'operation_upserted',
      operation(IDS.download, 'download', 'cancelled', {
        queueItemId: IDS.item,
        finishedAtMs: 30
      })
    );
    await emit('queue_item_upserted', {
      ...runningItem,
      state: 'cancelled',
      updatedAtMs: 30
    });
    await expect($('.status-pill')).toHaveText('Cancelled');

    await browser.refresh();
    mocks = await registerRenderer(snapshot, Object.values(mocks));
    await expect($('.queue')).toHaveText(expect.stringContaining('Fixture Video'));

    const reconciled = {
      ...snapshot,
      latestSequence: snapshot.latestSequence + 2,
      queue: snapshot.queue.map((item) =>
        item.id === IDS.item ? { ...item, title: 'Reconciled Fixture Video' } : item
      )
    };
    await browser.execute((next) => {
      window.__NUCLEAR_E2E_SNAPSHOT__ = next;
    }, reconciled);
    await browser.tauri.emitEvent('app-state-changed', {
      schemaVersion: 1,
      sequence: reconciled.latestSequence,
      emittedAtMs: Date.now(),
      kind: 'runtime_readiness_changed',
      value: 'ready'
    });
    snapshot = reconciled;
    await expect($('.queue')).toHaveText(expect.stringContaining('Reconciled Fixture Video'));

    await mocks.begin_inspection.mockResolvedValueOnce({
      operationId: IDS.playlistInspection
    });
    await mocks.begin_inspection.mockResolvedValue({ operationId: IDS.childInspection });
    await mocks.add_inspection_result_to_queue.mockResolvedValue(
      queueItem(IDS.playlistItem, childVideo)
    );
    await $('#video-url').setValue('https://fixture.test/playlist');
    await $('button=Add').click();
    await waitForMockCalls(mocks.begin_inspection, 1);
    await emit(
      'operation_upserted',
      operation(IDS.playlistInspection, 'inspection', 'completed', {
        inspectionResult: {
          kind: 'playlist',
          playlist: {
            title: 'Fixture Playlist',
            channel: 'Fixture Channel',
            entry_count: 2,
            truncated: false,
            entries: [
              {
                id: 'child-1',
                title: 'Playlist Child One',
                duration: 15,
                url: childVideo.url,
                thumbnail: null
              },
              {
                id: 'child-2',
                title: 'Playlist Child Two',
                duration: 20,
                url: 'https://fixture.test/child-2',
                thumbnail: null
              }
            ]
          }
        }
      })
    );
    const dialog = await $('[role="dialog"][aria-labelledby="playlist-modal-title"]');
    await dialog.waitForDisplayed();
    const checkboxes = await dialog.$$('input[type="checkbox"]');
    await checkboxes[0].click();
    await checkboxes[1].click();
    await dialog.$('button=Add 1 Videos to Queue').click();
    await waitForMockCalls(mocks.dismiss_operation, 1);
    assert.deepEqual(mocks.dismiss_operation.mock.calls[0][0], {
      operationId: IDS.playlistInspection
    });
    await waitForMockCalls(mocks.begin_inspection, 2);
    await emit(
      'operation_upserted',
      operation(IDS.childInspection, 'inspection', 'completed', {
        inspectionResult: { kind: 'video', video: childVideo }
      })
    );
    await waitForMockCalls(mocks.add_inspection_result_to_queue, 1);
    assert.equal(
      mocks.add_inspection_result_to_queue.mock.calls[0][0].input.inspectionOperationId,
      IDS.childInspection
    );
    await emit('queue_item_upserted', queueItem(IDS.playlistItem, childVideo, 40));
    await expect($('.queue')).toHaveText(expect.stringContaining('Playlist Child One'));

    await $('button=Export Diagnostics').click();
    await waitForMockCalls(mocks.export_diagnostics, 1);
    assert.deepEqual(mocks.export_diagnostics.mock.calls[0][0], {
      destination: 'C:\\fixture-output\\diagnostics.jsonl'
    });
    await $('button=Clear Diagnostics').click();
    await waitForMockCalls(mocks.clear_diagnostics, 1);
    await expect($('.actions')).toHaveText(expect.stringContaining('diagnostics were cleared'));

    await $('button=Update v0.6.1').click();
    const updateDialog = await $('[role="dialog"][aria-labelledby="update-modal-title"]');
    await updateDialog.waitForDisplayed();
    await expect(updateDialog).toHaveText(expect.stringContaining('Fixture release notes'));
    await updateDialog.$('button=Install v0.6.1').click();
    await waitForMockCalls(mocks.begin_app_update, 1);
    await emit('operation_upserted', operation(IDS.appUpdate, 'app_update', 'completed'));
    await updateDialog.$('button=Close').click();

    await $('button=Update Runtime').click();
    await waitForMockCalls(mocks.begin_runtime_update, 1);
    await emit('operation_upserted', operation(IDS.runtime, 'runtime_update', 'completed'));
  });
});

import assert from 'node:assert/strict';
import { Key } from 'webdriverio';
import {
  assertInterruptedQueueRow,
  editQueuedFilename,
  startQueuedDownloadByTitle
} from '../native/helpers.mjs';
import {
  IDS,
  applyDelta,
  childVideo,
  initialSnapshot,
  operation,
  queueItem,
  registerRenderer,
  video,
  waitForMockCalls
} from './support/renderer-fixture.mjs';

let previousMocks = [];

async function startRenderer(seed = initialSnapshot) {
  const state = { snapshot: structuredClone(seed), mocks: null };
  await browser.refresh();
  state.mocks = await registerRenderer(state.snapshot, previousMocks);
  previousMocks = Object.values(state.mocks);
  state.emit = async (kind, value) => {
    const delta = {
      schemaVersion: 1,
      sequence: state.snapshot.latestSequence + 1,
      emittedAtMs: Date.now(),
      kind,
      value
    };
    state.snapshot = applyDelta(state.snapshot, delta);
    await browser.execute((next) => {
      window.__NUCLEAR_E2E_SNAPSHOT__ = next;
    }, state.snapshot);
    await browser.tauri.emitEvent('app-state-changed', delta);
  };
  return state;
}

async function replaceFilenameDraft(value) {
  const editor = await $('input[aria-label="Edit queued filename"]');
  await editor.waitForDisplayed({ timeoutMsg: 'Queued filename editor did not open.' });
  await editor.click();
  await browser.keys([Key.Ctrl, 'a']);
  await browser.keys(value);
  return editor;
}

describe('renderer workflows with deterministic Tauri IPC', () => {
  it('covers queue actions, cancellation, retry, removal, and reload reconciliation', async () => {
    const fixture = await startRenderer();
    let { mocks } = fixture;
    const sourceUrlInput = await $('#video-url');
    assert.equal(await sourceUrlInput.getAttribute('autocomplete'), 'off');
    assert.equal(await sourceUrlInput.getAttribute('autocapitalize'), 'none');
    assert.equal(await sourceUrlInput.getAttribute('spellcheck'), 'false');
    assert.equal(await sourceUrlInput.getAttribute('aria-autocomplete'), 'none');

    await sourceUrlInput.setValue(video.url);
    await $('button=Add').click();
    await waitForMockCalls(mocks.begin_inspection, 1);
    const missedCompletion = {
      schemaVersion: 1,
      sequence: fixture.snapshot.latestSequence + 1,
      emittedAtMs: Date.now(),
      kind: 'operation_upserted',
      value: operation(IDS.videoInspection, 'inspection', 'completed', {
        inspectionResult: { kind: 'video', video }
      })
    };
    fixture.snapshot = applyDelta(fixture.snapshot, missedCompletion);
    await browser.execute((next) => {
      window.__NUCLEAR_E2E_SNAPSHOT__ = next;
    }, fixture.snapshot);
    await waitForMockCalls(mocks.add_inspection_result_to_queue, 1);
    assert.equal(
      mocks.add_inspection_result_to_queue.mock.calls[0][0].input.inspectionOperationId,
      IDS.videoInspection
    );

    await fixture.emit('queue_item_upserted', queueItem(IDS.item, video));
    await $('button[aria-label="Download Fixture Video"]').click();
    await waitForMockCalls(mocks.enqueue_queue_items, 1);
    assert.deepEqual(mocks.enqueue_queue_items.mock.calls[0][0], {
      itemIds: [IDS.item],
      priority: 'front'
    });
    const runningItem = {
      ...queueItem(IDS.item, video),
      state: 'running',
      latestOperationId: IDS.download,
      updatedAtMs: 20
    };
    await fixture.emit('queue_item_upserted', runningItem);
    await fixture.emit(
      'operation_upserted',
      operation(IDS.download, 'download', 'running', {
        queueItemId: IDS.item,
        progress: 20,
        phase: 'download'
      })
    );
    await browser.tauri.emitEvent('download-progress', {
      download_id: IDS.download,
      status: 'downloading',
      progress: 20,
      download_progress: 20,
      conversion_progress: null,
      phase: 'download',
      speed: '1 MiB/s',
      eta: '12s',
      error: null,
      error_code: null,
      error_detail: null,
      filename: null
    });
    await expect($('.progress-text')).toHaveText('20%');
    assert.match(await $('.progress-fill').getAttribute('style'), /width:\s*20%/);
    await expect($('tr.queue-item .col-speed')).toHaveText(expect.stringContaining('1 MiB/s'));
    await expect($('tr.queue-item .col-eta')).toHaveText(expect.stringContaining('12s'));
    await $('button=Cancel All').click();
    await waitForMockCalls(mocks.cancel_all_downloads, 1);
    await expect($('.actions')).toHaveText(
      expect.stringContaining('1 operation still stopping. New work remains paused.')
    );
    await $('button[aria-label="Cancel Fixture Video"]').click();
    await waitForMockCalls(mocks.cancel_operation, 1);
    assert.deepEqual(mocks.cancel_operation.mock.calls[0][0], { operationId: IDS.download });
    await fixture.emit(
      'operation_upserted',
      operation(IDS.download, 'download', 'cancelled', {
        queueItemId: IDS.item,
        finishedAtMs: 30
      })
    );
    await fixture.emit('queue_item_upserted', {
      ...runningItem,
      state: 'cancelled',
      updatedAtMs: 30
    });
    await expect($('.status-pill')).toHaveText('Cancelled');
    await $('button=Retry').click();
    await waitForMockCalls(mocks.enqueue_queue_items, 2);
    await $('input[aria-label="Select Fixture Video"]').click();
    await $('button=Remove Selected').click();
    await waitForMockCalls(mocks.remove_queue_items, 1);
    assert.deepEqual(mocks.remove_queue_items.mock.calls[0][0], { itemIds: [IDS.item] });

    await browser.refresh();
    mocks = await registerRenderer(fixture.snapshot, Object.values(mocks));
    previousMocks = Object.values(mocks);
    await expect($('.queue')).toHaveText(expect.stringContaining('Fixture Video'));
    const reconciled = {
      ...fixture.snapshot,
      latestSequence: fixture.snapshot.latestSequence + 2,
      queue: fixture.snapshot.queue.map((item) =>
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
    await expect($('.queue')).toHaveText(expect.stringContaining('Reconciled Fixture Video'));
  });

  it('restores a running row and exposes diagnostics when item cancellation fails', async () => {
    const running = {
      ...queueItem(IDS.item, video),
      state: 'running',
      latestOperationId: IDS.download,
      updatedAtMs: 20
    };
    const seed = {
      ...structuredClone(initialSnapshot),
      queue: [running],
      operations: [
        operation(IDS.download, 'download', 'running', {
          queueItemId: IDS.item,
          progress: 20,
          phase: 'download'
        })
      ]
    };
    const { mocks } = await startRenderer(seed);
    await mocks.cancel_operation.mockRejectedValueOnce('fixture cancellation rejected');
    await $('button[aria-label="Cancel Fixture Video"]').click();
    await waitForMockCalls(mocks.cancel_operation, 1);
    await expect($('.status-pill')).toHaveText('Downloading');
    await expect($('.error-summary')).toHaveText(
      expect.stringContaining('Cancellation failed: fixture cancellation rejected')
    );
    await expect($('.diagnostics-panel')).toBeDisplayed();
    await expect($('button=Retry')).not.toBeExisting();
  });

  it('renders a restored interruption as a retryable error with backend diagnostics', async () => {
    const interruptedItem = {
      ...queueItem(IDS.item, video),
      state: 'interrupted',
      latestOperationId: IDS.download,
      updatedAtMs: 20
    };
    const interruptedOperation = operation(IDS.download, 'download', 'interrupted', {
      queueItemId: IDS.item,
      finishedAtMs: 20,
      error: {
        code: 'interrupted',
        summary: 'The application stopped before this operation finished.',
        detail: null,
        retryable: true,
        correlationId: null
      }
    });
    const { mocks } = await startRenderer({
      ...structuredClone(initialSnapshot),
      queue: [interruptedItem],
      operations: [interruptedOperation]
    });
    const row = await $('tr.queue-item');

    await assertInterruptedQueueRow(row);
    await mocks.enqueue_queue_items.update();
    assert.equal(mocks.enqueue_queue_items.mock.calls.length, 0);
    await row.$('button=Retry').click();
    await waitForMockCalls(mocks.enqueue_queue_items, 1);
    assert.deepEqual(mocks.enqueue_queue_items.mock.calls[0][0], {
      itemIds: [IDS.item],
      priority: 'front'
    });
  });

  it('cancels an in-flight inspection without displaying a failure', async () => {
    const fixture = await startRenderer();
    const { mocks } = fixture;
    await $('#video-url').setValue(video.url);
    await $('button=Add').click();
    await waitForMockCalls(mocks.begin_inspection, 1);
    await $('button=Cancel').click();
    await waitForMockCalls(mocks.cancel_operation, 1);
    assert.deepEqual(mocks.cancel_operation.mock.calls[0][0], {
      operationId: IDS.videoInspection
    });
    await fixture.emit(
      'operation_upserted',
      operation(IDS.videoInspection, 'inspection', 'cancelled')
    );
    await $('button=Add').waitForEnabled();
    await expect($('#url-error')).not.toBeExisting();
  });

  it('covers playlist selection and child inspection', async () => {
    const fixture = await startRenderer();
    const { mocks } = fixture;
    await mocks.begin_inspection.mockResolvedValueOnce({ operationId: IDS.playlistInspection });
    await mocks.begin_inspection.mockResolvedValue({ operationId: IDS.childInspection });
    await mocks.add_inspection_result_to_queue.mockResolvedValue(
      queueItem(IDS.playlistItem, childVideo)
    );
    await $('#video-url').setValue('https://fixture.test/playlist');
    await $('button=Add').click();
    await waitForMockCalls(mocks.begin_inspection, 1);
    await fixture.emit(
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
    await fixture.emit(
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
    await fixture.emit('queue_item_upserted', queueItem(IDS.playlistItem, childVideo, 40));
    await expect($('.queue')).toHaveText(expect.stringContaining('Playlist Child One'));
  });

  it('covers filename sanitizing plus Enter, Escape, and blur editing', async () => {
    const seed = { ...structuredClone(initialSnapshot), queue: [queueItem(IDS.item, video)] };
    const fixture = await startRenderer(seed);
    const { mocks } = fixture;
    await $('.title-button').click();
    await replaceFilenameDraft('   ');
    await browser.keys('Enter');
    await expect($('.filename-error')).toHaveText(
      'Filename must contain at least one valid character.'
    );
    await mocks.update_queue_item.update();
    assert.equal(mocks.update_queue_item.mock.calls.length, 0);
    await replaceFilenameDraft('Renamed Fixture.mp4');
    await browser.keys('Enter');
    await waitForMockCalls(mocks.update_queue_item, 1);
    assert.deepEqual(mocks.update_queue_item.mock.calls[0][0], {
      itemId: IDS.item,
      input: { filenameOverride: 'Renamed Fixture' }
    });
    await expect($('.queue')).toHaveText(expect.stringContaining('Fixture Video'));

    await fixture.emit('queue_item_upserted', {
      ...queueItem(IDS.item, video),
      filenameOverride: 'Renamed Fixture',
      updatedAtMs: 20
    });
    await expect($('.title-button')).toHaveText('Renamed Fixture');
    await $('.title-button').click();
    await replaceFilenameDraft('Discarded Draft');
    await browser.keys('Escape');
    await expect($('input[aria-label="Edit queued filename"]')).not.toBeExisting();
    await mocks.update_queue_item.update();
    assert.equal(mocks.update_queue_item.mock.calls.length, 1);

    // Exercise the native acceptance helper here so an unsupported WebDriver
    // command fails during ordinary CI, before a signed candidate is built.
    await editQueuedFilename(await $('tr.queue-item'), 'CON.txt');
    await waitForMockCalls(mocks.update_queue_item, 2);
    assert.deepEqual(mocks.update_queue_item.mock.calls[1][0], {
      itemId: IDS.item,
      input: { filenameOverride: 'CON_.txt' }
    });

    await fixture.emit('queue_item_upserted', {
      ...queueItem(IDS.item, video),
      filenameOverride: 'CON_.txt',
      updatedAtMs: 30
    });
    await $('.title-button').click();
    await replaceFilenameDraft('Blur Commit.webm');
    await $('h1').click();
    await waitForMockCalls(mocks.update_queue_item, 3);
    assert.deepEqual(mocks.update_queue_item.mock.calls[2][0], {
      itemId: IDS.item,
      input: { filenameOverride: 'Blur Commit' }
    });
  });

  it('keeps the ninth queue row actionable for filename editing and download at 1000x700', async () => {
    const originalSize = await browser.getWindowSize();
    const renamed = 'nuclear-interrupted-34442304569';
    try {
      await browser.setWindowSize(1000, 700);
      const queued = Array.from({ length: 9 }, (_, index) => {
        const suffix = String(index + 1).padStart(12, '0');
        return queueItem(
          `20000000-0000-4000-8000-${suffix}`,
          {
            ...video,
            id: `seed-video-${index + 1}`,
            title: `Seed Video ${index + 1}`,
            url: `https://fixture.test/seed-video-${index + 1}`
          },
          10 + index
        );
      });
      const fixture = await startRenderer({ ...structuredClone(initialSnapshot), queue: queued });
      const { mocks } = fixture;
      const rows = await $$('tr.queue-item');
      assert.equal(rows.length, 9);
      const ninthRow = rows[8];

      await editQueuedFilename(ninthRow, renamed);
      await waitForMockCalls(mocks.update_queue_item, 1);
      assert.deepEqual(mocks.update_queue_item.mock.calls[0][0], {
        itemId: queued[8].id,
        input: { filenameOverride: renamed }
      });
      await fixture.emit('queue_item_upserted', {
        ...queued[8],
        filenameOverride: renamed,
        updatedAtMs: 30
      });

      await startQueuedDownloadByTitle(renamed);
      await waitForMockCalls(mocks.enqueue_queue_items, 1);
      assert.equal(mocks.enqueue_queue_items.mock.calls.length, 1);
      assert.deepEqual(mocks.enqueue_queue_items.mock.calls[0][0], {
        itemIds: [queued[8].id],
        priority: 'front'
      });
    } finally {
      await browser.setWindowSize(originalSize.width, originalSize.height);
    }
  });

  it('covers settings and output, cookie, and compatibility paths', async () => {
    const seed = { ...structuredClone(initialSnapshot), queue: [queueItem(IDS.item, video)] };
    const { mocks } = await startRenderer(seed);
    await $('#quality').selectByAttribute('value', '720p');
    await waitForMockCalls(mocks.update_queue_item, 1);
    assert.deepEqual(mocks.update_queue_item.mock.calls[0][0], {
      itemId: IDS.item,
      input: { quality: '720p' }
    });
    await $('#format').selectByAttribute('value', 'mp3');
    await waitForMockCalls(mocks.update_queue_item, 2);
    assert.deepEqual(mocks.update_queue_item.mock.calls[1][0], {
      itemId: IDS.item,
      input: { format: 'mp3' }
    });
    await mocks['plugin:dialog|open'].mockResolvedValueOnce('C:\\chosen-output');
    await $('button=Browse').click();
    await waitForMockCalls(mocks.validate_output_directory, 2);
    assert.deepEqual(mocks.validate_output_directory.mock.calls.at(-1)[0], {
      path: 'C:\\chosen-output'
    });
    await expect($('#outdir')).toHaveValue('C:\\chosen-output');
    await $('label=Cookies').click();
    await $('#cookie-mode').selectByAttribute('value', 'file');
    await mocks['plugin:dialog|open'].mockResolvedValueOnce('C:\\fixtures\\cookies.txt');
    await $('button=Select cookies.txt').click();
    await expect($('button=cookies.txt')).toBeDisplayed();
    await mocks['plugin:dialog|open'].mockResolvedValueOnce('C:\\fixtures\\yt-dlp.conf');
    await $('#compat-config').click();
    await expect($('button=yt-dlp.conf')).toBeDisplayed();
  });

  it('covers diagnostics and persistence degradation', async () => {
    const fixture = await startRenderer();
    const { mocks } = fixture;
    await $('button=Export Diagnostics').click();
    await waitForMockCalls(mocks.export_diagnostics, 1);
    assert.deepEqual(mocks.export_diagnostics.mock.calls[0][0], {
      destination: 'C:\\fixture-output\\diagnostics.jsonl'
    });
    await $('button=Clear Diagnostics').click();
    await waitForMockCalls(mocks.clear_diagnostics, 1);
    await expect($('.actions')).toHaveText(expect.stringContaining('diagnostics were cleared'));
    await fixture.emit('persistence_health_changed', {
      degraded: true,
      error: 'Fixture persistence is degraded.'
    });
    await expect($('.actions')).toHaveText(
      expect.stringContaining('Fixture persistence is degraded.')
    );
  });

  it('reloads the authoritative snapshot when the backend requests resynchronization', async () => {
    const seed = { ...structuredClone(initialSnapshot), queue: [queueItem(IDS.item, video)] };
    await startRenderer(seed);
    const resynced = {
      ...seed,
      latestSequence: 5,
      queue: [{ ...seed.queue[0], title: 'Resynchronized Fixture Video', updatedAtMs: 50 }]
    };
    await browser.execute((next) => {
      window.__NUCLEAR_E2E_SNAPSHOT__ = next;
    }, resynced);
    await browser.tauri.emitEvent('app-state-resync-required', { latestSequence: 5 });
    await expect($('.queue')).toHaveText(expect.stringContaining('Resynchronized Fixture Video'));
  });

  it('shows focused command failures and reconciles failed queue settings', async () => {
    const seed = { ...structuredClone(initialSnapshot), queue: [queueItem(IDS.item, video)] };
    const { mocks } = await startRenderer(seed);
    await mocks.update_queue_item.mockRejectedValueOnce('fixture setting rejected');
    await $('#quality').selectByAttribute('value', '720p');
    await waitForMockCalls(mocks.update_queue_item, 1);
    await expect($('.actions')).toHaveText(expect.stringContaining('fixture setting rejected'));
    await waitForMockCalls(mocks.get_app_snapshot, 2);

    await mocks.export_diagnostics.mockRejectedValueOnce('fixture export rejected');
    await $('button=Export Diagnostics').click();
    await waitForMockCalls(mocks.export_diagnostics, 1);
    await expect($('.actions')).toHaveText(expect.stringContaining('fixture export rejected'));
  });

  it('covers runtime refresh and runtime update completion', async () => {
    const fixture = await startRenderer();
    const { mocks } = fixture;
    await $('button=Check Runtime').click();
    await waitForMockCalls(mocks.check_downloader_runtime, 2);
    await $('button=Update Runtime').click();
    await waitForMockCalls(mocks.begin_runtime_update, 1);
    await browser.tauri.emitEvent('downloader-runtime-update-progress', {
      status: 'error',
      version: '2026.8.1',
      downloadedBytes: 10,
      totalBytes: 20,
      message: 'Fixture runtime update failed.'
    });
    await expect($('.url-bar')).toHaveText(
      expect.stringContaining('Fixture runtime update failed.')
    );
    await expect($('button=Update Runtime')).toBeEnabled();
    await fixture.emit('operation_upserted', operation(IDS.runtime, 'runtime_update', 'completed'));
    await waitForMockCalls(mocks.check_downloader_runtime, 3);
    await expect($('[data-testid="runtime-status"]')).toHaveText(
      expect.stringContaining('Runtime ready')
    );
  });

  it('covers app update details and completion', async () => {
    const fixture = await startRenderer();
    const { mocks } = fixture;
    const trigger = await $('button=Update v0.6.1');
    await trigger.click();
    const dialog = await $('[role="dialog"][aria-labelledby="update-modal-title"]');
    await dialog.waitForDisplayed();
    await browser.waitUntil(
      async () => browser.execute(() => document.activeElement?.textContent?.trim() === 'Close'),
      { timeoutMsg: 'Update dialog did not focus its Close button.' }
    );
    assert.equal(await browser.execute(() => document.querySelector('main')?.inert), true);
    await browser.keys('Escape');
    await expect(dialog).not.toBeDisplayed();
    assert.equal(await browser.execute(() => document.querySelector('main')?.inert), false);
    assert.equal(
      await browser.execute(() => document.activeElement?.textContent?.trim()),
      'Update v0.6.1'
    );
    await trigger.click();
    const reopenedDialog = await $('[role="dialog"][aria-labelledby="update-modal-title"]');
    await reopenedDialog.waitForDisplayed();
    await expect(reopenedDialog).toHaveText(expect.stringContaining('Fixture release notes'));
    await reopenedDialog.$('button=Install v0.6.1').click();
    await waitForMockCalls(mocks.begin_app_update, 1);
    assert.deepEqual(mocks.begin_app_update.mock.calls[0][0], { expectedVersion: '0.6.1' });
    await browser.tauri.emitEvent('update-install-progress', {
      status: 'error',
      version: '0.6.1',
      downloadedBytes: 10,
      totalBytes: 20,
      message: 'Fixture app update failed.'
    });
    await expect(reopenedDialog).toHaveText(expect.stringContaining('Fixture app update failed.'));
    await fixture.emit('operation_upserted', operation(IDS.appUpdate, 'app_update', 'completed'));
    await reopenedDialog.$('button=Close').click();
    await expect(reopenedDialog).not.toBeDisplayed();
  });
});

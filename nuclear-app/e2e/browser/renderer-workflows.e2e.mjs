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
    const encoded = JSON.stringify({ snapshot: state.snapshot, delta });
    await browser.execute((serialized) => {
      const next = JSON.parse(serialized);
      window.__NUCLEAR_E2E_SNAPSHOT__ = next.snapshot;
      window.__wdio_emit_tauri_event__('app-state-changed', next.delta);
    }, encoded);
  };
  state.resync = async (next) => {
    state.snapshot = structuredClone(next);
    const encoded = JSON.stringify(state.snapshot);
    await browser.execute((serialized) => {
      const snapshot = JSON.parse(serialized);
      window.__NUCLEAR_E2E_SNAPSHOT__ = snapshot;
      window.__wdio_emit_tauri_event__('app-state-resync-required', {
        latestSequence: snapshot.latestSequence
      });
    }, encoded);
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
    await $('button=Add link').click();
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
    await $('summary[aria-label="More queue actions"]').click();
    await $('button=Cancel all downloads').click();
    await $('summary[aria-label="More queue actions"]').click();
    await waitForMockCalls(mocks.cancel_all_downloads, 1);
    await expect($('.error-notice')).toHaveText(expect.stringContaining('Check Settings'));
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
    await $('button[aria-label="Remove selected"]').click();
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
    await expect($('.error-summary')).toHaveText('Check Settings for details.');
    assert.equal(
      await $('main')
        .getText()
        .then((text) => text.includes('fixture cancellation rejected')),
      false
    );
    await $('.settings-nav').click();
    await $('.error-entry summary').click();
    await expect($('.error-history')).toHaveText(
      expect.stringContaining('fixture cancellation rejected')
    );
    await browser.keys('Escape');
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
    await $('button=Add link').click();
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
    await $('button=Add link').waitForEnabled();
    await expect($('#url-error')).not.toBeExisting();
  });

  it('covers backend-authoritative playlist batch admission', async () => {
    const fixture = await startRenderer();
    const { mocks } = fixture;
    await mocks.begin_inspection.mockResolvedValueOnce({ operationId: IDS.playlistInspection });
    await mocks.add_inspection_result_to_queue.mockImplementation(({ input }) => ({
      kind: 'playlist',
      requestId: input.playlist.requestId,
      itemIds: ['20000000-0000-4000-8000-000000000002'],
      skippedCount: 0
    }));
    await $('#video-url').setValue('https://fixture.test/playlist');
    await $('button=Add link').click();
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
                thumbnail: null,
                video: childVideo
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
    await waitForMockCalls(mocks.add_inspection_result_to_queue, 1);
    const input = mocks.add_inspection_result_to_queue.mock.calls[0][0].input;
    assert.equal(input.inspectionOperationId, IDS.playlistInspection);
    assert.deepEqual(input.playlist.entryIndices, [0]);
    assert.match(input.playlist.requestId, /^[0-9a-f-]{36}$/i);
    assert.equal(mocks.begin_inspection.mock.calls.length, 1);
    assert.equal(mocks.dismiss_operation.mock.calls.length, 0);
    await dialog.waitForExist({ reverse: true });
    await expect($('#url-error')).not.toBeExisting();

    await fixture.emit('queue_item_upserted', queueItem(IDS.playlistItem, childVideo, 40));
    await expect($('.queue')).toHaveText(expect.stringContaining('Playlist Child One'));
    await expect($('.queue')).toHaveText(expect.stringContaining('0:15'));
  });

  for (const { count, deadlineMs } of [
    { count: 100, deadlineMs: 1_000 },
    { count: 1_000, deadlineMs: 2_000 }
  ]) {
    it(`confirms and renders ${count} pending playlist rows within ${deadlineMs}ms`, async () => {
      // This measures the real renderer against a deterministic IPC snapshot/resync fixture.
      // It does not exercise or qualify the Rust journal durability path.
      const traceStartedAt = performance.now();
      const trace = (milestone) =>
        console.log(
          `[playlist-${count}] ${milestone} ${(performance.now() - traceStartedAt).toFixed(1)}ms`
        );
      const fixture = await startRenderer();
      trace('startRenderer done');
      const { mocks } = fixture;
      const entries = Array.from({ length: count }, (_, index) => ({
        id: `entry-${index}`,
        title: `Batch entry ${index}`,
        duration: index + 1,
        url: `https://fixture.test/batch/${index}`,
        thumbnail: null
      }));
      await mocks.begin_inspection.mockResolvedValueOnce({ operationId: IDS.playlistInspection });
      trace('inspection mock installed');
      await $('#video-url').setValue('https://fixture.test/large-playlist');
      trace('URL set');
      await $('button=Add link').click();
      trace('Add clicked');
      await waitForMockCalls(mocks.begin_inspection, 1);
      trace('inspection call observed');
      trace('before playlist fixture emit');
      await fixture.emit(
        'operation_upserted',
        operation(IDS.playlistInspection, 'inspection', 'completed', {
          inspectionResult: {
            kind: 'playlist',
            playlist: {
              title: `${count} entry fixture`,
              channel: 'Fixture Channel',
              entry_count: count,
              truncated: false,
              entries
            }
          }
        })
      );
      trace('after playlist fixture emit');
      const dialog = await $('[role="dialog"][aria-labelledby="playlist-modal-title"]');
      await dialog.waitForDisplayed();
      trace('dialog displayed');
      const queue = entries.map((entry, index) => {
        const id = `20000000-0000-4000-8001-${String(index).padStart(12, '0')}`;
        return {
          ...queueItem(id, {
            ...video,
            id: entry.id,
            title: entry.title,
            duration: entry.duration,
            url: entry.url
          }),
          preparation: 'pending',
          preparationOperationId: `10000000-0000-4000-8001-${String(index).padStart(12, '0')}`,
          latestOperationId: `10000000-0000-4000-8001-${String(index).padStart(12, '0')}`
        };
      });
      const operations = queue.map((item, index) =>
        operation(item.latestOperationId, 'inspection', 'queued', {
          queueItemId: item.id,
          createdAtMs: index,
          updatedAtMs: index
        })
      );
      const admittedSnapshot = {
        ...structuredClone(initialSnapshot),
        queue,
        operations,
        latestSequence: fixture.snapshot.latestSequence + 1
      };
      const encodedAdmittedSnapshot = JSON.stringify(admittedSnapshot);
      await browser.execute((serialized) => {
        window.__NUCLEAR_E2E_BATCH_SNAPSHOT__ = JSON.parse(serialized);
      }, encodedAdmittedSnapshot);
      trace('admitted snapshot uploaded');
      await mocks.add_inspection_result_to_queue.mockImplementation(({ input }) => {
        window.__NUCLEAR_E2E_RENDER_MILESTONES__?.push({
          name: 'mock entered',
          at: performance.now()
        });
        const snapshot = window.__NUCLEAR_E2E_BATCH_SNAPSHOT__;
        window.__NUCLEAR_E2E_SNAPSHOT__ = snapshot;
        window.__wdio_emit_tauri_event__('app-state-resync-required', {
          latestSequence: snapshot.latestSequence
        });
        window.__NUCLEAR_E2E_RENDER_MILESTONES__?.push({
          name: 'resync dispatched',
          at: performance.now()
        });
        return {
          kind: 'playlist',
          requestId: input.playlist.requestId,
          itemIds: input.playlist.entryIndices.map(
            (index) => `20000000-0000-4000-8001-${String(index).padStart(12, '0')}`
          ),
          skippedCount: 0
        };
      });
      trace('batch mock installed');
      trace('before timed execute');
      await browser.execute(() => {
        const button = document.querySelector('[role="dialog"] .modal-footer .primary');
        if (!(button instanceof HTMLButtonElement))
          throw new Error('Playlist confirmation missing.');
        window.__NUCLEAR_E2E_RENDER_TIMING__ = new Promise((resolve) => {
          const startedAt = performance.now();
          window.__NUCLEAR_E2E_RENDER_MILESTONES__ = [{ name: 'confirm click', at: startedAt }];
          let settled = false;
          const finish = (result) => {
            if (settled) return;
            settled = true;
            observer.disconnect();
            clearTimeout(timeout);
            resolve(result);
          };
          const observer = new MutationObserver(() => {
            if (!document.querySelector('.queue-item')) return;
            window.__NUCLEAR_E2E_RENDER_MILESTONES__.push({
              name: 'first row mutation',
              at: performance.now()
            });
            requestAnimationFrame(() =>
              requestAnimationFrame(() => {
                window.__NUCLEAR_E2E_RENDER_MILESTONES__.push({
                  name: 'paint settled',
                  at: performance.now()
                });
                finish({
                  elapsedMs: performance.now() - startedAt,
                  milestones: window.__NUCLEAR_E2E_RENDER_MILESTONES__.map((entry) => ({
                    name: entry.name,
                    elapsedMs: entry.at - startedAt
                  }))
                });
              })
            );
          });
          const timeout = setTimeout(
            () =>
              finish({
                error: 'render timeout',
                milestones: window.__NUCLEAR_E2E_RENDER_MILESTONES__.map((entry) => ({
                  name: entry.name,
                  elapsedMs: entry.at - startedAt
                }))
              }),
            2_000
          );
          observer.observe(document.body, { childList: true, subtree: true });
          button.click();
        });
      });
      trace('after timed execute');
      const timing = await browser.executeAsync((done) => {
        window.__NUCLEAR_E2E_RENDER_TIMING__.then(done);
      });
      trace(`timing result ${JSON.stringify(timing)}`);
      try {
        assert.equal(
          timing.error,
          undefined,
          `${count} pending rows exceeded the 2s harness timeout.`
        );
        assert.ok(
          timing.elapsedMs <= deadlineMs,
          `${count} rows took ${timing.elapsedMs}ms (limit ${deadlineMs}ms).`
        );
      } finally {
        await browser.execute(() => {
          delete window.__NUCLEAR_E2E_BATCH_SNAPSHOT__;
          delete window.__NUCLEAR_E2E_RENDER_TIMING__;
          delete window.__NUCLEAR_E2E_RENDER_MILESTONES__;
        });
      }
      assert.equal(
        await browser.execute(() => window.__NUCLEAR_E2E_SNAPSHOT__.queue.length),
        count
      );
      assert.ok(
        (await $$('.queue-item')).length < 50,
        'Virtualized queue mounted too many DOM rows.'
      );
      assert.equal((await $$('select[aria-label^="Quality for"]')).length, 0);
      assert.equal((await $$('.title-button')).length, 0);
    });
  }
  it('covers filename sanitizing plus Enter, Escape, and blur editing', async () => {
    const seed = { ...structuredClone(initialSnapshot), queue: [queueItem(IDS.item, video)] };
    const fixture = await startRenderer(seed);
    const { mocks } = fixture;
    await $('.title-button').click();
    await replaceFilenameDraft('   ');
    await browser.keys('Enter');
    await expect($('.filename-error')).toHaveText('Check Settings for filename details.');
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
    await $('button[aria-label="Choose output folder"]').click();
    await waitForMockCalls(mocks.validate_output_directory, 2);
    assert.deepEqual(mocks.validate_output_directory.mock.calls.at(-1)[0], {
      path: 'C:\\chosen-output'
    });
    await expect($('#outdir')).toHaveAttribute('title', 'C:\\chosen-output');
    await $('.settings-nav').click();
    await $('label=Use browser cookies').click();
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
    await $('.settings-nav').click();
    await $('button=Export diagnostics').click();
    await waitForMockCalls(mocks.export_diagnostics, 1);
    assert.deepEqual(mocks.export_diagnostics.mock.calls[0][0], {
      destination: 'C:\\fixture-output\\diagnostics.jsonl'
    });
    await $('button=Clear diagnostics').click();
    await waitForMockCalls(mocks.clear_diagnostics, 1);
    await expect($('.settings-dialog')).toHaveText(
      expect.stringContaining('diagnostics were cleared')
    );
    await fixture.emit('persistence_health_changed', {
      degraded: true,
      error: 'Fixture persistence is degraded.'
    });
    await $('.error-entry summary').click();
    await expect($('.error-history')).toHaveText(
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
    await expect($('.error-notice')).toHaveText(expect.stringContaining('Check Settings'));
    await waitForMockCalls(mocks.get_app_snapshot, 2);

    await $('.settings-nav').click();
    await mocks.export_diagnostics.mockRejectedValueOnce('fixture export rejected');
    await $('button=Export diagnostics').click();
    await waitForMockCalls(mocks.export_diagnostics, 1);
    await expect($('.settings-dialog')).toHaveText(
      expect.stringContaining('fixture export rejected')
    );
  });

  it('covers runtime refresh and runtime update completion', async () => {
    const fixture = await startRenderer();
    const { mocks } = fixture;
    await $('.settings-nav').click();
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
    await expect($('.settings-dialog')).toHaveText(
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
    await $('.settings-nav').click();
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
    assert.equal(await browser.execute(() => document.querySelector('main')?.inert), true);
    await $('button=Update v0.6.1').click();
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
    await expect(reopenedDialog).toHaveText(
      expect.stringContaining('Check Settings for more information.')
    );
    await fixture.emit('operation_upserted', operation(IDS.appUpdate, 'app_update', 'completed'));
    await reopenedDialog.$('button=Close').click();
    await expect(reopenedDialog).not.toBeDisplayed();
  });
});

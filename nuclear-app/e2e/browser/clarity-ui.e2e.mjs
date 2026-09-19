import assert from 'node:assert/strict';
import path from 'node:path';
import {
  initialSnapshot,
  video,
  queueItem,
  operation,
  registerRenderer,
  waitForMockCalls
} from './support/renderer-fixture.mjs';

const scale = Number(process.env.NUCLEAR_E2E_SCALE ?? '1');
let previousMocks = [];
async function start(seed = initialSnapshot, setup) {
  await browser.refresh();
  const mocks = await registerRenderer(seed, previousMocks, setup);
  previousMocks = Object.values(mocks);
  return mocks;
}
const itemId = (i) => `20000000-0000-4000-8000-${String(i).padStart(12, '0')}`;
const opId = (i) => `30000000-0000-4000-8000-${String(i).padStart(12, '0')}`;
function sampleQueue() {
  const titles = [
    'The coast at dawn',
    'Designing a quieter workspace',
    'Piano session No. 4',
    'Walking Kyoto at night',
    'Color and light'
  ];
  const queue = titles.map((title, i) => ({
    ...queueItem(itemId(i), { ...video, title }),
    quality: i === 2 ? '720p' : '1080p',
    state: i < 2 ? 'running' : i === 4 ? 'completed' : 'inert',
    latestOperationId: i < 2 || i === 4 ? opId(i) : null
  }));
  return {
    ...structuredClone(initialSnapshot),
    queue,
    operations: [
      operation(opId(0), 'download', 'running', {
        queueItemId: itemId(0),
        progress: 62,
        phase: 'download'
      }),
      operation(opId(1), 'download', 'running', {
        queueItemId: itemId(1),
        progress: 100,
        phase: 'postprocess'
      }),
      operation(opId(4), 'download', 'completed', {
        queueItemId: itemId(4),
        publishedOutput: { path: 'C:\\fixture-output\\Color and light.mp4', recordedAtMs: 1 }
      })
    ]
  };
}
async function capture(name) {
  if (process.env.NUCLEAR_VISUAL_OUTPUT_DIRECTORY) {
    await browser.saveScreenshot(
      path.join(process.env.NUCLEAR_VISUAL_OUTPUT_DIRECTORY, `${name}.png`)
    );
  }
}
async function noOverflow() {
  const dimensions = await browser.execute(() => {
    const queue = document.querySelector('.queue');
    return {
      body: document.documentElement.scrollWidth,
      viewport: innerWidth,
      queue: queue.scrollWidth,
      queueViewport: queue.clientWidth
    };
  });
  assert.ok(dimensions.body <= dimensions.viewport, JSON.stringify(dimensions));
  assert.ok(dimensions.queue <= dimensions.queueViewport + 1, JSON.stringify(dimensions));
}

describe('Clarity UI', () => {
  it('retains a failed draft when its virtualized row scrolls out of view and returns through a filter', async () => {
    await browser.setViewport({ width: 1340, height: 850, devicePixelRatio: scale });
    const seed = {
      ...structuredClone(initialSnapshot),
      queue: Array.from({ length: 1000 }, (_, i) =>
        queueItem(itemId(i), { ...video, title: `Virtual item ${i}` })
      )
    };
    const mocks = await start(seed);
    await mocks.update_queue_item.mockRejectedValue('The worker claimed this item');
    await browser.execute(() => {
      document.querySelector('.queue').scrollTop = 150 * 88;
    });
    const title = $('button.title-button=Virtual item 150');
    await title.waitForDisplayed();
    await title.click();
    await browser.keys('Preserved virtual draft');
    await browser.keys('Enter');
    await $('.filename-error').waitForDisplayed();
    await $('button*=Queued').click();
    await browser.waitUntil(async () => !(await $('.title-editor').isExisting()));
    await browser.execute(() => {
      document.querySelector('.queue').scrollTop = 150 * 88;
    });
    await expect($('.title-editor')).toHaveValue('Preserved virtual draft');
    assert.ok(
      (await $$('tr.queue-item')).length < 40,
      'The live table must still virtualize the queue.'
    );
    await mocks.update_queue_item.mockResolvedValue(null);
    await browser.keys('Enter');
    await expect($('.title-editor')).not.toBeExisting();
    assert.ok(mocks.update_queue_item.mock.calls.every(([args]) => args.itemId === itemId(150)));
    await $('button[aria-label="Search downloads"]').click();
    await $('input[aria-label="Search downloads"]').setValue('Virtual item 999');
    await expect($('.queue')).toHaveAttribute('data-queue-count', '1');
    await $('button.title-button=Virtual item 999').click();
    await expect($('.title-editor')).toBeFocused();
    await browser.keys('Escape');
  });
  for (const theme of ['light', 'dark']) {
    for (const [width, height] of [
      [1340, 850],
      [800, 500]
    ]) {
      it(`edits ready and waiting filenames with a single pointer click in ${theme} at ${width}x${height}`, async () => {
        await browser.setViewport({ width, height, devicePixelRatio: scale });
        const seed = sampleQueue();
        seed.queue[3].state = 'queued';
        seed.queue[3].latestOperationId = opId(3);
        seed.operations.push(operation(opId(3), 'download', 'queued', { queueItemId: itemId(3) }));
        const mocks = await start(seed, async (mocks) => {
          await mocks.get_ui_theme.mockResolvedValue(theme);
        });
        for (const [index, title] of [
          [2, 'Piano session No. 4'],
          [3, 'Walking Kyoto at night']
        ]) {
          await $(`button.title-button=${title}`).click();
          await $('.title-editor').waitForDisplayed();
          const selection = await browser.execute(() => ({
            focused: document.activeElement?.classList.contains('title-editor'),
            start: document.activeElement?.selectionStart,
            end: document.activeElement?.selectionEnd,
            value: document.activeElement?.value
          }));
          assert.deepEqual(selection, { focused: true, start: 0, end: title.length, value: title });
          await browser.keys(`Renamed ${index}`);
          await capture(`rename-${theme}-${width}-${index}`);
          if (index === 2) await browser.keys('Enter');
          else await $('h1').click();
          await waitForMockCalls(mocks.update_queue_item, index - 1);
          assert.deepEqual(mocks.update_queue_item.mock.calls[index - 2][0], {
            itemId: itemId(index),
            input: { filenameOverride: `Renamed ${index}` }
          });
          await expect($('.title-editor')).not.toBeExisting();
        }
        await $('button.title-button=Piano session No. 4').click();
        await browser.keys('Discarded draft');
        await browser.keys('Escape');
        await expect($('.title-editor')).not.toBeExisting();
        assert.equal(mocks.update_queue_item.mock.calls.length, 2);
        await noOverflow();
      });
    }
  }
  it('renders both themes at desktop and minimum size, with working filters and search', async () => {
    await browser.setViewport({ width: 1340, height: 850, devicePixelRatio: scale });
    const mocks = await start(sampleQueue());
    await capture('clarity-light');
    await noOverflow();
    await $('.settings-nav').click();
    await $('button=Dark').click();
    await waitForMockCalls(mocks.set_ui_theme, 1);
    assert.deepEqual(mocks.set_ui_theme.mock.calls[0][0], { theme: 'dark' });
    await expect($('html')).toHaveAttribute('data-theme', 'dark');
    await capture('clarity-settings-dark');
    await browser.keys('Escape');
    await capture('clarity-dark');
    await $('button*=Completed').click();
    assert.equal((await $$('tr.queue-item')).length, 1);
    await $('button[aria-label="Show Color and light in folder"]').click();
    await waitForMockCalls(mocks.reveal_download, 1);
    assert.deepEqual(mocks.reveal_download.mock.calls[0][0], { itemId: itemId(4) });
    await $('button*=Queued').click();
    await $('button=Select all').click();
    await $('button=Download selected').click();
    await waitForMockCalls(mocks.enqueue_queue_items, 1);
    assert.deepEqual(mocks.enqueue_queue_items.mock.calls[0][0].itemIds, [itemId(2), itemId(3)]);
    await $('button*=All downloads').click();
    await $('button[aria-label="Search downloads"]').click();
    await $('input[aria-label="Search downloads"]').setValue('piano');
    await expect($('.queue')).toHaveAttribute('data-queue-count', '1');
    await browser.keys('Escape');
    await expect($('.queue')).toHaveAttribute('data-queue-count', '5');
    await browser.setViewport({ width: 800, height: 500, devicePixelRatio: scale });
    await noOverflow();
    await capture('clarity-dark-small');
    await $('.settings-nav').click();
    await $('button=Light').click();
    await expect($('html')).toHaveAttribute('data-theme', 'light');
    await capture('clarity-settings-light-small');
    await browser.keys('Escape');
    await noOverflow();
    await capture('clarity-light-small');
  });

  it('keeps errors out of the main UI and automatically clears and renews the Settings dot', async () => {
    await browser.setViewport({ width: 1340, height: 850, devicePixelRatio: scale });
    const mocks = await start();
    await mocks.begin_inspection.mockRejectedValueOnce(
      'Detailed failure: extractor_fixture_code_77'
    );
    await $('#video-url').setValue(video.url);
    await $('button=Add link').click();
    await $('.notification-dot').waitForDisplayed();
    await expect($('.error-notice')).toHaveText(
      expect.stringContaining('An error occurred. Check Settings')
    );
    assert.ok(!(await $('main').getText()).includes('extractor_fixture_code_77'));
    await capture('clarity-error-notification');
    await $('.settings-nav').click();
    await expect($('.notification-dot')).not.toBeExisting();
    await $('.error-entry summary').click();
    await expect($('.error-history')).toHaveText(
      expect.stringContaining('extractor_fixture_code_77')
    );
    await capture('clarity-errors-settings');
    await browser.keys('Escape');
    await expect($('.notification-dot')).not.toBeExisting();
    await mocks.begin_inspection.mockRejectedValueOnce('Second detailed failure');
    await $('button=Add link').click();
    await $('.notification-dot').waitForDisplayed();
    await $('.settings-nav').click();
    await expect($('.notification-dot')).not.toBeExisting();
    assert.equal((await $$('.error-entry')).length, 2);
    await browser.keys('Escape');
  });

  it('restores a saved dark theme and rolls back failed preference writes', async () => {
    const mocks = await start(initialSnapshot, async (mocks) => {
      await mocks.get_ui_theme.mockResolvedValue('dark');
    });
    await expect($('html')).toHaveAttribute('data-theme', 'dark');
    await $('.settings-nav').click();
    await mocks.set_ui_theme.mockRejectedValueOnce('Preference write was rejected');
    await $('button=Light').click();
    await waitForMockCalls(mocks.set_ui_theme, 1);
    await expect($('html')).toHaveAttribute('data-theme', 'dark');
    await expect($('.settings-dialog')).toHaveText(
      expect.stringContaining('Preference write was rejected')
    );
    await expect($('.notification-dot')).not.toBeExisting();
    await browser.keys('Escape');
    await capture('clarity-empty-dark');
  });
});

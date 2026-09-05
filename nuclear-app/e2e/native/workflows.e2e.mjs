import assert from 'node:assert/strict';
import { readdirSync, statSync } from 'node:fs';
import path from 'node:path';
import { addUrl, waitForTerminalQueueStatus, waitForWorkReady } from './helpers.mjs';

const fixtureUrl = process.env.NUCLEAR_E2E_FIXTURE_URL;
const slowFixtureUrl = process.env.NUCLEAR_E2E_SLOW_FIXTURE_URL;
const fixtureTitle = process.env.NUCLEAR_E2E_FIXTURE_TITLE ?? 'fixture';

describe('real backend fixture lifecycle', () => {
  it('downloads, converts, cancels, reconciles after reload, and clears diagnostics', async () => {
    assert.ok(fixtureUrl, 'NUCLEAR_E2E_FIXTURE_URL is required.');
    assert.ok(slowFixtureUrl, 'NUCLEAR_E2E_SLOW_FIXTURE_URL is required.');

    await waitForWorkReady();
    const outputDirectory = await $('#outdir').getValue();
    const originalFiles = new Set(readdirSync(outputDirectory));
    await $('#format').selectByAttribute('value', 'mp3');
    await addUrl(fixtureUrl);

    const firstRow = (await $$('tr.queue-item'))[0];
    const firstDownload = await firstRow.$('button[aria-label^="Download "]');
    await firstDownload.waitForClickable();
    await firstDownload.click();
    await waitForTerminalQueueStatus(firstRow, 'completed', 4 * 60_000);
    const audioFiles = readdirSync(outputDirectory).filter(
      (name) => !originalFiles.has(name) && name.endsWith('.mp3')
    );
    assert.equal(audioFiles.length, 1, 'Fixture conversion must publish exactly one new MP3.');
    assert.ok(statSync(path.join(outputDirectory, audioFiles[0])).size > 0);

    await $('#format').selectByAttribute('value', 'mp4');
    await addUrl(slowFixtureUrl);
    await browser.waitUntil(async () => (await $$('tr.queue-item')).length === 2, {
      timeout: 60_000,
      timeoutMsg: 'Slow cancellation fixture did not enter the queue.'
    });

    const rows = await $$('tr.queue-item');
    const slowRow = rows[1];
    const slowDownload = await slowRow.$('button[aria-label^="Download "]');
    await slowDownload.waitForClickable();
    await slowDownload.click();
    const cancel = await slowRow.$('button[aria-label^="Cancel "]');
    await cancel.waitForClickable({ timeout: 60_000 });
    await cancel.click();
    await waitForTerminalQueueStatus(slowRow, 'cancelled', 60_000);

    await browser.refresh();
    await $('h1').waitForDisplayed();
    await browser.waitUntil(async () => (await $$('tr.queue-item')).length === 2, {
      timeout: 30_000,
      timeoutMsg: 'Renderer reload did not reconcile the backend-owned queue.'
    });
    assert.match(await $('.queue').getText(), new RegExp(fixtureTitle, 'i'));

    await browser.execute(() => {
      window.confirm = () => true;
    });
    await $('button=Clear Diagnostics').click();
    await expect($('.actions [role="status"]')).toHaveText(expect.stringContaining('diagnostics'));

    const checkUpdates = await $('button=Check for Updates');
    await checkUpdates.waitForClickable({ timeout: 60_000 });
    await checkUpdates.click();
    const updateDialog = await $('[role="dialog"][aria-labelledby="update-modal-title"]');
    await updateDialog.waitForDisplayed();
    await updateDialog.$('button=Close').click();
  });
});

import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { addUrl, editQueuedFilename, waitForWorkReady } from './helpers.mjs';

describe('forced active-process interruption seed', () => {
  it('keeps a real download active until the acceptance runner terminates the app process', async () => {
    const slowFixtureUrl = process.env.NUCLEAR_E2E_SLOW_FIXTURE_URL;
    const sentinelPath = process.env.NUCLEAR_E2E_INTERRUPT_SENTINEL;
    const expectedTitle = process.env.NUCLEAR_E2E_RESTART_TITLE;
    assert.ok(slowFixtureUrl, 'NUCLEAR_E2E_SLOW_FIXTURE_URL is required.');
    assert.ok(sentinelPath, 'NUCLEAR_E2E_INTERRUPT_SENTINEL is required.');
    assert.ok(expectedTitle, 'NUCLEAR_E2E_RESTART_TITLE is required.');

    await waitForWorkReady();
    await $('#format').selectByAttribute('value', 'mp4');
    const row = await addUrl(`${slowFixtureUrl}?case=forced-active-restart`);
    await editQueuedFilename(row, expectedTitle);
    const download = await row.$('button[aria-label^="Download "]');
    await download.waitForClickable({ timeout: 60_000 });
    await download.click();
    const cancel = await row.$('button[aria-label^="Cancel "]');
    await cancel.waitForClickable({
      timeout: 60_000,
      timeoutMsg: 'The forced-restart fixture never entered an active download state.'
    });

    writeFileSync(sentinelPath, expectedTitle, { encoding: 'utf8', flag: 'wx' });
    await new Promise(() => {});
  });
});

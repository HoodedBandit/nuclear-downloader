import assert from 'node:assert/strict';

const fixtureUrl = process.env.NUCLEAR_E2E_FIXTURE_URL;
const slowFixtureUrl = process.env.NUCLEAR_E2E_SLOW_FIXTURE_URL;
const fixtureTitle = process.env.NUCLEAR_E2E_FIXTURE_TITLE ?? 'fixture';

async function addUrl(url) {
  const input = await $('#video-url');
  await input.setValue(url);
  await $('button=Add').click();
  await browser.waitUntil(async () => (await $$('tr.queue-item')).length > 0, {
    timeout: 60_000,
    timeoutMsg: `Fixture URL was not added to the queue: ${url}`
  });
}

describe('real backend fixture lifecycle', () => {
  it('downloads, converts, cancels, reconciles after reload, and clears diagnostics', async () => {
    assert.ok(fixtureUrl, 'NUCLEAR_E2E_FIXTURE_URL is required.');
    assert.ok(slowFixtureUrl, 'NUCLEAR_E2E_SLOW_FIXTURE_URL is required.');

    await $('h1').waitForDisplayed();
    await $('#format').selectByAttribute('value', 'mp3');
    await addUrl(fixtureUrl);

    const firstRow = (await $$('tr.queue-item'))[0];
    const firstDownload = await firstRow.$('button[aria-label^="Download "]');
    await firstDownload.waitForClickable();
    await firstDownload.click();
    const firstStatus = await firstRow.$('.status-pill');
    await firstStatus.waitUntil(async () => (await firstStatus.getText()) === 'completed', {
      timeout: 4 * 60_000,
      timeoutMsg: 'The exact candidate did not complete fixture download and audio conversion.'
    });

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
    const slowStatus = await slowRow.$('.status-pill');
    await slowStatus.waitUntil(async () => (await slowStatus.getText()) === 'cancelled', {
      timeout: 60_000,
      timeoutMsg: 'Cancellation did not reach the backend terminal cancelled state.'
    });

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

    await $('button=Check for Updates').click();
    const updateDialog = await $('[role="dialog"][aria-labelledby="update-modal-title"]');
    await updateDialog.waitForDisplayed();
    await updateDialog.$('button=Close').click();
  });
});

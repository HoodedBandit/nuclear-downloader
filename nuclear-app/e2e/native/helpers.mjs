import assert from 'node:assert/strict';
import { Key } from 'webdriverio';

export async function waitForTerminalQueueStatus(row, expected, timeout) {
  const status = await row.$('.status-pill');
  let actual;
  await status.waitUntil(
    async () => {
      // WebDriver returns rendered text, including CSS text-transform: capitalize.
      actual = (await status.getText()).trim().toLowerCase();
      return ['completed', 'cancelled', 'error', 'interrupted'].includes(actual);
    },
    { timeout, interval: 500, timeoutMsg: `The queue item did not reach terminal ${expected}.` }
  );
  assert.equal(actual, expected, `The queue item ended in ${actual}, expected ${expected}.`);
}

export async function waitForWorkReady() {
  await $('h1').waitForDisplayed();
  // The heading can render before hydration, runtime probes, folder validation,
  // and snapshot/listener initialization. A click on disabled Add is a no-op.
  await $('button=Add').waitForClickable({
    timeout: 60_000,
    timeoutMsg: 'The application did not enable work after startup.'
  });
}

export async function waitForQueueCount(expected, timeout = 60_000) {
  await browser.waitUntil(async () => (await $$('tr.queue-item')).length === expected, {
    timeout,
    interval: 250,
    timeoutMsg: `The queue did not reach exactly ${expected} items.`
  });
}

async function submitReadyUrl(url) {
  const input = await $('#video-url');
  await input.setValue(url);
  const add = await $('button=Add');
  await add.waitForClickable({ timeout: 60_000 });
  await add.click();
}

export async function submitUrl(url) {
  await waitForWorkReady();
  await submitReadyUrl(url);
}

export async function addUrl(url) {
  await waitForWorkReady();
  const previousCount = (await $$('tr.queue-item')).length;
  await submitReadyUrl(url);
  await waitForQueueCount(previousCount + 1);
  return (await $$('tr.queue-item'))[previousCount];
}

export async function editQueuedFilename(row, filename) {
  const edit = await row.$('button.title-button');
  await edit.waitForClickable({ timeout: 30_000 });
  await edit.click();
  const input = await row.$('input[aria-label="Edit queued filename"]');
  await input.waitForDisplayed({ timeout: 30_000 });
  await input.click();
  await browser.keys([Key.Ctrl, 'a']);
  await browser.keys(filename);
  await browser.keys('Enter');
  await row.$('.title-text').waitForDisplayed({ timeout: 30_000 });
}

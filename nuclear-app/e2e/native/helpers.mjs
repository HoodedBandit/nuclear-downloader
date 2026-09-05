import assert from 'node:assert/strict';

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

export async function addUrl(url) {
  await waitForWorkReady();
  const previousCount = (await $$('tr.queue-item')).length;
  const input = await $('#video-url');
  await input.setValue(url);
  const add = await $('button=Add');
  await add.waitForClickable({ timeout: 60_000 });
  await add.click();
  await browser.waitUntil(async () => (await $$('tr.queue-item')).length === previousCount + 1, {
    timeout: 60_000,
    timeoutMsg: 'The fixture inspection did not add exactly one queue item.'
  });
}

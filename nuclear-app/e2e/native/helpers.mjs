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

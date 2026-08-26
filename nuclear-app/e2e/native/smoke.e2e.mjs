import assert from 'node:assert/strict';

describe('exact Nuclear Downloader executable', () => {
  it('starts the real Tauri window and exposes a usable renderer', async () => {
    const heading = await $('h1');
    await heading.waitForDisplayed();
    assert.equal(await heading.getText(), 'Nuclear Downloader');

    await expect($('#video-url')).toBeDisplayed();
    const runtimeStatus = $('[data-testid="runtime-status"]');
    await browser.waitUntil(async () => (await runtimeStatus.getText()).includes('Runtime ready'), {
      timeout: 45_000,
      timeoutMsg: `Packaged runtime did not become ready: ${await runtimeStatus.getAttribute('title')}`
    });
    await expect($('button=Check Runtime')).toBeDisplayed();
    await expect($('button=Check for Updates')).toBeDisplayed();
    await expect($('#outdir')).toHaveValue(expect.stringMatching(/\S/));
    assert.doesNotMatch(await $('#outdir').getValue(), /^\\\\\?\\/);
    await expect($('button=Add')).toBeEnabled();
  });
});

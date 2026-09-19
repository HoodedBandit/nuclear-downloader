import assert from 'node:assert/strict';
import { waitForWorkReady } from './helpers.mjs';

describe('exact Nuclear Downloader executable', () => {
  it('starts the real Tauri window and exposes a usable renderer', async () => {
    await waitForWorkReady();
    assert.equal(await $('h1').getText(), 'Downloads');

    await expect($('#video-url')).toBeDisplayed();
    const outputDirectory = await $('#outdir').getAttribute('title');
    assert.match(outputDirectory, /\S/);
    assert.doesNotMatch(outputDirectory, /^\\\\\?\\/);
    await expect($('button=Add link')).toBeEnabled();
    await $('.settings-nav').click();
    const runtimeStatus = $('[data-testid="runtime-status"]');
    await runtimeStatus.scrollIntoView({ block: 'center' });
    await browser.waitUntil(async () => (await runtimeStatus.getText()).includes('Runtime ready'), {
      timeout: 45_000,
      timeoutMsg: `Packaged runtime did not become ready: ${await runtimeStatus.getAttribute('title')}`
    });
    await expect($('button=Check Runtime')).toBeDisplayed();
    await expect($('button=Check for Updates')).toBeDisplayed();
    await browser.keys('Escape');
  });
});

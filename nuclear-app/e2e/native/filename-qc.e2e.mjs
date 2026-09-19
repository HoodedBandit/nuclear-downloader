import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { addUrl, waitForWorkReady, waitForTerminalQueueStatus } from './helpers.mjs';

const prefix = process.env.NUCLEAR_QC_FILENAME_PREFIX;
const outputRoot = process.env.NUCLEAR_QC_RESULTS;
const restarting = process.env.NUCLEAR_QC_RESTART === '1';
const names = Array.from({ length: 6 }, (_, i) => `${prefix}-${i}`);
const waitingName = `${prefix}-waiting-renamed`;
const hash = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');

async function rename(row, name) {
  await row.$('.title-button').click();
  await row.$('.title-editor').waitForDisplayed();
  const focused = await browser.execute(() => {
    const input = document.activeElement;
    return (
      input?.classList.contains('title-editor') &&
      input.selectionStart === 0 &&
      input.selectionEnd === input.value.length
    );
  });
  assert.ok(focused, 'One pointer click must focus and select the whole filename.');
  await browser.keys(name);
  await browser.keys('Enter');
  await expect(row.$('.title-text')).toHaveText(name);
}

describe('native preview filename quality check', () => {
  it(
    restarting
      ? 'restores renamed files and theme after restarting the actual executable'
      : 'publishes exact local fixture bytes under ready and waiting renames',
    async () => {
      assert.ok(prefix && /^nuclear-qc-[a-z0-9-]+$/.test(prefix));
      assert.ok(outputRoot && path.isAbsolute(outputRoot));
      await waitForWorkReady();
      const directory = await $('#outdir').getAttribute('title');
      const publishedNames = [...names.slice(0, 5), waitingName];
      if (restarting) {
        await expect($('html')).toHaveAttribute('data-theme', 'dark');
        for (const name of publishedNames) await expect($(`.title-text=${name}`)).toBeDisplayed();
      } else {
        assert.equal(
          (await $$('tr.queue-item')).length,
          0,
          'Native QC requires a fresh isolated preview profile.'
        );
        await $('#format').selectByAttribute('value', 'mp4');
        for (let i = 0; i < 6; i += 1) {
          const row = await addUrl(`${process.env.NUCLEAR_E2E_SLOW_FIXTURE_URL}?qc=${prefix}-${i}`);
          await rename(row, names[i]);
        }
        await $('button=Download queued').click();
        const rows = await $$('tr.queue-item');
        await browser.waitUntil(
          async () =>
            (await rows[0].$('.col-status').getText()).toLowerCase().includes('downloading'),
          { timeout: 30_000 }
        );
        await expect(rows[5].$('.col-status')).toHaveText(expect.stringContaining('Queued'));
        await rename(rows[5], waitingName);
        await $('.settings-nav').click();
        await $('button=Dark').click();
        await expect($('html')).toHaveAttribute('data-theme', 'dark');
        await browser.keys('Escape');
        await browser.saveScreenshot(path.join(outputRoot, 'native-waiting-rename-dark.png'));
        for (const row of rows) await waitForTerminalQueueStatus(row, 'completed', 5 * 60_000);
      }
      const fixtureHash = hash(process.env.NUCLEAR_E2E_FIXTURE_FILE);
      const files = publishedNames.map((name) => {
        const file = path.join(directory, `${name}.mp4`);
        const sha256 = hash(file);
        assert.equal(sha256, fixtureHash, `${name} must contain the exact locally served media.`);
        return { path: file, sha256 };
      });
      await browser.saveScreenshot(
        path.join(outputRoot, restarting ? 'native-restart.png' : 'native-completed.png')
      );
      writeFileSync(
        path.join(outputRoot, restarting ? 'restart.json' : 'downloads.json'),
        JSON.stringify(
          {
            executable: process.env.NUCLEAR_E2E_APP_BINARY,
            files,
            fixtureHash,
            restarted: restarting
          },
          null,
          2
        )
      );
    }
  );
});

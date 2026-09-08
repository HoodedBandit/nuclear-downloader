import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';
import {
  addUrl,
  editQueuedFilename,
  submitUrl,
  waitForQueueCount,
  waitForTerminalQueueStatus,
  waitForWorkReady
} from './helpers.mjs';

const fixtureUrl = process.env.NUCLEAR_E2E_FIXTURE_URL;
const slowFixtureUrl = process.env.NUCLEAR_E2E_SLOW_FIXTURE_URL;
const playlistFixtureUrl = process.env.NUCLEAR_E2E_PLAYLIST_FIXTURE_URL;
const fixtureFile = process.env.NUCLEAR_E2E_FIXTURE_FILE;
const fixtureTitle = process.env.NUCLEAR_E2E_FIXTURE_TITLE ?? 'fixture';
const collisionStem = process.env.NUCLEAR_E2E_COLLISION_STEM;
const controlledSiteFixtures = [
  {
    caseId: 'youtube-maintainer-fixture',
    fixtureId: process.env.NUCLEAR_E2E_YOUTUBE_FIXTURE_ID,
    url: process.env.NUCLEAR_E2E_YOUTUBE_FIXTURE_URL
  },
  {
    caseId: 'x-maintainer-fixture',
    fixtureId: process.env.NUCLEAR_E2E_X_FIXTURE_ID,
    url: process.env.NUCLEAR_E2E_X_FIXTURE_URL
  }
].filter((fixture) => fixture.fixtureId && fixture.url);

function newFiles(outputDirectory, before, extension) {
  return readdirSync(outputDirectory).filter(
    (name) => !before.has(name) && name.toLowerCase().endsWith(extension)
  );
}

function sha256(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

function assertPublishedFixture(outputDirectory, filename) {
  const published = path.join(outputDirectory, filename);
  assert.ok(statSync(published).size > 0, `${filename} must contain published media bytes.`);
  assert.equal(
    sha256(published),
    sha256(fixtureFile),
    `${filename} must contain the exact loopback MP4 fixture bytes.`
  );
}

async function startRow(row) {
  const download = await row.$('button[aria-label^="Download "]');
  await download.waitForClickable({ timeout: 60_000 });
  await download.click();
}

describe('real backend fixture lifecycle', () => {
  let outputDirectory;

  before(async () => {
    assert.ok(fixtureUrl, 'NUCLEAR_E2E_FIXTURE_URL is required.');
    assert.ok(slowFixtureUrl, 'NUCLEAR_E2E_SLOW_FIXTURE_URL is required.');
    assert.ok(playlistFixtureUrl, 'NUCLEAR_E2E_PLAYLIST_FIXTURE_URL is required.');
    assert.ok(fixtureFile && path.isAbsolute(fixtureFile), 'NUCLEAR_E2E_FIXTURE_FILE is required.');
    assert.ok(collisionStem, 'NUCLEAR_E2E_COLLISION_STEM is required.');
    await waitForWorkReady();
    outputDirectory = await $('#outdir').getValue();
  });

  it('publishes the exact successful MP4 fixture', async () => {
    const before = new Set(readdirSync(outputDirectory));
    await $('#format').selectByAttribute('value', 'mp4');
    const row = await addUrl(`${fixtureUrl}?case=successful-mp4`);
    await startRow(row);
    await waitForTerminalQueueStatus(row, 'completed', 4 * 60_000);

    const videoFiles = newFiles(outputDirectory, before, '.mp4');
    assert.equal(videoFiles.length, 1, 'Successful MP4 download must publish exactly one file.');
    assertPublishedFixture(outputDirectory, videoFiles[0]);
  });

  it('retries the same cancelled queue item to completion', async () => {
    const before = new Set(readdirSync(outputDirectory));
    const row = await addUrl(`${slowFixtureUrl}?case=cancel-retry`);
    await startRow(row);

    const cancel = await row.$('button[aria-label^="Cancel "]');
    await cancel.waitForClickable({ timeout: 60_000 });
    await cancel.click();
    await waitForTerminalQueueStatus(row, 'cancelled', 60_000);

    const retry = await row.$('button=Retry');
    await retry.waitForClickable({ timeout: 60_000 });
    await retry.click();
    await row.$('button[aria-label^="Cancel "]').waitForExist({
      timeout: 60_000,
      timeoutMsg: 'Retry did not start a new operation for the cancelled queue item.'
    });
    await waitForTerminalQueueStatus(row, 'completed', 5 * 60_000);

    const videoFiles = newFiles(outputDirectory, before, '.mp4');
    assert.equal(videoFiles.length, 1, 'Cancelled-item retry must publish exactly one MP4.');
    assertPublishedFixture(outputDirectory, videoFiles[0]);
  });

  it('allocates collision suffixes without replacing either output', async () => {
    const first = await addUrl(`${fixtureUrl}?case=collision-one`);
    const second = await addUrl(`${fixtureUrl}?case=collision-two`);
    await editQueuedFilename(first, collisionStem);
    await editQueuedFilename(second, collisionStem);

    await startRow(first);
    await startRow(second);
    await waitForTerminalQueueStatus(first, 'completed', 4 * 60_000);
    await waitForTerminalQueueStatus(second, 'completed', 4 * 60_000);

    assertPublishedFixture(outputDirectory, `${collisionStem}.mp4`);
    assertPublishedFixture(outputDirectory, `${collisionStem} (2).mp4`);
  });

  it('discovers both entries from the validated loopback generic playlist', async () => {
    const previousCount = (await $$('tr.queue-item')).length;
    await submitUrl(playlistFixtureUrl);
    const dialog = await $('[role="dialog"][aria-labelledby="playlist-modal-title"]');
    await dialog.waitForDisplayed({ timeout: 60_000 });
    assert.match(await dialog.getText(), /Nuclear Generic Playlist/i);
    assert.equal((await dialog.$$('.playlist-entry')).length, 2);
    const addSelection = await dialog.$('button=Add 2 Videos to Queue');
    await addSelection.waitForClickable({ timeout: 30_000 });
    await addSelection.click();
    await waitForQueueCount(previousCount + 2, 2 * 60_000);
  });

  it('cancels all active work and reopens admission', async () => {
    const first = await addUrl(`${slowFixtureUrl}?case=cancel-all-one`);
    const second = await addUrl(`${slowFixtureUrl}?case=cancel-all-two`);
    await startRow(first);
    await startRow(second);

    const cancelAll = await $('button=Cancel All');
    await cancelAll.waitForClickable({ timeout: 60_000 });
    await cancelAll.click();
    await waitForTerminalQueueStatus(first, 'cancelled', 90_000);
    await waitForTerminalQueueStatus(second, 'cancelled', 90_000);
    await $('button=Add').waitForClickable({
      timeout: 60_000,
      timeoutMsg: 'Admission did not reopen after Cancel All drained.'
    });
  });

  for (const fixture of controlledSiteFixtures) {
    it(`downloads the configured ${fixture.caseId} through the installed UI`, async () => {
      const before = new Set(readdirSync(outputDirectory));
      const row = await addUrl(fixture.url);
      const format = await row.$('select[aria-label^="Format for "]');
      await format.selectByAttribute('value', 'mp4');
      await editQueuedFilename(row, `site-${fixture.fixtureId}`);
      await startRow(row);
      await waitForTerminalQueueStatus(row, 'completed', 10 * 60_000);

      const published = readdirSync(outputDirectory).filter((name) => {
        if (before.has(name)) return false;
        return statSync(path.join(outputDirectory, name)).isFile();
      });
      assert.equal(
        published.length,
        1,
        `${fixture.caseId} must publish exactly one new media file.`
      );
      assert.ok(statSync(path.join(outputDirectory, published[0])).size > 0);
    });
  }

  it('reconciles after reload and exercises runtime, diagnostics, and update checks', async () => {
    const expectedCount = (await $$('tr.queue-item')).length;
    await browser.refresh();
    await $('h1').waitForDisplayed();
    await waitForQueueCount(expectedCount, 30_000);
    assert.match(await $('.queue').getText(), new RegExp(fixtureTitle, 'i'));

    const runtimeCheck = await $('button=Check Runtime');
    await runtimeCheck.waitForClickable({ timeout: 60_000 });
    await runtimeCheck.click();
    await runtimeCheck.waitUntil(async () => (await runtimeCheck.getText()) === 'Check Runtime', {
      timeout: 60_000,
      timeoutMsg: 'Runtime repair/readiness check did not settle.'
    });
    assert.match(await $('[data-testid="runtime-status"]').getText(), /Runtime ready/i);

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

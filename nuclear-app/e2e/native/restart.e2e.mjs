import assert from 'node:assert/strict';

describe('installed application process restart', () => {
  it('restores the backend-owned queue journal in a new process', async () => {
    const expectedTitle = process.env.NUCLEAR_E2E_RESTART_TITLE;
    assert.ok(expectedTitle, 'NUCLEAR_E2E_RESTART_TITLE is required.');

    await $('h1').waitForDisplayed();
    const queue = await $('.queue');
    await queue.waitUntil(async () => (await queue.getText()).includes(expectedTitle), {
      timeout: 30_000,
      timeoutMsg: `Persisted queue item ${expectedTitle} did not return after process restart.`
    });
  });
});

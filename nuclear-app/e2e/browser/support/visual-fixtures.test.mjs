import assert from 'node:assert/strict';
import test from 'node:test';
import { pngDimensions } from './png-contract.mjs';
import { visualStates } from './visual-fixtures.mjs';

test('visual state fixtures project the intended renderer states', () => {
  assert.deepEqual(Object.keys(visualStates), [
    'empty',
    'populated',
    'downloading',
    'converting',
    'cancelled',
    'failed',
    'interrupted',
    'persistence-degraded',
    'playlist-modal',
    'update-modal'
  ]);

  const empty = visualStates.empty();
  assert.equal(empty.queue.length, 0);
  assert.equal(empty.operations.length, 0);

  const populated = visualStates.populated();
  assert.equal(populated.queue[0].state, 'inert');
  assert.equal(populated.operations.length, 0);

  const expectedOperations = {
    downloading: ['running', 'download'],
    converting: ['running', 'conversion'],
    cancelled: ['cancelled', 'download'],
    failed: ['failed', 'download'],
    interrupted: ['interrupted', 'download']
  };
  for (const [id, [state, phase]] of Object.entries(expectedOperations)) {
    const snapshot = visualStates[id]();
    assert.equal(snapshot.queue[0].state, state);
    assert.equal(snapshot.queue[0].latestOperationId, snapshot.operations[0].id);
    assert.equal(snapshot.operations[0].state, state);
    assert.equal(snapshot.operations[0].phase, phase);
  }
  assert.equal(visualStates.converting().queue[0].format, 'webm');
  assert.equal(visualStates.cancelled().operations[0].error, null);
  assert.equal(visualStates.failed().operations[0].error.code, 'fixture_download_failed');
  assert.equal(visualStates.interrupted().operations[0].error.code, 'fixture_interrupted');

  const degraded = visualStates['persistence-degraded']();
  assert.equal(degraded.persistenceHealth.degraded, true);
  assert.equal(degraded.persistenceHealth.error.code, 'fixture_persistence');

  for (const id of ['playlist-modal', 'update-modal']) {
    const snapshot = visualStates[id]();
    assert.equal(snapshot.queue.length, 0, `${id} must begin clean before its real UI action.`);
    assert.equal(
      snapshot.operations.length,
      0,
      `${id} must begin clean before its real UI action.`
    );
  }
});

test('PNG dimensions require a signature and leading IHDR chunk', () => {
  const png = Buffer.alloc(24);
  Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]).copy(png);
  png.writeUInt32BE(13, 8);
  png.write('IHDR', 12, 'ascii');
  png.writeUInt32BE(1200, 16);
  png.writeUInt32BE(750, 20);
  assert.deepEqual(pngDimensions(png), { width: 1200, height: 750 });
  assert.throws(() => pngDimensions(Buffer.from('not a png')), /too short|PNG signature/);
  const wrongChunk = Buffer.from(png);
  wrongChunk.write('IDAT', 12, 'ascii');
  assert.throws(() => pngDimensions(wrongChunk), /valid PNG IHDR/);
});

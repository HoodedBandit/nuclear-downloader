import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { after, before, test } from 'node:test';

import { performanceClockIsolation, performanceClockPath } from './performance-clock.mjs';

let server;
let origin;

before(async () => {
  let middleware;
  performanceClockIsolation().configureServer({
    middlewares: {
      use(value) {
        middleware = value;
      }
    }
  });
  assert.equal(typeof middleware, 'function');

  server = createServer((request, response) => {
    middleware(request, response, () => {
      response.statusCode = 200;
      response.end('ok');
    });
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  assert.ok(address && typeof address === 'object');
  origin = `http://127.0.0.1:${address.port}`;
});

after(async () => {
  if (!server) return;
  await new Promise((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
  });
});

async function headersFor(route) {
  const response = await fetch(`${origin}${route}`);
  assert.equal(await response.text(), 'ok');
  return response.headers;
}

test('adds isolation headers only to the exact performance document route', async () => {
  const headers = await headersFor(performanceClockPath);
  assert.equal(headers.get('cross-origin-opener-policy'), 'same-origin');
  assert.equal(headers.get('cross-origin-embedder-policy'), 'require-corp');
});

test('does not add isolation headers to the ordinary renderer document', async () => {
  const headers = await headersFor('/');
  assert.equal(headers.get('cross-origin-opener-policy'), null);
  assert.equal(headers.get('cross-origin-embedder-policy'), null);
});

test('does not add isolation headers to renderer resources', async () => {
  const headers = await headersFor('/src/routes/+page.svelte?nuclear-performance-clock=isolated');
  assert.equal(headers.get('cross-origin-opener-policy'), null);
  assert.equal(headers.get('cross-origin-embedder-policy'), null);
});

test('ignores non-exact performance clock flags', async () => {
  for (const route of [
    '/?nuclear-performance-clock=other',
    '/?nuclear-performance-clock=isolated&extra=true',
    '/?extra=true&nuclear-performance-clock=isolated'
  ]) {
    const headers = await headersFor(route);
    assert.equal(headers.get('cross-origin-opener-policy'), null);
    assert.equal(headers.get('cross-origin-embedder-policy'), null);
  }
});

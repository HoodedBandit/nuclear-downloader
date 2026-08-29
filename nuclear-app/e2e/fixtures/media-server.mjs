import { createReadStream, statSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import path from 'node:path';

const [mediaArgument, readyArgument] = process.argv.slice(2);
if (!mediaArgument || !readyArgument) {
  throw new Error('Usage: node media-server.mjs <media-file> <ready-file>');
}

const mediaPath = path.resolve(mediaArgument);
const readyPath = path.resolve(readyArgument);
const mediaSize = statSync(mediaPath).size;

function writeHeaders(response) {
  response.writeHead(200, {
    'Content-Type': 'video/mp4',
    'Content-Length': mediaSize,
    'Content-Disposition': 'inline; filename="fixture-video.mp4"',
    'Cache-Control': 'no-store',
    Connection: 'close'
  });
}

const server = http.createServer((request, response) => {
  if (request.url === '/health') {
    response.writeHead(200, { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' });
    response.end('ok');
    return;
  }
  if (request.url !== '/fixture-video.mp4' && request.url !== '/slow-fixture-video.mp4') {
    response.writeHead(404, { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' });
    response.end('not found');
    return;
  }

  writeHeaders(response);
  if (request.method === 'HEAD') {
    response.end();
    return;
  }
  if (request.method !== 'GET') {
    response.destroy();
    return;
  }

  const stream = createReadStream(mediaPath, {
    highWaterMark: request.url.startsWith('/slow-') ? 32 * 1024 : 1024 * 1024
  });
  let timer;
  stream.on('data', () => {
    if (!request.url.startsWith('/slow-')) return;
    stream.pause();
    timer = setTimeout(() => stream.resume(), 150);
  });
  stream.on('error', () => response.destroy());
  response.on('close', () => {
    if (timer) clearTimeout(timer);
    stream.destroy();
  });
  stream.pipe(response);
});

server.listen(0, '127.0.0.1', () => {
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Could not bind fixture server.');
  writeFileSync(readyPath, String(address.port), { encoding: 'ascii', flag: 'wx' });
});

function shutdown() {
  server.closeAllConnections?.();
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(1), 5_000).unref();
}

process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);

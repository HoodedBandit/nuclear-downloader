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

function writeHeaders(response, filename = 'fixture-video.mp4') {
  response.writeHead(200, {
    'Content-Type': 'video/mp4',
    'Content-Length': mediaSize,
    'Content-Disposition': `inline; filename="${filename}"`,
    'Cache-Control': 'no-store',
    Connection: 'close'
  });
}

const server = http.createServer((request, response) => {
  const requestUrl = new URL(request.url ?? '/', 'http://127.0.0.1');
  if (requestUrl.pathname === '/health') {
    response.writeHead(200, { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' });
    response.end('ok');
    return;
  }
  if (requestUrl.pathname === '/generic-playlist.html') {
    const origin = `http://127.0.0.1:${server.address().port}`;
    const html = `<!doctype html>
<html lang="en">
  <head><meta charset="utf-8"><title>Nuclear Generic Playlist</title></head>
  <body>
    <h1>Nuclear Generic Playlist</h1>
    <video controls title="Nuclear playlist entry one">
      <source src="${origin}/playlist-one.mp4" type="video/mp4">
    </video>
    <video controls title="Nuclear playlist entry two">
      <source src="${origin}/playlist-two.mp4" type="video/mp4">
    </video>
  </body>
</html>`;
    response.writeHead(200, {
      'Content-Type': 'text/html; charset=utf-8',
      'Content-Length': Buffer.byteLength(html),
      'Cache-Control': 'no-store',
      Connection: 'close'
    });
    response.end(request.method === 'HEAD' ? undefined : html);
    return;
  }

  const mediaNames = new Map([
    ['/fixture-video.mp4', 'fixture-video.mp4'],
    ['/slow-fixture-video.mp4', 'slow-fixture-video.mp4'],
    ['/playlist-one.mp4', 'playlist-one.mp4'],
    ['/playlist-two.mp4', 'playlist-two.mp4']
  ]);
  const mediaName = mediaNames.get(requestUrl.pathname);
  if (!mediaName) {
    response.writeHead(404, { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' });
    response.end('not found');
    return;
  }

  writeHeaders(response, mediaName);
  if (request.method === 'HEAD') {
    response.end();
    return;
  }
  if (request.method !== 'GET') {
    response.destroy();
    return;
  }

  const stream = createReadStream(mediaPath, {
    highWaterMark: requestUrl.pathname.startsWith('/slow-') ? 32 * 1024 : 1024 * 1024
  });
  let timer;
  stream.on('data', () => {
    if (!requestUrl.pathname.startsWith('/slow-')) return;
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

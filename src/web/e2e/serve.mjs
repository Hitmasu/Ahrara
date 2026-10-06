// Serves the browser app like its static host: _headers on every file and
// index.html for any path without a file.
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';

const root = new URL('../../../.dev/web-e2e/', import.meta.url).pathname;
const types = {
  '.css': 'text/css; charset=utf-8',
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.png': 'image/png',
  '.wasm': 'application/wasm',
};
const headers = Object.fromEntries(
  (await readFile(join(root, '_headers'), 'utf8'))
    .split('\n')
    .filter(line => line.startsWith('  '))
    .map(line => line.trim().split(/: (.*)/s).slice(0, 2)),
);

createServer(async (request, response) => {
  const path = normalize(decodeURIComponent(new URL(request.url, 'http://host').pathname));
  let file = join(root, path.endsWith('/') ? `${path}index.html` : path);
  let body = await readFile(file).catch(() => undefined);
  if (!body) body = await readFile((file = join(root, 'index.html')));
  response.writeHead(200, { ...headers, 'Content-Type': types[extname(file)] ?? 'application/octet-stream' });
  response.end(body);
}).listen(28080, '127.0.0.1');

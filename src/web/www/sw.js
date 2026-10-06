'use strict';
// Answers the app's pages as the CLI's local server would, from this browser's data.
importScripts('/app/store.js', '/app/ahrara_web.js');
const ready = wasm_bindgen({ module_or_path: '/app/ahrara_web_bg.wasm' });
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));
self.addEventListener('fetch', event => {
  const request = event.request;
  const url = new URL(request.url);
  // The app's own files come from the host.
  if (url.origin !== location.origin || url.pathname === '/sw.js' || url.pathname.startsWith('/app/') || url.pathname.startsWith('/assets/')) return;
  // A visit always opens the shell, which shows the page in its frame. Forms
  // only post from that frame, with the page's token.
  if (request.mode === 'navigate' && request.destination === 'document') {
    event.respondWith(request.method === 'GET' ? fetch('/') : new Response(null, { status: 405 }));
    return;
  }
  url.hash = '';
  event.respondWith(answer(request, url.href).catch(error => {
    console.error(error);
    return new Response('Ahrara could not open this page. Reload to try again.', { status: 500, headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
  }));
});
async function answer(request, url) {
  await ready;
  const reply = await wasm_bindgen.respond(request.method, url, new Uint8Array(await request.arrayBuffer()), navigator.languages.join(','));
  const target = reply.headers.find(([name]) => name === 'location');
  if (target) return Response.redirect(new URL(target[1], request.url), reply.status);
  return new Response(reply.body, { status: reply.status, headers: reply.headers });
}

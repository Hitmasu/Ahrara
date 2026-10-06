'use strict';
// The shell never navigates. It keeps the relay connection in a worker and shows
// the pages, which the Service Worker answers, in a same-origin frame.
let worker;
const start = () => { worker ??= new Worker('/app/worker.js'); };
// A frozen page would hold the connection lock; another tab takes over instead.
const stop = () => { worker?.terminate(); worker = undefined; };
document.addEventListener('freeze', stop);
document.addEventListener('resume', start);
addEventListener('pagehide', stop);
addEventListener('pageshow', start);

// Mirror the page so that reloading, bookmarks and the title keep working.
const frame = document.querySelector('iframe');
frame.addEventListener('load', () => {
  const page = frame.contentWindow.location;
  if (page.origin !== location.origin) return;
  const path = page.pathname + page.search + page.hash;
  if (path !== location.pathname + location.search + location.hash) history.replaceState(null, '', path);
  document.title = frame.contentDocument.title;
});

(async () => {
  await navigator.serviceWorker.register('/sw.js');
  if (!navigator.serviceWorker.controller) {
    await new Promise(resolve => navigator.serviceWorker.addEventListener('controllerchange', resolve, { once: true }));
  }
  // Replacing the frame's blank first page keeps it out of the history.
  frame.contentWindow.location.replace(location.pathname + location.search + location.hash);
})().catch(() => {
  frame.hidden = true;
  document.querySelector('.unsupported').hidden = false;
});

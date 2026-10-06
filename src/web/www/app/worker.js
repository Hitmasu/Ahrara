'use strict';
// Holds this browser's one relay connection. Tabs elect the holder with a lock;
// when its tab closes, another tab connects.
importScripts('/app/store.js', '/app/ahrara_web.js');
const events = new BroadcastChannel('ahrara');
let identityChanged = () => {};
events.addEventListener('message', ({ data }) => { if (data.kind === 'identity_changed') identityChanged(); });
const changed = () => new Promise(resolve => { identityChanged = resolve; });
const sleep = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));

navigator.locks.request('ahrara-connection', async () => {
  await wasm_bindgen({ module_or_path: '/app/ahrara_web_bg.wasm' });
  for (let delay = 1000; ;) {
    let result;
    try {
      const engine = await wasm_bindgen.Engine.open();
      if (!engine) { await changed(); continue; }
      result = await connect(engine);
      engine.free();
    } catch (error) {
      result = { error: String(error?.message ?? error) };
    }
    await wasm_bindgen.connection(result.error === 'identity_in_use' ? 'identity_in_use' : 'disconnected', result.error ?? null).catch(() => {});
    if (result.changed) continue;
    // identity_in_use is retried too: after a reload the relay may still hold the previous socket.
    delay = result.ready ? 1000 : Math.min(delay * 2, 30000);
    await Promise.race([sleep(delay * (0.5 + Math.random() / 2)), changed()]);
  }
});

function connect(engine) {
  return new Promise(resolve => {
    const socket = new WebSocket(wasm_bindgen.relay_url());
    socket.binaryType = 'arraybuffer';
    let queue = Promise.resolve();
    let heard = Date.now();
    let closed = false;
    const finish = (error, changed = false) => {
      if (closed) return;
      closed = true;
      clearInterval(keepalive);
      clearTimeout(handshake);
      identityChanged = () => {};
      socket.close();
      resolve({ error, ready: engine.ready(), changed });
    };
    identityChanged = () => finish(undefined, true);
    // Calls run in order: each reply's encryption nonce follows the previous one.
    const enqueue = call => {
      queue = queue.then(call)
        .then(reply => { if (reply && !closed) socket.send(reply); })
        .catch(error => finish(String(error?.message ?? error)));
    };
    socket.onopen = () => {
      wasm_bindgen.connection('connecting', null);
      socket.send(engine.hello());
    };
    socket.onmessage = ({ data }) => {
      heard = Date.now();
      enqueue(() => engine.receive(new Uint8Array(data)));
    };
    socket.onclose = () => finish('connection closed');
    const handshake = setTimeout(() => { if (!engine.ready()) finish('handshake timeout'); }, 15000);
    // Browsers cannot send WebSocket pings; the relay echoes this frame instead.
    const keepalive = setInterval(() => {
      if (Date.now() - heard > 90000) finish('relay stopped answering');
      else if (engine.ready()) enqueue(() => engine.keepalive());
    }, 30000);
  });
}

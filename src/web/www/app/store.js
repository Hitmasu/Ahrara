'use strict';
// IndexedDB for the browser app. The wasm module seals every record before it
// gets here. Each call is one transaction and settles after it commits.
globalThis.ahraraStore = (() => {
  const stores = ['kv', 'addresses', 'messages', 'raw'];
  const events = new BroadcastChannel('ahrara');
  let database;
  function open() {
    database ??= new Promise((resolve, reject) => {
      const request = indexedDB.open('ahrara', 1);
      request.onupgradeneeded = () => {
        const db = request.result;
        db.createObjectStore('kv');
        db.createObjectStore('addresses', { keyPath: 'id', autoIncrement: true });
        db.createObjectStore('messages', { keyPath: 'id', autoIncrement: true })
          .createIndex('delivery', 'delivery', { unique: true });
        db.createObjectStore('raw');
      };
      request.onsuccess = () => {
        request.result.onversionchange = () => { request.result.close(); database = undefined; };
        resolve(request.result);
      };
      request.onerror = () => { database = undefined; reject(request.error); };
    });
    return database;
  }
  async function run(names, mode, body) {
    const transaction = (await open()).transaction(names, mode, { durability: 'strict' });
    return new Promise((resolve, reject) => {
      let result;
      transaction.oncomplete = () => resolve(result);
      transaction.onabort = () => reject(transaction.error ?? new DOMException('Transaction aborted', 'AbortError'));
      body(transaction, value => { result = value; });
    });
  }
  const one = (name, mode, call) => run([name], mode, (transaction, done) => {
    call(transaction.objectStore(name)).onsuccess = event => done(event.target.result);
  });
  return Object.freeze({
    get: (name, key) => one(name, 'readonly', store => store.get(key)),
    all: name => one(name, 'readonly', store => store.getAll()),
    count: name => one(name, 'readonly', store => store.count()),
    put: (name, value, key) => one(name, 'readwrite', store => key === undefined ? store.put(value) : store.put(value, key)),
    // Rejects when the key exists.
    add: (name, value, key) => one(name, 'readwrite', store => store.add(value, key)),
    remove: (names, key) => run(names, 'readwrite', transaction => names.forEach(name => transaction.objectStore(name).delete(key))),
    deliver: entries => run(['messages', 'raw'], 'readwrite', (transaction, done) => {
      const ids = entries.map(() => null);
      done(ids);
      entries.forEach(({ delivery, sealed, raw }, index) => {
        const request = transaction.objectStore('messages').add({ delivery, sealed });
        request.onsuccess = () => {
          ids[index] = request.result;
          transaction.objectStore('raw').put(raw, request.result);
        };
        // A repeated transfer is already stored; keep the rest of the transaction.
        request.onerror = event => { if (request.error.name === 'ConstraintError') event.preventDefault(); };
      });
    }),
    erase: () => run(stores, 'readwrite', transaction => stores.forEach(name => transaction.objectStore(name).clear())),
    emit: (kind, data) => events.postMessage({ kind, data: JSON.parse(data) }),
  });
})();

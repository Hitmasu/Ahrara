'use strict';
// The page's language, from its data-strings attribute.
const strings = JSON.parse(document.body.dataset.strings ?? '{}');
const notice = document.querySelector('#notice');
let noticeTimer;
function notify(text) {
  if (!notice) return;
  notice.textContent = text;
  notice.hidden = false;
  clearTimeout(noticeTimer);
  noticeTimer = setTimeout(() => { notice.hidden = true; }, 5000);
}
document.addEventListener('click', async event => {
  const button = event.target.closest('[data-copy]');
  if (!button) return;
  try { await navigator.clipboard.writeText(button.dataset.copy); notify(button.dataset.copied ?? strings.copied); }
  catch { notify(strings.clipboard); }
});
document.addEventListener('click', event => {
  if (event.defaultPrevented || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
  if (event.target.closest('a, button, input, select, textarea, label, form')) return;
  if (window.getSelection()?.isCollapsed === false) return;
  const link = event.target.closest('.inbox-table tbody tr')?.querySelector('.subject-cell a');
  if (link) location.assign(link.href);
});
document.addEventListener('submit', event => {
  const message = event.target.dataset.confirm;
  if (!message) return;
  if (!window.confirm(message)) { event.preventDefault(); return; }
  if (!event.target.querySelector('input[name=confirm]')) {
    const confirmation = document.createElement('input');
    confirmation.type = 'hidden'; confirmation.name = 'confirm'; confirmation.value = 'true';
    event.target.append(confirmation);
  }
});
let refreshing = false;
let pending = false;
async function refresh() {
  if (refreshing) { pending = true; return; }
  refreshing = true;
  try {
    const response = await fetch(document.querySelector('#live-content') ? location.href : '/', { headers: { 'Accept': 'text/html' }, cache: 'no-store' });
    if (response.redirected && new URL(response.url).pathname === '/login') { location.assign('/login'); return; }
    if (!response.ok) throw new Error('unavailable');
    const page = new DOMParser().parseFromString(await response.text(), 'text/html');
    const address = page.querySelector('.current-address');
    if (address) document.querySelector('.current-address')?.replaceWith(address);
    const updated = page.querySelector('#live-content');
    if (updated) {
      // DOMParser exposes noscript form controls; they must stay out of JS updates.
      updated.querySelectorAll('noscript').forEach(el => el.remove());
      document.querySelector('#live-content')?.replaceWith(updated);
      document.querySelectorAll('[data-count]').forEach(el => { el.textContent = updated.dataset.total; });
    }
  } catch { notify(strings.refreshFailed); }
  finally { refreshing = false; if (pending) { pending = false; refresh(); } }
}
// The browser app announces the same events from its workers.
function subscribe() {
  if (document.body.dataset.events !== 'broadcast') return new EventSource('/events');
  const channel = new BroadcastChannel('ahrara');
  const target = new EventTarget();
  channel.addEventListener('message', ({ data }) => target.dispatchEvent(new MessageEvent(data.kind, { data: JSON.stringify(data.data) })));
  target.close = () => channel.close();
  return target;
}
const events = subscribe();
events.addEventListener('open', refresh);
for (const type of ['email_received', 'email_deleted', 'emails_cleared', 'address_created', 'address_updated', 'address_deleted', 'resync']) {
  events.addEventListener(type, () => {
    refresh();
    if (type === 'email_received') notify(strings.newEmail);
    if (document.body.dataset.page === 'email' && (type === 'email_deleted' || type === 'emails_cleared')) notify(strings.historyChanged);
  });
}
events.addEventListener('connection_state_changed', event => {
  const state = JSON.parse(event.data);
  document.querySelectorAll('[data-connection]').forEach(el => {
    el.textContent = state.connected ? strings.connected : strings.disconnected;
    el.classList.toggle('online', state.connected);
  });
});
for (const type of ['mcp_state_changed', 'web_settings_changed']) events.addEventListener(type, () => notify(strings.settingsChanged));
events.addEventListener('session_expired', () => { events.close(); location.assign('/login'); });
window.addEventListener('pagehide', () => events.close());

// Native form submission targets only the sandboxed message frame; consent is never persisted.
document.addEventListener('submit', event => {
  if (!event.target.matches('[data-load-images]')) return;
  document.querySelector('.html-view').open = true;
  document.querySelector('[data-remote-notice]').hidden = true;
  document.querySelector('[data-image-state]').textContent = strings.imagesAllowed;
});
document.addEventListener('click', event => {
  if (event.target.closest('[data-keep-blocked]')) document.querySelector('[data-remote-notice]').hidden = true;
});

// The browser app's data survives storage pressure only when persisted; ask when the
// user commits to it, by creating or adding an address or keeping its key.
document.addEventListener('submit', event => { if (event.target.matches('[data-persist]')) navigator.storage?.persist?.(); });
const storage = document.querySelector('[data-storage]');
if (storage) navigator.storage?.persisted?.().then(persisted => {
  storage.textContent = persisted ? strings.persistent : strings.bestEffort;
});

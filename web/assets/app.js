// THUGSred SMS Gateway dashboard: start-up, routing, authentication and live updates.

import { app, h, fill, api, setToken, errorBox, button, dialog, field, toast, debounce } from './core.js';
import { overview, send, inbox, outbox, history, queue } from './pages-main.js';
import { pdu, calls, integrations, modem, system, apidocs } from './pages-more.js';

const ROUTES = {
  overview: ['Overview', overview],
  send: ['Send SMS', send],
  inbox: ['Inbox', inbox],
  outbox: ['Outbox', outbox],
  history: ['Message History', history],
  queue: ['Message Queue', queue],
  pdu: ['PDU Tools', pdu],
  calls: ['Calls', calls],
  integrations: ['Integrations', integrations],
  modem: ['Modem', modem],
  system: ['System', system],
  api: ['API Documentation', apidocs],
};

const $ = (id) => document.getElementById(id);
const main = $('main');
let current = null;
let live = false;
let streamGen = 0;
let pollTimer = null;
let ticks = 0;

// ---- theme ---------------------------------------------------------------------

function applyTheme(theme) {
  const t = theme === 'light' ? 'light' : 'dark';
  document.documentElement.classList.remove('dark', 'light');
  document.documentElement.classList.add(t);
  $('theme-btn').textContent = t === 'dark' ? 'Light theme' : 'Dark theme';
}

function storedTheme() {
  try {
    return localStorage.getItem('smsgw.theme');
  } catch (_) {
    return null;
  }
}

$('theme-btn').addEventListener('click', () => {
  const next = document.documentElement.classList.contains('dark') ? 'light' : 'dark';
  try {
    localStorage.setItem('smsgw.theme', next);
  } catch (_) { /* per-browser preference only */ }
  applyTheme(next);
});

// ---- navigation -------------------------------------------------------------------

function setNav(open) {
  document.body.classList.toggle('nav-open', open);
  $('nav-toggle').setAttribute('aria-expanded', String(open));
}
$('nav-toggle').addEventListener('click', () => setNav(!document.body.classList.contains('nav-open')));
$('nav').addEventListener('click', (e) => {
  if (e.target.closest('a')) setNav(false);
});

function route() {
  const name = (location.hash.replace(/^#\/?/, '').split(/[/?]/)[0] || 'overview').toLowerCase();
  const entry = ROUTES[name];
  if (!entry) {
    location.hash = '#/overview';
    return;
  }
  if (current && current.destroy) current.destroy();
  current = null;
  document.querySelectorAll('dialog.modal').forEach((d) => d.close());
  for (const a of $('nav').querySelectorAll('a')) {
    if (a.getAttribute('href') === '#/' + name) a.setAttribute('aria-current', 'page');
    else a.removeAttribute('aria-current');
  }
  document.title = `${entry[0]} · ${app.cfg.title || 'THUGSred SMS Gateway'}`;
  fill(main, h('h1', null, entry[0]));
  try {
    current = entry[1](main) || {};
  } catch (e) {
    main.appendChild(errorBox(e));
  }
  window.scrollTo(0, 0);
}

function refresh() {
  if (document.hidden || !current || !current.refresh) return;
  if (document.querySelector('dialog.modal[open]')) return; // do not move things under an open dialog
  current.refresh(false);
}
const refreshSoon = debounce(refresh, 800);

// ---- live updates -----------------------------------------------------------------

function setLive(on) {
  live = on;
  const el = $('live');
  el.className = 'badge ' + (on ? 'ok' : 'warn');
  fill(el, h('span', { class: 'glyph', 'aria-hidden': 'true' }, on ? '●' : '▲'), on ? 'live' : 'polling');
  el.title = on ? 'Connected to the event stream; pages update as events arrive.' : `Event stream not connected; refreshing every ${app.cfg.refresh_interval_seconds || 10} s.`;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Consume the server-sent event stream with fetch so the bearer token can be sent. */
async function stream() {
  const gen = ++streamGen;
  let backoff = 1000;
  while (gen === streamGen) {
    try {
      const headers = { Accept: 'text/event-stream' };
      if (app.token) headers.Authorization = 'Bearer ' + app.token;
      const res = await fetch('/api/v1/events/stream', { headers, cache: 'no-store' });
      if (!res.ok || !res.body) throw new Error('HTTP ' + res.status);
      const reader = res.body.getReader();
      if (gen !== streamGen) {
        reader.cancel();
        return;
      }
      setLive(true);
      backoff = 1000;
      const dec = new TextDecoder();
      let buf = '';
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (gen !== streamGen) {
          reader.cancel();
          return;
        }
        buf += dec.decode(value, { stream: true });
        let i;
        while ((i = buf.indexOf('\n')) >= 0) {
          const line = buf.slice(0, i);
          buf = buf.slice(i + 1);
          if (line.startsWith('event:')) refreshSoon();
        }
        if (buf.length > 1 << 20) buf = '';
      }
    } catch (_) {
      /* fall through to reconnect */
    }
    if (gen !== streamGen) return;
    setLive(false);
    await sleep(backoff);
    backoff = Math.min(backoff * 2, 30000);
  }
}

function startPolling() {
  clearInterval(pollTimer);
  const every = Math.max(2, Number(app.cfg.refresh_interval_seconds) || 10);
  // Without the stream, poll at the configured interval; with it, only a slow safety refresh.
  pollTimer = setInterval(() => {
    ticks++;
    if (!live || ticks % 6 === 0) refresh();
  }, every * 1000);
}

document.addEventListener('visibilitychange', () => {
  if (!document.hidden) refresh();
});

// ---- authentication ---------------------------------------------------------------------

let prompting = false;

function tokenPrompt(message) {
  if (prompting) return;
  prompting = true;
  const input = h('input', { type: 'password', autocomplete: 'off', required: true, placeholder: 'Bearer token' });
  const form = h('form', { onsubmit: (e) => {
    e.preventDefault();
    const t = input.value.trim();
    if (!t) return;
    setToken(t);
    d.close();
    start();
  } },
    message ? h('p', { class: 'bad-text' }, message) : null,
    h('p', null, 'Enter an API token. Administrative actions need an administrative token.'),
    field('Token', input),
    h('p', { class: 'muted small-text' }, 'The token is kept in this browser tab only (session storage) and is sent to this gateway alone.'),
    h('div', { class: 'row end' }, button('Cancel', () => d.close(), 'ghost'), h('button', { type: 'submit', class: 'btn primary' }, 'Use token')));
  const d = dialog('Authentication', form, { onClose: () => (prompting = false) });
  input.focus();
}

app.onUnauthorized = tokenPrompt;
$('token-btn').addEventListener('click', () => tokenPrompt(null));
$('forget-btn').addEventListener('click', () => {
  setToken(null);
  toast('Token forgotten', 'ok');
  start();
});

// ---- start --------------------------------------------------------------------------------

async function start() {
  $('forget-btn').hidden = !app.token;
  $('token-btn').textContent = app.token ? 'Change token' : 'Set token';
  streamGen++;
  clearInterval(pollTimer);
  try {
    app.cfg = (await api('/api/v1/ui/config')).data || {};
  } catch (e) {
    if (current && current.destroy) current.destroy();
    current = null;
    fill(main, h('h1', null, 'THUGSred SMS Gateway'), errorBox(e),
      e.status === 401 ? h('p', null, 'This gateway requires a bearer token.') : button('Try again', start, 'primary'));
    return;
  }
  const cfg = app.cfg;
  applyTheme(storedTheme() || cfg.theme);
  $('title').textContent = cfg.title || 'THUGSred SMS Gateway';
  $('instance').textContent = cfg.instance_name && cfg.instance_name !== cfg.title ? cfg.instance_name : '';
  $('version').textContent = cfg.version ? 'v' + cfg.version : '';
  setLive(false);
  route();
  stream();
  startPolling();
}

window.addEventListener('hashchange', route);
applyTheme(storedTheme() || 'dark');
start();

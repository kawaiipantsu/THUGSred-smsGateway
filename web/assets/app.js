// THUGSred SMS Gateway dashboard: start-up, routing, authentication and live updates.

import { app, h, s, fill, api, setToken, errorBox, button, dialog, field, toast, debounce } from './core.js';
import { overview, inbox, outbox, history as historyPage, queue, openMessage } from './pages-main.js';
import { send, contacts } from './pages-send.js';
import { threats } from './pages-threats.js';
import { tokens } from './pages-tokens.js';
import { pdu, calls, integrations, modem, system, apidocs } from './pages-more.js';

const ROUTES = {
  overview: ['Overview', overview],
  send: ['Send SMS', send],
  contacts: ['Contacts', contacts],
  inbox: ['Inbox', inbox],
  outbox: ['Outbox', outbox],
  history: ['Message History', historyPage],
  queue: ['Message Queue', queue],
  threats: ['Threats', threats],
  pdu: ['PDU Tools', pdu],
  calls: ['Calls', calls],
  integrations: ['Integrations', integrations],
  modem: ['Modem', modem],
  tokens: ['API Tokens', tokens],
  system: ['System', system],
  docs: ['API Documentation', apidocs],
};

const $ = (id) => document.getElementById(id);
const main = $('main');
let current = null;
let live = false;
let streamGen = 0;
let pollTimer = null;
let ticks = 0;

// ---- theme ---------------------------------------------------------------------

const SUN = 'M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10ZM12 1v2M12 21v2M4.22 4.22l1.42 1.42M18.36 18.36l1.42 1.42M1 12h2M21 12h2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42';
const MOON = 'M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79Z';
const prefersLight = window.matchMedia ? window.matchMedia('(prefers-color-scheme: light)') : null;

/** "auto" follows the browser's preference. */
function applyTheme(theme) {
  const t = theme === 'auto' ? (prefersLight && prefersLight.matches ? 'light' : 'dark') : theme === 'light' ? 'light' : 'dark';
  document.documentElement.classList.remove('dark', 'light');
  document.documentElement.classList.add(t);
  const next = t === 'dark' ? 'light' : 'dark';
  fill($('theme-btn'), s('svg', { viewBox: '0 0 24 24', class: 'ico', 'aria-hidden': 'true' }, s('path', { d: t === 'dark' ? SUN : MOON })));
  $('theme-btn').title = `Switch to the ${next} theme`;
  $('theme-btn').setAttribute('aria-label', `Switch to the ${next} theme`);
}
if (prefersLight) {
  prefersLight.addEventListener('change', () => {
    if (!storedTheme() && app.cfg.theme === 'auto') applyTheme('auto');
  });
}

/** Name, logo, colours and footer from the gateway's branding settings. */
function applyBranding(cfg) {
  const title = cfg.title || 'SMS Gateway';
  $('title').textContent = title;
  $('subtitle').textContent = cfg.subtitle || '';
  const icon = cfg.logo || '/favicon.svg';
  if ($('logo').getAttribute('src') !== icon) $('logo').setAttribute('src', icon);
  $('icon').setAttribute('href', icon);
  // A custom logo is also shown large above the navigation.
  $('nav-brand').hidden = !cfg.logo;
  if (cfg.logo && $('nav-logo').getAttribute('src') !== icon) $('nav-logo').setAttribute('src', icon);
  for (const tile of [$('logo-tile'), $('nav-brand')]) {
    tile.classList.toggle('logo-bg', !!(cfg.logo && cfg.logo_background));
    tile.style.background = cfg.logo && cfg.logo_background ? cfg.logo_background : '';
  }
  const root = document.documentElement;
  if (/^#[0-9a-f]{6}$/i.test(cfg.accent_color || '')) root.style.setProperty('--accent', cfg.accent_color);
  else root.style.removeProperty('--accent');

  const f = cfg.footer || {};
  const sep = () => h('span', { class: 'sep', 'aria-hidden': 'true' }, '·');
  const bits = [h('span', null, h('strong', null, f.company || title), cfg.version ? ` · v${cfg.version}` : '')];
  if (f.contact_name || f.contact_email) {
    bits.push(h('span', null, 'Contact: ', f.contact_name || '', f.contact_name && f.contact_email ? ' · ' : '',
      f.contact_email ? h('a', { href: 'mailto:' + f.contact_email }, f.contact_email) : null));
  }
  if (f.text) bits.push(h('span', null, f.text));
  bits.push(h('span', { class: 'grow' }));
  if (/^https?:\/\//i.test(f.website || '')) bits.push(h('a', { href: f.website, rel: 'noopener noreferrer', target: '_blank' }, f.website.replace(/^https?:\/\//i, '').replace(/\/$/, '')));
  if (f.show_project_link !== false) bits.push(h('a', { href: 'https://github.com/kawaiipantsu/THUGSred-smsGateway', rel: 'noopener noreferrer', target: '_blank', title: 'Open-source SMS gateway' }, 'THUGSred-smsGateway'));
  fill($('foot'), bits.map((b, i) => (i && !b.classList.contains('grow') && !bits[i - 1].classList.contains('grow') ? [sep(), b] : b)));
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

/** Page name of the current address ("/" is the overview). */
function pageName() {
  return (location.pathname.replace(/^\/+|\/+$/g, '') || 'overview').toLowerCase();
}

/** Go to another dashboard page without reloading the document. */
function navigate(url, replace) {
  if (replace) history.replaceState(null, '', url);
  else history.pushState(null, '', url);
  route();
}

function route() {
  const name = pageName();
  const entry = ROUTES[name];
  if (!entry) {
    navigate('/overview', true);
    return;
  }
  if (current && current.destroy) current.destroy();
  current = null;
  document.querySelectorAll('dialog.modal').forEach((d) => d.close());
  for (const a of $('nav').querySelectorAll('a')) {
    if (a.getAttribute('href') === '/' + name && !a.classList.contains('nav-brand')) a.setAttribute('aria-current', 'page');
    else a.removeAttribute('aria-current');
  }
  document.title = `${entry[0]} · ${app.cfg.title || 'SMS Gateway'}`;
  fill(main, h('h1', null, entry[0]));
  try {
    current = entry[1](main) || {};
  } catch (e) {
    main.appendChild(errorBox(e));
  }
  window.scrollTo(0, 0);
  // Deep link to one message: /history?message=<id>
  const message = new URLSearchParams(location.search).get('message');
  if (message) openMessage(message);
}

// Links to dashboard pages are handled here; everything else is left to the browser.
document.addEventListener('click', (e) => {
  if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
  const a = e.target.closest && e.target.closest('a[href]');
  if (!a || a.target || a.hasAttribute('download')) return;
  const url = new URL(a.href, location.href);
  if (url.origin !== location.origin || !ROUTES[url.pathname.replace(/^\/+|\/+$/g, '') || 'overview']) return;
  e.preventDefault();
  if (url.pathname + url.search !== location.pathname + location.search) navigate(url.pathname + url.search);
});

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
    fill(main, h('h1', null, $('title').textContent || 'SMS Gateway'), errorBox(e),
      e.status === 401 ? h('p', null, 'This gateway requires a bearer token.') : button('Try again', start, 'primary'));
    return;
  }
  const cfg = app.cfg;
  applyTheme(storedTheme() || cfg.theme);
  applyBranding(cfg);
  $('instance').textContent = '';
  setLive(false);
  route();
  stream();
  startPolling();
}

window.addEventListener('popstate', route);
// Addresses of earlier versions used "#/page"; move them to the real path.
if (/^#\//.test(location.hash)) {
  const old = location.hash.slice(2).replace(/^api(?=$|[/?])/, 'docs');
  history.replaceState(null, '', '/' + old);
}
applyTheme(storedTheme() || (document.documentElement.classList.contains('light') ? 'light' : 'dark'));
start();

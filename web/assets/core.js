// THUGSred SMS Gateway dashboard: shared helpers (DOM, API client, widgets).
// All API data reaches the page through text nodes only; nothing is parsed as HTML.

const TOKEN_KEY = 'smsgw.token';

export const app = {
  cfg: {},
  token: null,
  /** Called with the server's message when a request is rejected with 401. */
  onUnauthorized: null,
};

try {
  app.token = sessionStorage.getItem(TOKEN_KEY) || null;
} catch (_) {
  app.token = null;
}

export function setToken(token) {
  app.token = token || null;
  try {
    if (app.token) sessionStorage.setItem(TOKEN_KEY, app.token);
    else sessionStorage.removeItem(TOKEN_KEY);
  } catch (_) {
    /* storage unavailable: the token lives in memory only */
  }
}

// ---- DOM ---------------------------------------------------------------------

const SVG_NS = 'http://www.w3.org/2000/svg';

function setAttrs(el, attrs) {
  let value;
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k.startsWith('on')) {
      // Listeners only; handler attributes are never written.
      if (typeof v === 'function') el.addEventListener(k.slice(2), v);
    } else if (k === 'style') {
      continue; // forbidden by the Content-Security-Policy
    } else if (k === 'value') {
      value = v;
    } else if (v === true) {
      el.setAttribute(k, '');
    } else {
      el.setAttribute(k, String(v));
    }
  }
  return value;
}

function append(el, kids) {
  for (const k of kids) {
    if (k == null || k === false || k === true) continue;
    if (Array.isArray(k)) append(el, k);
    else if (k instanceof Node) el.appendChild(k);
    else el.appendChild(document.createTextNode(String(k)));
  }
}

/** Create an element. Strings always become text nodes. */
export function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  const value = setAttrs(el, attrs);
  append(el, kids);
  if (value !== undefined) el.value = value;
  return el;
}

/** Create an SVG element. */
export function s(tag, attrs, ...kids) {
  const el = document.createElementNS(SVG_NS, tag);
  setAttrs(el, attrs);
  append(el, kids);
  return el;
}

export function fill(el, ...kids) {
  el.replaceChildren();
  append(el, kids);
  return el;
}

// ---- API -----------------------------------------------------------------------

export class ApiError extends Error {
  constructor(message, status, code) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

export function qs(query) {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(query || {})) {
    if (v !== undefined && v !== null && v !== '') p.append(k, v);
  }
  const out = p.toString();
  return out ? '?' + out : '';
}

/** Low-level request: adds the bearer token and JSON content type. Returns the Response. */
export async function request(path, opt = {}) {
  const headers = Object.assign({ Accept: 'application/json' }, opt.headers || {});
  if (app.token) headers.Authorization = 'Bearer ' + app.token;
  let body;
  if (opt.rawBody !== undefined) {
    body = opt.rawBody;
    headers['Content-Type'] = 'application/json';
  } else if (opt.body !== undefined) {
    body = JSON.stringify(opt.body);
    headers['Content-Type'] = 'application/json';
  }
  try {
    return await fetch(path + qs(opt.query), { method: opt.method || 'GET', headers, body, cache: 'no-store', signal: opt.signal });
  } catch (e) {
    if (e && e.name === 'AbortError') throw e;
    throw new ApiError('Cannot reach the gateway (network error).', 0, 'network');
  }
}

async function failure(res, opt) {
  let message = 'HTTP ' + res.status + (res.statusText ? ' ' + res.statusText : '');
  let code = 'http_' + res.status;
  try {
    const j = await res.json();
    if (j && j.error) {
      message = j.error.message || message;
      code = j.error.code || code;
    }
  } catch (_) {
    /* not JSON */
  }
  if (res.status === 401 && !opt.noAuthPrompt && app.onUnauthorized) app.onUnauthorized(message);
  return new ApiError(message, res.status, code);
}

/** JSON request. Resolves with the whole response envelope; rejects with ApiError. */
export async function api(path, opt = {}) {
  const res = await request(path, opt);
  if (!res.ok) throw await failure(res, opt);
  try {
    return await res.json();
  } catch (_) {
    throw new ApiError('The gateway returned an unreadable response.', res.status, 'bad_response');
  }
}

export const get = async (path, query) => (await api('/api/v1' + path, { query })).data;
export const post = async (path, body, query) => (await api('/api/v1' + path, { method: 'POST', body, query })).data;

/** Download through fetch so the bearer token is sent, then hand the Blob to the browser. */
export async function download(path, query, fallbackName) {
  const res = await request(path, { query, headers: { Accept: '*/*' } });
  if (!res.ok) throw await failure(res, {});
  const cd = res.headers.get('content-disposition') || '';
  const m = /filename="?([^";]+)"?/.exec(cd);
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  const a = h('a', { href: url, download: (m && m[1]) || fallbackName, hidden: true });
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 30000);
}

// ---- formatting ------------------------------------------------------------------

const pad = (n) => String(n).padStart(2, '0');

export function fmtTime(iso) {
  if (!iso) return h('span', { class: 'muted' }, '—');
  const d = new Date(iso);
  if (isNaN(d)) return String(iso);
  const txt = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  return h('time', { datetime: String(iso), title: String(iso), class: 'nowrap' }, txt);
}

export function fmtBytes(n) {
  if (typeof n !== 'number' || !isFinite(n)) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return (i === 0 ? n : n.toFixed(1)) + ' ' + units[i];
}

export function fmtDuration(sec) {
  if (typeof sec !== 'number' || !isFinite(sec)) return '—';
  sec = Math.floor(sec);
  const d = Math.floor(sec / 86400), hh = Math.floor((sec % 86400) / 3600), mm = Math.floor((sec % 3600) / 60);
  if (d) return `${d} d ${hh} h`;
  if (hh) return `${hh} h ${mm} min`;
  if (mm) return `${mm} min ${sec % 60} s`;
  return `${sec} s`;
}

export const dash = (v) => (v === null || v === undefined || v === '' ? h('span', { class: 'muted' }, '—') : v);

/** "Name (number)" when the address book knows the number. */
export const party = (number, name) => (number ? (name ? `${name} (${number})` : number) : dash(null));

export const HIDDEN = 'hidden by configuration';
export const hidden = () => h('em', { class: 'muted' }, HIDDEN);

// ---- widgets -----------------------------------------------------------------------

const KINDS = {
  ok: ['delivered', 'sent', 'submitted', 'received', 'ready', 'home', 'completed', 'running', 'healthy', 'supported', 'enabled', 'yes', 'valid', 'success', 'assembled', 'answered'],
  warn: ['queued', 'scheduled', 'processing', 'sending', 'retrying', 'pending', 'registering', 'connecting', 'initializing', 'recovering', 'degraded', 'roaming', 'searching', 'submission_unknown', 'unverified', 'unknown', 'experimental', 'shutting_down', 'ringing', 'restart required'],
  bad: ['failed', 'delivery_failed', 'dead', 'expired', 'disconnected', 'sim_locked', 'denied', 'missing', 'error', 'pin_required', 'puk_required', 'not_registered', 'unhealthy', 'invalid', 'malformed', 'missed'],
  muted: ['cancelled', 'disabled', 'unsupported', 'no', 'none'],
};
const GLYPH = { ok: '●', warn: '▲', bad: '✖', muted: '○', info: '◆' };

export function kindOf(text) {
  const t = String(text).toLowerCase();
  for (const k of Object.keys(KINDS)) if (KINDS[k].includes(t)) return k;
  return 'info';
}

/** Status badge: always carries its text label plus a shape, never colour alone. */
export function badge(text, kind, title) {
  if (text === null || text === undefined || text === '') return dash(null);
  const k = kind || kindOf(text);
  return h('span', { class: 'badge ' + k, title }, h('span', { class: 'glyph', 'aria-hidden': 'true' }, GLYPH[k] || GLYPH.info), String(text));
}

export const yesno = (v, yes = 'yes', no = 'no') => badge(v ? yes : no, v ? 'ok' : 'muted');

export function errorBox(e) {
  return h('div', { class: 'notice bad', role: 'alert' }, h('strong', null, 'Error: '), (e && e.message) || String(e));
}

export function notice(kind, ...kids) {
  return h('div', { class: 'notice ' + kind }, kids);
}

export function button(label, onclick, cls, attrs) {
  return h('button', Object.assign({ type: 'button', class: 'btn ' + (cls || ''), onclick }, attrs || {}), label);
}

/** Run an async action from a button: disables it meanwhile and reports failures. */
export async function busy(btn, fn) {
  if (btn) btn.disabled = true;
  try {
    return await fn();
  } catch (e) {
    toast(e.message || String(e), 'bad');
  } finally {
    if (btn) btn.disabled = false;
  }
}

/** Definition list from [label, value] pairs. */
export function kv(pairs) {
  return h('dl', { class: 'kv' }, pairs.filter(Boolean).map(([k, v]) => [h('dt', null, k), h('dd', null, dash(v))]));
}

/** Nested key/value rendering of arbitrary JSON. */
export function tree(v) {
  if (v === null || v === undefined) return h('span', { class: 'muted' }, 'null');
  if (Array.isArray(v)) {
    if (!v.length) return h('span', { class: 'muted' }, '(empty)');
    if (v.every((x) => x === null || typeof x !== 'object')) return h('span', { class: 'mono' }, v.map(String).join(', '));
    return h('ol', { class: 'tree-list' }, v.map((x) => h('li', null, tree(x))));
  }
  if (typeof v === 'object') {
    const keys = Object.keys(v);
    if (!keys.length) return h('span', { class: 'muted' }, '(empty)');
    return h('dl', { class: 'tree' }, keys.map((k) => [h('dt', null, k), h('dd', null, tree(v[k]))]));
  }
  return h('span', { class: 'mono val-' + typeof v }, String(v));
}

/**
 * Table. cols: [{label, get(row) -> node|string, cls}]. opt.onRow makes rows activatable.
 */
export function table(cols, rows, opt = {}) {
  if (!rows || !rows.length) return h('p', { class: 'muted empty' }, opt.empty || 'Nothing to show.');
  const body = rows.map((r) => {
    const attrs = {};
    if (opt.onRow) {
      attrs.class = 'click';
      attrs.tabindex = 0;
      attrs.onclick = (e) => {
        if (!e.target.closest('button, a, input, select')) opt.onRow(r);
      };
      attrs.onkeydown = (e) => {
        if (e.key === 'Enter' && e.target === e.currentTarget) opt.onRow(r);
      };
    }
    return h('tr', attrs, cols.map((c) => h('td', { class: c.cls }, dash(c.get(r)))));
  });
  return h('div', { class: 'table-wrap' }, h('table', null, opt.caption ? h('caption', null, opt.caption) : null, h('thead', null, h('tr', null, cols.map((c) => h('th', { scope: 'col' }, c.label)))), h('tbody', null, body)));
}

/** Pagination controls for {total, limit, offset}. */
export function pager(p, go) {
  if (!p) return null;
  const from = p.total ? p.offset + 1 : 0;
  const to = Math.min(p.offset + p.limit, p.total);
  return h('div', { class: 'pager' },
    h('span', { class: 'muted' }, `${from}–${to} of ${p.total}`),
    button('‹ Previous', () => go(Math.max(0, p.offset - p.limit)), 'small', { disabled: p.offset <= 0 }),
    button('Next ›', () => go(p.offset + p.limit), 'small', { disabled: p.offset + p.limit >= p.total }));
}

/**
 * A card whose content comes from an async loader. reload() keeps the previous
 * content visible until the new content is ready, and leaves the card alone
 * while the user is typing inside it.
 */
export function panel(title, loader, opt = {}) {
  const body = h('div', { class: 'panel-body' }, h('p', { class: 'muted' }, 'Loading…'));
  const el = h('section', { class: 'card ' + (opt.cls || '') }, title ? h('header', { class: 'card-head' }, h('h2', null, title), opt.tools) : null, body);
  let seq = 0;
  async function reload(force) {
    const a = document.activeElement;
    if (!force && a && body.contains(a) && /^(INPUT|SELECT|TEXTAREA)$/.test(a.tagName)) return;
    const my = ++seq;
    let content;
    try {
      content = await loader();
    } catch (e) {
      content = errorBox(e);
    }
    if (my !== seq) return;
    const open = [...body.querySelectorAll('details')].map((d) => d.open);
    fill(body, content);
    body.querySelectorAll('details').forEach((d, i) => {
      if (open[i]) d.open = true;
    });
  }
  return { el, body, reload };
}

/** Append panels to a page and load them. Returns the page handle. */
export function mountPanels(root, panels) {
  for (const p of panels) root.appendChild(p.el);
  const refresh = (force) => panels.forEach((p) => p.reload(force));
  refresh(true);
  return { refresh };
}

/** Modal dialog built from DOM nodes. Returns the <dialog>. */
export function dialog(title, content, opt = {}) {
  const d = h('dialog', { class: 'modal' + (opt.wide ? ' wide' : ''), 'aria-label': title });
  const close = () => d.close();
  append(d, [
    h('header', { class: 'modal-head' }, h('h2', null, title), button('✕', close, 'ghost small', { 'aria-label': 'Close' })),
    h('div', { class: 'modal-body' }, content),
    opt.actions ? h('footer', { class: 'modal-foot' }, opt.actions(close)) : null,
  ]);
  d.addEventListener('close', () => {
    d.remove();
    if (opt.onClose) opt.onClose();
  });
  document.body.appendChild(d);
  d.showModal();
  return d;
}

/** Confirmation dialog. Resolves true only when the confirm button is clicked. */
export function confirmBox(title, content, okLabel = 'Confirm', danger = false) {
  return new Promise((resolve) => {
    let result = false;
    dialog(title, typeof content === 'string' ? h('p', null, content) : content, {
      onClose: () => resolve(result),
      actions: (close) => [
        button('Cancel', close, 'ghost'),
        button(okLabel, () => {
          result = true;
          close();
        }, danger ? 'danger' : 'primary'),
      ],
    });
  });
}

export function toast(message, kind = 'info') {
  const host = document.getElementById('toasts');
  if (!host) return;
  const t = h('div', { class: 'toast ' + kind, role: kind === 'bad' ? 'alert' : 'status' }, String(message));
  host.appendChild(t);
  setTimeout(() => t.remove(), kind === 'bad' ? 9000 : 4500);
}

/** Labelled form field. */
export function field(label, control, hint) {
  return h('label', { class: 'field' }, h('span', { class: 'field-label' }, label), control, hint ? h('small', { class: 'muted' }, hint) : null);
}

export function select(options, attrs) {
  return h('select', attrs, options.map((o) => (Array.isArray(o) ? h('option', { value: o[0] }, o[1]) : h('option', { value: o }, o))));
}

export function check(label, attrs) {
  const input = h('input', Object.assign({ type: 'checkbox' }, attrs || {}));
  return { input, el: h('label', { class: 'check' }, input, h('span', null, label)) };
}

/** Signal strength: bar plus percentage and dBm as text. */
export function signal(percent, dbm) {
  if (percent === null || percent === undefined) return h('span', { class: 'muted' }, 'no signal information');
  const p = Math.max(0, Math.min(100, Number(percent) || 0));
  const bar = h('span', { class: 'meter-fill ' + (p >= 50 ? 'ok' : p >= 25 ? 'warn' : 'bad') });
  bar.style.width = p + '%';
  return h('span', { class: 'signal' },
    h('span', { class: 'meter', role: 'meter', 'aria-valuemin': 0, 'aria-valuemax': 100, 'aria-valuenow': p, 'aria-label': 'Signal strength' }, bar),
    h('span', { class: 'nowrap' }, `${p} %` + (dbm !== null && dbm !== undefined ? ` · ${dbm} dBm` : '')));
}

export function debounce(fn, ms) {
  let t;
  return (...a) => {
    clearTimeout(t);
    t = setTimeout(() => fn(...a), ms);
  };
}

/** Short summary of a stored event that never includes message text. */
export function eventSummary(ev) {
  const d = (ev && ev.data) || {};
  const m = d.message;
  if (m && typeof m === 'object') {
    const bits = [m.direction, [m.sender, m.recipient].filter(Boolean).join(' → '), m.status, m.parts > 1 ? m.parts + ' parts' : null, m.error ? 'error: ' + m.error : null];
    return bits.filter(Boolean).join(' · ');
  }
  const out = [];
  for (const [k, v] of Object.entries(d)) {
    if (v === null || typeof v === 'object') continue;
    if (app.cfg.show_modem_information === false && ['firmware', 'imei', 'iccid', 'imsi'].includes(k)) continue;
    out.push(`${k}: ${String(v).slice(0, 60)}`);
    if (out.length >= 4) break;
  }
  if (d.ussd && d.ussd.code) out.push('code: ' + d.ussd.code);
  if (d.call && typeof d.call === 'object' && d.call.number) out.push('number: ' + d.call.number);
  return out.join(' · ');
}

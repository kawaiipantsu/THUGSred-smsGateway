// Overview, Send SMS, Inbox / Outbox / History and Message Queue.

import {
  app, h, s, fill, api, get, post, download, fmtTime, fmtBytes, fmtDuration, dash, party, hidden, badge, yesno,
  errorBox, notice, button, busy, kv, tree, table, pager, panel, mountPanels, dialog, confirmBox, toast, field,
  select, check, signal, debounce, eventSummary,
} from './core.js';

const showBody = () => app.cfg.show_message_content !== false;
const bodyCell = (m) => (showBody() ? (m.body === null || m.body === undefined ? dash(null) : m.body) : hidden());
const modemName = (m) => [m.manufacturer, m.model].filter(Boolean).join(' ') || 'unidentified modem';

// ---- message detail -------------------------------------------------------------

const TIME_KEY = /_at$|timestamp$/;
const SKIP = ['body', 'raw_pdu', 'decoded_pdu', 'delivery_reports', 'attachments', 'queue', 'metadata', 'payload', 'sender_name', 'recipient_name'];

function messageFields(m) {
  const rows = [];
  for (const k of Object.keys(m)) {
    if (SKIP.includes(k)) continue;
    let v = m[k];
    if (k === 'sender') v = party(m.sender, m.sender_name);
    else if (k === 'recipient') v = party(m.recipient, m.recipient_name);
    else if (v === null || v === undefined) v = null;
    else if (TIME_KEY.test(k)) v = fmtTime(v);
    else if (k === 'status' || k === 'delivery_status' || k === 'integration_status') v = badge(v);
    else if (typeof v === 'boolean') v = yesno(v);
    else if (typeof v === 'object') v = tree(v);
    else v = h('span', { class: /id$|_key$/.test(k) ? 'mono' : null }, String(v));
    rows.push([k, v]);
  }
  return kv(rows);
}

const sub = (title, ...kids) => h('section', { class: 'sub' }, h('h3', null, title), kids);
const pduBlock = (hex) => h('pre', { class: 'pdu' }, hex);

/** Open the detail dialog for one message. */
export async function openMessage(id, onChanged) {
  const box = h('div', null, h('p', { class: 'muted' }, 'Loading…'));
  // The address carries the open message, so it can be reloaded and shared.
  const setParam = (value) => {
    const u = new URL(location.href);
    if (value) u.searchParams.set('message', value);
    else if (u.searchParams.get('message') === id) u.searchParams.delete('message');
    else return;
    window.history.replaceState(null, "", u.pathname + u.search);
  };
  setParam(id);
  const d = dialog('Message ' + id, box, { wide: true, onClose: () => setParam(null) });
  try {
    const [m, parts, st] = await Promise.all([get('/messages/' + encodeURIComponent(id)), get(`/messages/${encodeURIComponent(id)}/parts`), get(`/messages/${encodeURIComponent(id)}/status`)]);
    const content = showBody();
    const del = button('Delete message', async (e) => {
      if (!(await confirmBox('Delete message', 'Delete this message and its parts permanently? This is an administrative action and cannot be undone.', 'Delete', true))) return;
      await busy(e.target, async () => {
        await api('/api/v1/messages/' + encodeURIComponent(id), { method: 'DELETE' });
        toast('Message deleted', 'ok');
        d.close();
        if (onChanged) onChanged();
      });
    }, 'danger small');
    fill(box,
      h('div', { class: 'row wrap' }, badge(m.direction, 'info'), badge(m.message_type, 'info'), badge(m.status), m.delivery_status ? badge('delivery: ' + m.delivery_status, null) : null, h('span', { class: 'grow' }), del),
      sub('Message text', content ? (m.body === null ? h('p', { class: 'muted' }, 'No text body.') : h('pre', { class: 'body' }, m.body)) : hidden()),
      m.error ? notice('bad', h('strong', null, 'Error: '), m.error) : null,
      sub('Fields', messageFields(m)),
      m.metadata ? sub('Metadata', tree(m.metadata)) : null,
      sub('Raw PDU', content ? (m.raw_pdu ? pduBlock(m.raw_pdu) : h('p', { class: 'muted' }, 'No PDU stored.')) : hidden()),
      sub('Decoded PDU', content ? (m.decoded_pdu ? tree(m.decoded_pdu) : h('p', { class: 'muted' }, 'Not available.')) : hidden()),
      sub(`Parts (${parts.length})`, table([
        { label: '#', get: (p) => p.sequence + (p.concat_total ? ' / ' + p.concat_total : '') },
        { label: 'Status', get: (p) => badge(p.status) },
        { label: 'Encoding', get: (p) => p.encoding },
        { label: 'Ref', get: (p) => p.modem_ref },
        { label: 'Submitted', get: (p) => fmtTime(p.submitted_at) },
        { label: 'Delivered', get: (p) => fmtTime(p.delivered_at) },
        { label: 'Error', get: (p) => p.error },
        { label: 'PDU', cls: 'mono wrap-any', get: (p) => (content ? p.pdu : hidden()) },
      ], parts, { empty: 'No parts recorded.' })),
      sub('Status history', table([
        { label: 'Time', get: (x) => fmtTime(x.created_at) },
        { label: 'Status', get: (x) => badge(x.status) },
        { label: 'Detail', get: (x) => x.detail },
      ], st.history, { empty: 'No status changes recorded.' })),
      sub('Delivery reports', table([
        { label: 'Received', get: (x) => fmtTime(x.received_at) },
        { label: 'Category', get: (x) => badge(x.category) },
        { label: 'Description', get: (x) => x.description },
        { label: 'Code', get: (x) => x.status },
        { label: 'Recipient', get: (x) => x.recipient },
        { label: 'Discharge time', get: (x) => fmtTime(x.discharge_time) },
        { label: 'Ref', get: (x) => x.modem_ref },
      ], st.delivery_reports, { empty: 'No delivery reports.' })),
      sub('Attachments', table([
        { label: 'File', get: (x) => x.filename },
        { label: 'Type', get: (x) => x.content_type },
        { label: 'Size', get: (x) => fmtBytes(x.size_bytes) },
        { label: 'SHA-256', cls: 'mono wrap-any', get: (x) => x.sha256 },
        { label: 'Stored', get: (x) => fmtTime(x.created_at) },
      ], m.attachments, { empty: 'No attachments.' })),
      m.queue ? sub('Queue entry', tree(m.queue)) : null);
  } catch (e) {
    fill(box, errorBox(e));
  }
}

const messageCols = (withDirection) => [
  { label: 'Time', get: (m) => fmtTime(m.created_at) },
  withDirection ? { label: 'Direction', get: (m) => badge(m.direction, 'info') } : null,
  { label: 'From', get: (m) => party(m.sender, m.sender_name) },
  { label: 'To', get: (m) => party(m.recipient, m.recipient_name) },
  { label: 'Message', cls: 'clip', get: bodyCell },
  { label: 'Type', get: (m) => m.message_type },
  { label: 'Status', get: (m) => badge(m.status) },
  { label: 'Parts', get: (m) => m.parts },
].filter(Boolean);

// ---- overview ---------------------------------------------------------------------

function niceTop(max) {
  let p = 1;
  for (;;) {
    for (const m of [1, 2, 4, 10]) if (m * p >= max) return m * p;
    p *= 10;
  }
}

function barPath(x, y, w, y0) {
  const r = Math.min(3, w / 2, y0 - y);
  return `M${x},${y0}V${y + r}Q${x},${y} ${x + r},${y}H${x + w - r}Q${x + w},${y} ${x + w},${y + r}V${y0}Z`;
}

/** Inbound/outbound bar chart as inline SVG with a hover read-out and a table view. */
function activityChart(d) {
  const b = d.buckets || [];
  const hourly = d.bucket === 'hour';
  const W = 720, H = 220, L = 38, R = 8, T = 10, B = 26;
  const top = niceTop(Math.max(1, ...b.map((x) => Math.max(x.inbound, x.outbound))));
  const y0 = H - B, ph = y0 - T, pw = (W - L - R) / Math.max(1, b.length);
  const bw = Math.max(2, Math.min(12, (pw - 5) / 2));
  const label = (k) => (hourly ? k.slice(11) + ':00' : k.slice(5));
  const long = (k) => (hourly ? `${k.slice(0, 10)} ${k.slice(11)}:00` : k) + ' UTC';
  const totalIn = b.reduce((a, x) => a + x.inbound, 0), totalOut = b.reduce((a, x) => a + x.outbound, 0);
  const caption = `Messages per ${hourly ? 'hour' : 'day'} (${d.range}, UTC): ${totalIn} inbound, ${totalOut} outbound`;
  const tip = h('div', { class: 'chart-tip', hidden: true });
  const svg = s('svg', { viewBox: `0 0 ${W} ${H}`, role: 'img', 'aria-label': caption, class: 'chart' }, s('title', null, caption));
  for (const t of top === 1 ? [0, 1] : [0, top / 2, top]) {
    const y = y0 - (t / top) * ph;
    svg.appendChild(s('line', { x1: L, x2: W - R, y1: y, y2: y, class: t ? 'gridline' : 'axis' }));
    svg.appendChild(s('text', { x: L - 6, y: y + 4, class: 'tick', 'text-anchor': 'end' }, t));
  }
  const every = Math.ceil(b.length / 7);
  b.forEach((x, i) => {
    const gx = L + i * pw, cx = gx + pw / 2;
    const text = `${long(x.bucket)}: ${x.inbound} inbound, ${x.outbound} outbound`;
    const g = s('g', { class: 'bucket' }, s('title', null, text), s('rect', { x: gx, y: T, width: pw, height: ph, class: 'hit' }));
    if (x.inbound) g.appendChild(s('path', { d: barPath(cx - bw - 1, y0 - (x.inbound / top) * ph, bw, y0), class: 'bar-in' }));
    if (x.outbound) g.appendChild(s('path', { d: barPath(cx + 1, y0 - (x.outbound / top) * ph, bw, y0), class: 'bar-out' }));
    g.addEventListener('pointermove', (e) => {
      const box = wrap.getBoundingClientRect();
      fill(tip, h('strong', null, long(x.bucket)), h('span', null, h('i', { class: 'sw in' }), `Inbound ${x.inbound}`), h('span', null, h('i', { class: 'sw out' }), `Outbound ${x.outbound}`));
      tip.hidden = false;
      tip.style.left = Math.max(0, Math.min(box.width - 170, e.clientX - box.left + 12)) + wrap.scrollLeft + 'px';
      tip.style.top = Math.max(0, e.clientY - box.top - 70) + 'px';
    });
    g.addEventListener('pointerleave', () => (tip.hidden = true));
    svg.appendChild(g);
    if (i % every === 0) svg.appendChild(s('text', { x: cx, y: H - 8, class: 'tick', 'text-anchor': 'middle' }, label(x.bucket)));
  });
  const wrap = h('div', { class: 'chart-wrap' }, svg, tip);
  return [
    h('div', { class: 'legend' }, h('span', null, h('i', { class: 'sw in' }), `Inbound (${totalIn})`), h('span', null, h('i', { class: 'sw out' }), `Outbound (${totalOut})`), h('span', { class: 'muted' }, `per ${hourly ? 'hour' : 'day'}, UTC`)),
    wrap,
    h('details', null, h('summary', null, 'Show data table'), table([
      { label: hourly ? 'Hour (UTC)' : 'Day (UTC)', get: (x) => long(x.bucket) },
      { label: 'Inbound', get: (x) => x.inbound },
      { label: 'Outbound', get: (x) => x.outbound },
    ], b)),
  ];
}

const tile = (label, value, note) => h('div', { class: 'tile' }, h('div', { class: 'tile-label' }, label), h('div', { class: 'tile-value' }, value), note ? h('div', { class: 'muted small-text' }, note) : null);

function modemCard(m) {
  return h('div', { class: 'modem' },
    h('div', { class: 'row wrap' }, h('strong', { class: 'mono' }, m.id), badge(m.state), m.enabled === false ? badge('disabled', 'muted') : null),
    m.hint ? notice('warn hint', h('strong', null, 'Action needed: '), m.hint) : null,
    m.last_error ? notice('bad', h('strong', null, 'Last error: '), m.last_error) : null,
    kv([
      ['Modem', modemName(m)],
      ['Operator', m.operator],
      ['Registration', badge(m.registration)],
      ['SIM', h('span', null, badge(m.sim_state), m.sim_id ? ' ' + m.sim_id : '')],
      ['Gateway number', m.mobile_number ? h('span', { class: 'mono' }, m.mobile_number) : h('span', { class: 'muted' }, 'not configured')],
      ['Signal', signal(m.signal_percent, m.rssi_dbm)],
    ]));
}

export function overview(root) {
  let range = '24h';
  // The status document feeds several cards; fetch it once per refresh.
  let stP = null, stT = 0;
  const status = () => {
    if (!stP || Date.now() - stT > 1500) {
      stT = Date.now();
      stP = get('/status');
    }
    return stP;
  };
  const hints = panel(null, async () => {
    const st = await status();
    const list = st.modems.filter((m) => m.hint);
    hints.el.hidden = !list.length;
    return list.map((m) => notice('warn hint', h('strong', null, `Modem ${m.id} needs attention: `), m.hint));
  }, { cls: 'bare' });
  const daemon = panel('Daemon', async () => {
    const d = (await status()).daemon;
    return kv([['Status', badge(d.status)], ['Instance', `${d.instance_name} (${d.instance_id})`], ['Version', `${d.version} (${d.commit})`], ['Uptime', fmtDuration(d.uptime_seconds)], ['Started', fmtTime(d.started_at)]]);
  });
  const counters = panel('Counters', async () => {
    const st = await get('/stats');
    const m = st.messages || {};
    const rate = m.delivery_success_rate_percent;
    return h('div', { class: 'tiles' },
      tile('Received', m.received),
      tile('Sent', m.sent, `${dash(m.outbound_total)} outbound in total`),
      tile('Delivery success', typeof rate === 'number' ? rate.toFixed(1) + ' %' : '—', `${m.delivered} delivered · ${m.delivery_failed} not delivered`),
      tile('Failed', m.failed, m.submission_unknown ? `${m.submission_unknown} with unknown outcome` : null),
      tile('Pending queue', st.queue ? st.queue.pending : '—'));
  }, { cls: 'span2' });
  const modems = panel('Modems', async () => {
    const st = await status();
    return st.modems.length ? h('div', { class: 'modems' }, st.modems.map(modemCard)) : h('p', { class: 'muted' }, 'No modems configured.');
  }, { cls: 'span2' });
  const rangeBtns = h('div', { class: 'seg', role: 'group', 'aria-label': 'Time range' });
  const drawRange = () => fill(rangeBtns, ['24h', '7d', '30d'].map((r) => button(r === '24h' ? '24 h' : r === '7d' ? '7 d' : '30 d', () => {
    range = r;
    drawRange();
    activity.reload(true);
  }, 'small' + (r === range ? ' active' : ''), { 'aria-pressed': String(r === range) })));
  const activity = panel('Message activity', async () => activityChart(await get('/stats/activity', { range })), { cls: 'span2', tools: rangeBtns });
  drawRange();
  const integ = panel('Integrations', async () => {
    const st = await status();
    return table([
      { label: 'Name', get: (i) => i.name },
      { label: 'Type', get: (i) => i.type },
      { label: 'State', get: (i) => (i.enabled ? badge(i.healthy ? 'healthy' : 'unhealthy') : badge('disabled', 'muted')) },
      { label: 'Pending', get: (i) => i.pending },
      { label: 'Dead', get: (i) => i.dead },
    ], st.integrations, { empty: 'No integrations configured.' });
  }, { cls: 'span2' });
  const db = panel('Database and storage', async () => {
    const st = await status();
    const d = st.database, k = st.disk;
    return kv([
      ['Database', badge(d.healthy ? 'healthy' : 'unhealthy')],
      ['Driver', d.driver],
      ['Schema version', d.schema_version],
      ['Size', fmtBytes(d.size_bytes)],
      ['Disk free', k ? `${fmtBytes(k.free_bytes)} of ${fmtBytes(k.total_bytes)}` : null],
      ['Configuration', badge(st.config && st.config.valid ? 'valid' : 'invalid')],
    ]);
  });
  const recent = panel('Recent messages', async () => table(messageCols(true), await get('/messages', { limit: 8 }), { onRow: (m) => openMessage(m.id, () => recent.reload(true)), empty: 'No messages yet.' }), { cls: 'span2' });
  const events = panel('Recent events', async () => table([
    { label: 'Time', get: (e) => fmtTime(e.timestamp) },
    { label: 'Event', cls: 'mono', get: (e) => e.type },
    { label: 'Summary', get: eventSummary },
  ], await get('/events', { limit: 12 }), { empty: 'No events yet.' }), { cls: 'span2' });
  const grid = h('div', { class: 'grid' });
  root.appendChild(grid);
  return mountPanels(grid, [hints, daemon, db, counters, modems, activity, integ, recent, events]);
}

// ---- inbox / outbox / history ---------------------------------------------------------

const STATUSES = ['received', 'queued', 'scheduled', 'processing', 'retrying', 'submitted', 'delivered', 'delivery_failed', 'failed', 'expired', 'cancelled', 'submission_unknown'];
const TYPES = ['sms', 'flash', 'binary', 'sim_toolkit', 'service', 'mms_notification', 'wap_push', 'raw_pdu', 'unknown'];

function messagesPage(title, direction) {
  return (root) => {
    const f = { limit: 50, offset: 0 };
    const input = (name, attrs) => h('input', Object.assign({ name, type: 'search' }, attrs || {}));
    const ctl = {
      search: input('search', { placeholder: 'Text, sender or recipient' }),
      from: input('from', { type: 'date' }),
      to: input('to', { type: 'date' }),
      sender: input('sender'),
      recipient: input('recipient'),
      type: select([['', 'Any'], ...TYPES]),
      status: select([['', 'Any'], ...STATUSES]),
      direction: direction ? null : select([['', 'Both'], 'inbound', 'outbound']),
      limit: select(['25', '50', '100', '250'], { value: '50' }),
    };
    const filters = () => {
      const q = { direction: direction || ctl.direction.value };
      for (const k of ['search', 'from', 'to', 'sender', 'recipient', 'type', 'status']) q[k] = ctl[k].value.trim();
      return q;
    };
    const list = panel(title, async () => {
      const r = await api('/api/v1/messages', { query: Object.assign(filters(), { limit: f.limit, offset: f.offset }) });
      return [
        table(messageCols(!direction), r.data, { onRow: (m) => openMessage(m.id, () => list.reload(true)), empty: 'No messages match.' }),
        pager(r.pagination, (o) => { f.offset = o; list.reload(true); }),
      ];
    }, { tools: h('div', { class: 'row' },
      button('Export CSV', (e) => busy(e.target, () => download('/api/v1/history/export', Object.assign(filters(), { format: 'csv' }), 'smsgateway-messages.csv')), 'small'),
      button('Export JSON', (e) => busy(e.target, () => download('/api/v1/history/export', Object.assign(filters(), { format: 'json' }), 'smsgateway-messages.json')), 'small')) });
    const apply = () => { f.offset = 0; f.limit = Number(ctl.limit.value) || 50; list.reload(true); };
    const form = h('form', { class: 'card filters', onsubmit: (e) => { e.preventDefault(); apply(); } },
      field('Search', ctl.search), field('From date', ctl.from), field('To date', ctl.to), field('Sender', ctl.sender), field('Recipient', ctl.recipient),
      field('Type', ctl.type), field('Status', ctl.status), ctl.direction ? field('Direction', ctl.direction) : null, field('Page size', ctl.limit),
      h('div', { class: 'row end' }, h('button', { type: 'submit', class: 'btn primary small' }, 'Apply'), button('Reset', () => { form.reset(); ctl.limit.value = '50'; apply(); }, 'ghost small')));
    root.appendChild(form);
    return mountPanels(root, [list]);
  };
}

export const inbox = messagesPage('Inbox', 'inbound');
export const outbox = messagesPage('Outbox', 'outbound');
export const history = messagesPage('Message history', '');

// ---- queue ----------------------------------------------------------------------------

const UNKNOWN_NOTE = 'submission_unknown: the modem may or may not have sent this message; retrying can send it twice.';
const CANCELLABLE = ['queued', 'scheduled', 'retrying'];
const RETRYABLE = ['failed', 'expired', 'cancelled', 'submission_unknown', 'delivery_failed', 'retrying'];

export function queue(root) {
  const f = { offset: 0 };
  const status = select([['', 'Active (waiting or uncertain)'], ['all', 'All'], 'queued', 'scheduled', 'processing', 'retrying', 'submission_unknown', 'submitted', 'delivered', 'delivery_failed', 'failed', 'expired', 'cancelled'], { 'aria-label': 'Queue status' });
  const act = async (e, q, what) => {
    const risky = what === 'retry' && q.status === 'submission_unknown';
    const text = what === 'cancel' ? `Cancel the queued message to ${q.recipient}?` : `Queue the message to ${q.recipient} for sending again?` + (risky ? ' The modem may already have sent it, so the recipient can receive it twice.' : '');
    if (!(await confirmBox(what === 'cancel' ? 'Cancel message' : 'Retry message', text, what === 'cancel' ? 'Cancel message' : 'Retry', what === 'cancel' || risky))) return;
    await busy(e.target, async () => {
      const r = await post(`/queue/${encodeURIComponent(q.message_id)}/${what}`);
      toast(`Message is now ${r.status}`, 'ok');
      list.reload(true);
    });
  };
  const list = panel('Message queue', async () => {
    const r = await api('/api/v1/queue', { query: { status: status.value, limit: 50, offset: f.offset } });
    return [
      table([
        { label: 'Created', get: (q) => fmtTime(q.created_at) },
        { label: 'Recipient', get: (q) => q.recipient },
        { label: 'Status', get: (q) => badge(q.status, null, q.status === 'submission_unknown' ? UNKNOWN_NOTE : null) },
        { label: 'Priority', get: (q) => q.priority },
        { label: 'Parts', get: (q) => q.parts },
        { label: 'Attempts', get: (q) => q.attempts },
        { label: 'Modem', get: (q) => q.claimed_by || q.modem_id },
        { label: 'Not before', get: (q) => fmtTime(q.not_before || q.next_attempt_at) },
        { label: 'Last error', get: (q) => q.last_error },
        { label: 'Actions', get: (q) => h('span', { class: 'row' },
          CANCELLABLE.includes(q.status) ? button('Cancel', (e) => act(e, q, 'cancel'), 'small') : null,
          RETRYABLE.includes(q.status) ? button('Retry', (e) => act(e, q, 'retry'), 'small') : null) },
      ], r.data, { onRow: (q) => openMessage(q.message_id, () => list.reload(true)), empty: 'The queue is empty for this filter.' }),
      pager(r.pagination, (o) => { f.offset = o; list.reload(true); }),
    ];
  }, { tools: status });
  status.addEventListener('change', () => { f.offset = 0; list.reload(true); });
  root.appendChild(notice('info', h('strong', null, 'About uncertain submissions. '), UNKNOWN_NOTE, ' Retry is an administrative action.'));
  return mountPanels(root, [list]);
}

// Threats: detected inbound SMS attacks on honeypot numbers, with statistics.

import {
  app, h, s, fill, get, fmtTime, dash, badge, button, table, pager, panel, mountPanels, select,
} from './core.js';
import { openMessage } from './pages-main.js';

const CAT_NAME = {
  simjacker: 'Simjacker (S@T)',
  wibattack: 'WIBattack (WIB)',
  sim_ota: 'SIM OTA',
  silent_sms: 'Silent SMS',
  oma_cp: 'WAP provisioning',
  smishing: 'Smishing',
};
const catName = (c) => CAT_NAME[c] || c || 'unknown';
const sevKind = (sev) => (sev === 'high' ? 'bad' : sev === 'medium' ? 'warn' : 'muted');

/** Confidence as a labelled meter. */
function confidence(v) {
  const n = Math.max(0, Math.min(100, Number(v) || 0));
  const bar = h('span', { class: 'meter-fill ' + (n >= 70 ? 'bad' : n >= 45 ? 'warn' : 'ok') });
  bar.style.width = n + '%';
  return h('span', { class: 'signal' }, h('span', { class: 'meter', role: 'meter', 'aria-valuenow': n, 'aria-valuemin': 0, 'aria-valuemax': 100 }, bar), h('span', { class: 'nowrap' }, n + '%'));
}

const tile = (label, value, note) => h('div', { class: 'tile' }, h('div', { class: 'tile-label' }, label), h('div', { class: 'tile-value' }, value), note ? h('div', { class: 'muted small-text' }, note) : null);

export function threats(root) {
  let range = '30d';
  let category = '';
  let offset = 0;

  const rangeSel = select([['24h', 'Last 24 hours'], ['7d', 'Last 7 days'], ['30d', 'Last 30 days'], ['all', 'All time']], { value: range });
  const catSel = select([['', 'All categories']].concat(Object.keys(CAT_NAME).map((c) => [c, catName(c)])), { value: category });

  const stats = panel('Attack statistics', async () => {
    const d = await get('/security/stats', { range });
    if (!d.total) return h('p', { class: 'muted' }, 'No attacks detected in this period. Detections appear here as honeypot numbers receive SIM-toolkit pushes, silent SMS, provisioning pushes or smishing.');
    const byCat = d.by_category || [];
    const topCat = byCat[0] ? `${catName(byCat[0].k)} (${byCat[0].count})` : '—';
    return [
      h('div', { class: 'tiles' },
        tile('Total attacks', d.total, d.last_seen ? 'last ' + new Date(d.last_seen).toLocaleString() : null),
        tile('Distinct senders', d.distinct_senders ?? 0, 'attacker numbers'),
        tile('Targeted numbers', d.targeted_numbers ?? 0, 'honeypot SIMs hit'),
        tile('Avg confidence', d.avg_confidence == null ? '—' : Math.round(d.avg_confidence) + '%', 'peak ' + (d.max_confidence ?? 0) + '%'),
        tile('Most seen', topCat)),
      h('div', { class: 'grid' },
        h('section', { class: 'card bare' }, h('h3', null, 'By attack type'),
          table([
            { label: 'Attack', get: (r) => badge(catName(r.k), 'bad') },
            { label: 'Count', get: (r) => r.count },
            { label: 'Avg', get: (r) => (r.avg_confidence == null ? '—' : Math.round(r.avg_confidence) + '%') },
            { label: 'Peak', get: (r) => (r.max_confidence ?? 0) + '%' },
            { label: 'Last seen', get: (r) => fmtTime(r.last_seen) },
          ], byCat, { empty: 'None.' })),
        h('section', { class: 'card bare' }, h('h3', null, 'By targeted number'),
          table([
            { label: 'Honeypot number', cls: 'mono', get: (r) => dash(r.k) },
            { label: 'Attacks', get: (r) => r.count },
            { label: 'Senders', get: (r) => r.distinct_senders },
            { label: 'Peak', get: (r) => (r.max_confidence ?? 0) + '%' },
            { label: 'Last seen', get: (r) => fmtTime(r.last_seen) },
          ], d.by_number || [], { empty: 'None.' }))),
    ];
  });

  const list = panel('Detections', async () => {
    const d = await get('/security/events', { range: undefined, category: category || undefined, limit: 25, offset });
    const rows = d.events || [];
    return [
      table([
        { label: 'Time', get: (r) => fmtTime(r.created_at) },
        { label: 'Attack', get: (r) => badge(catName(r.category), 'bad', r.detail) },
        { label: 'Confidence', get: (r) => confidence(r.confidence) },
        { label: 'Severity', get: (r) => badge(r.severity, sevKind(r.severity)) },
        { label: 'From (sender)', cls: 'mono', get: (r) => dash(r.sender) },
        { label: 'To (honeypot)', cls: 'mono', get: (r) => dash(r.recipient) },
        { label: 'Detail', cls: 'clip', get: (r) => h('span', { title: r.detail }, r.detail) },
      ], rows, { empty: 'No detections.', onRow: (r) => r.message_id && openMessage(r.message_id) }),
      pager({ total: d.total, limit: 25, offset }, (o) => { offset = o; list.reload(true); }),
    ];
  });

  const page = mountPanels(root, [stats, list]);
  rangeSel.addEventListener('change', () => { range = rangeSel.value; stats.reload(true); });
  catSel.addEventListener('change', () => { category = catSel.value; offset = 0; list.reload(true); });
  // Toolbar above the panels.
  root.insertBefore(h('div', { class: 'toolbar' },
    h('label', { class: 'field inline' }, h('span', { class: 'field-label' }, 'Period'), rangeSel),
    h('label', { class: 'field inline' }, h('span', { class: 'field-label' }, 'Category'), catSel),
    h('span', { class: 'grow' }),
    h('a', { class: 'btn small ghost', href: '/docs' }, 'API'),
  ), stats.el);

  return { refresh: () => page.refresh(false), destroy: () => {} };
}

// PDU Tools, Calls, Integrations, Modem, System and API Documentation.

import {
  app, h, fill, api, request, get, post, download, fmtTime, fmtBytes, fmtDuration, hidden, badge, yesno,
  errorBox, notice, button, busy, kv, tree, table, pager, panel, mountPanels, confirmBox, toast, field,
  select, check, signal, eventSummary,
} from './core.js';

const sub = (title, ...kids) => h('section', { class: 'sub' }, h('h3', null, title), kids);
const list = (v) => (Array.isArray(v) && v.length ? v.join(', ') : null);
const num = (input) => (input.value.trim() === '' ? undefined : Number(input.value));

// ---- PDU tools -----------------------------------------------------------------------

export function pdu(root) {
  const offline = () => badge('nothing transmitted', 'ok');
  // Encoder
  const to = h('input', { type: 'tel', placeholder: '+4500000000', required: true });
  const kind = select([['text', 'Text message'], ['binary', 'Binary payload (hex)']]);
  const text = h('textarea', { rows: 4, placeholder: 'Message text' });
  const hex = h('input', { type: 'text', class: 'mono', placeholder: '0605040B8423F0…' });
  const hexField = field('Binary payload (hexadecimal)', hex);
  const textField = field('Message', text);
  hexField.hidden = true;
  kind.addEventListener('change', () => {
    hexField.hidden = kind.value !== 'binary';
    textField.hidden = kind.value === 'binary';
  });
  const enc = select([['auto', 'Automatic'], ['gsm7', 'GSM 7-bit'], ['ucs2', 'UCS-2'], ['8bit', '8-bit']]);
  const flash = check('Flash (class 0)');
  const report = check('Request delivery report');
  const validity = h('input', { type: 'number', min: 1, placeholder: 'default' });
  const dport = h('input', { type: 'number', min: 0, max: 65535 });
  const sport = h('input', { type: 'number', min: 0, max: 65535 });
  const smsc = h('input', { type: 'tel', placeholder: 'none (modem default)' });
  const encOut = h('div', { 'aria-live': 'polite' });
  const encForm = h('form', { class: 'card form', onsubmit: (e) => {
    e.preventDefault();
    busy(encForm.querySelector('button[type=submit]'), async () => {
      const body = { to: to.value.trim(), encoding: enc.value, flash: flash.input.checked, delivery_report: report.input.checked };
      if (kind.value === 'binary') body.binary = hex.value.replace(/\s+/g, '');
      else body.message = text.value;
      const add = (k, v) => v !== undefined && !Number.isNaN(v) && (body[k] = v);
      add('validity_minutes', num(validity));
      add('destination_port', num(dport));
      add('source_port', num(sport));
      if (smsc.value.trim()) body.smsc = smsc.value.trim();
      try {
        const r = await post('/pdu/encode', body);
        const e2 = r.encoded || {};
        fill(encOut,
          h('div', { class: 'row wrap' }, r.transmitted === false ? offline() : badge('transmitted: ' + r.transmitted, 'bad'), badge('encoding: ' + e2.encoding, 'info'), badge(`${(e2.parts || []).length} part(s)`, 'info')),
          (e2.parts || []).map((p) => sub(`Part ${p.sequence} — TPDU length ${p.tpdu_length}`,
            h('pre', { class: 'pdu' }, p.pdu),
            button('Copy PDU', () => navigator.clipboard ? navigator.clipboard.writeText(p.pdu).then(() => toast('Copied', 'ok'), () => toast('Copy failed', 'bad')) : toast('Clipboard is not available', 'bad'), 'small'))),
          sub('Structured result', tree(e2)));
      } catch (err) {
        fill(encOut, notice('bad', h('strong', null, 'Cannot encode: '), err.message));
      }
    });
  } },
    h('header', { class: 'card-head' }, h('h2', null, 'Encode SMS-SUBMIT'), offline()),
    field('Destination number', to), field('Payload', kind), textField, hexField,
    h('div', { class: 'cols' }, field('Encoding', enc), field('Validity (minutes)', validity), field('Destination port', dport), field('Source port', sport), field('Service centre (SMSC)', smsc)),
    h('div', { class: 'row wrap' }, flash.el, report.el),
    h('div', { class: 'row' }, h('button', { type: 'submit', class: 'btn primary' }, 'Encode')),
    encOut);

  // Decoder
  const raw = h('textarea', { rows: 4, class: 'mono', placeholder: '07917283010010F5040BC87238880900F1…', required: true });
  const dir = select([['auto', 'Detect automatically'], ['deliver', 'Deliver (received from the network)'], ['submit', 'Submit (sent to the network)']]);
  const hasSmsc = check('PDU starts with a service centre address', { checked: true });
  const decOut = h('div', { 'aria-live': 'polite' });
  const decForm = h('form', { class: 'card form', onsubmit: (e) => {
    e.preventDefault();
    busy(decForm.querySelector('button[type=submit]'), async () => {
      const body = { pdu: raw.value.replace(/\s+/g, ''), direction: dir.value };
      if (dir.value !== 'auto') body.smsc = hasSmsc.input.checked;
      try {
        const r = await post('/pdu/decode', body);
        const ud = (r.decoded && r.decoded.user_data) || {};
        fill(decOut,
          h('div', { class: 'row wrap' }, offline(), badge('type: ' + (r.decoded && r.decoded.type), 'info'), ud.encoding ? badge('encoding: ' + ud.encoding, 'info') : null),
          ud.text !== undefined && ud.text !== null ? sub('Text', h('pre', { class: 'body' }, ud.text)) : null,
          (ud.warnings || []).length ? notice('warn', h('strong', null, 'Warnings: '), ud.warnings.join('; ')) : null,
          sub('Decoded fields', tree(r.decoded)),
          r.mms_notification ? sub('MMS notification', tree(r.mms_notification)) : null);
      } catch (err) {
        fill(decOut, notice('bad', h('strong', null, 'Invalid PDU: '), err.message));
      }
    });
  } },
    h('header', { class: 'card-head' }, h('h2', null, 'Decode PDU'), offline()),
    field('PDU (hexadecimal)', raw),
    h('div', { class: 'cols' }, field('Direction', dir)),
    hasSmsc.el,
    h('div', { class: 'row' }, h('button', { type: 'submit', class: 'btn primary' }, 'Decode')),
    decOut);

  root.append(notice('info', h('strong', null, 'Offline tools. '), 'Encoding and decoding happen inside the gateway process only. Nothing on this page is transmitted to the modem or the mobile network.'), encForm, decForm);
  return {};
}

// ---- calls ---------------------------------------------------------------------------

export function calls(root) {
  const f = { calls: 0, rec: 0 };
  const log = panel('Call log', async () => {
    const r = await api('/api/v1/calls', { query: { limit: 50, offset: f.calls } });
    return [table([
      { label: 'Started', get: (c) => fmtTime(c.started_at) },
      { label: 'Direction', get: (c) => badge(c.direction, 'info') },
      { label: 'Number', get: (c) => c.number },
      { label: 'State', get: (c) => badge(c.state) },
      { label: 'Outcome', get: (c) => (c.missed ? badge('missed') : c.answered ? badge('answered') : null) },
      { label: 'Duration', get: (c) => (c.duration_seconds === null || c.duration_seconds === undefined ? null : fmtDuration(c.duration_seconds)) },
      { label: 'Ended', get: (c) => fmtTime(c.ended_at) },
      { label: 'Reason', get: (c) => c.termination_reason },
      { label: 'Modem', get: (c) => c.modem_id },
      { label: 'Recording', get: (c) => c.recording_status },
    ], r.data, { empty: 'No calls recorded.' }), pager(r.pagination, (o) => { f.calls = o; log.reload(true); })];
  });
  const rec = panel('Recordings', async () => {
    const r = await api('/api/v1/recordings', { query: { limit: 50, offset: f.rec } });
    return [table([
      { label: 'Created', get: (x) => fmtTime(x.created_at) },
      { label: 'Call', cls: 'mono', get: (x) => x.call_id },
      { label: 'Format', get: (x) => x.format },
      { label: 'Duration', get: (x) => (x.duration_seconds === null || x.duration_seconds === undefined ? null : fmtDuration(x.duration_seconds)) },
      { label: 'Size', get: (x) => fmtBytes(x.size_bytes) },
      { label: 'Download', get: (x) => button('Download', (e) => busy(e.target, () => download(`/api/v1/recordings/${encodeURIComponent(x.id)}/download`, null, `recording-${x.id}.wav`)), 'small') },
    ], r.data, { empty: 'No recordings stored.' }), pager(r.pagination, (o) => { f.rec = o; rec.reload(true); })];
  });
  return mountPanels(root, [log, rec]);
}

// ---- integrations ----------------------------------------------------------------------

export function integrations(root) {
  const outcomes = {};
  const f = { offset: 0, dead: 0 };
  const reloadAll = () => [main, deliveries, dead].forEach((p) => p.reload(true));
  const replay = (id) => async (e) => {
    if (!(await confirmBox('Replay delivery', 'Queue this delivery for another attempt? The integration will receive the event again.', 'Replay'))) return;
    await busy(e.target, async () => {
      await post(`/deliveries/${encodeURIComponent(id)}/replay`);
      toast('Delivery queued again', 'ok');
      reloadAll();
    });
  };
  const test = async (e, i) => {
    if (!(await confirmBox('Send test event', `Send a synthetic test event to the integration "${i.name}" now? The external system will receive it.`, 'Send test event'))) return;
    await busy(e.target, async () => {
      try {
        const r = await post(`/integrations/${encodeURIComponent(i.name)}/test`);
        outcomes[i.name] = { at: new Date().toISOString(), outcome: r.outcome || {} };
      } catch (err) {
        outcomes[i.name] = { at: new Date().toISOString(), outcome: { success: false, error: err.message } };
      }
      main.reload(true);
    });
  };
  const lastAttempt = (a) => (a ? h('span', null, fmtTime(a.created_at), ' ', badge(a.success ? 'success' : 'failed'), a.status_code ? ` HTTP ${a.status_code}` : '', a.error ? ` — ${a.error}` : '') : null);
  const main = panel('Integrations', async () => {
    const d = await get('/integrations');
    return [
      d.enabled ? null : notice('warn', 'Integrations are disabled globally (integrations.enabled = false).'),
      table([
        { label: 'Name', get: (i) => h('strong', null, i.name) },
        { label: 'Type', get: (i) => i.type + (i.method ? ` (${i.method})` : '') },
        { label: 'Enabled', get: (i) => yesno(i.enabled, 'enabled', 'disabled') },
        { label: 'Events', get: (i) => list(i.events) },
        { label: 'Delivered', get: (i) => i.stats && i.stats.delivered },
        { label: 'Pending', get: (i) => i.stats && i.stats.pending },
        { label: 'Retrying', get: (i) => i.stats && i.stats.retrying },
        { label: 'Dead', get: (i) => i.stats && i.stats.dead },
        { label: 'Last attempt', get: (i) => lastAttempt(i.stats && i.stats.last_attempt) },
        { label: 'Last success', get: (i) => fmtTime(i.stats && i.stats.last_success_at) },
        { label: 'Test', get: (i) => button('Send test event', (e) => test(e, i), 'small') },
      ], d.integrations, { empty: 'No integrations configured.' }),
      Object.keys(outcomes).map((name) => {
        const o = outcomes[name].outcome;
        return notice(o.success ? 'ok' : 'bad', h('strong', null, `Test of "${name}": `), badge(o.success ? 'success' : 'failed'), ' ',
          [o.status_code ? `HTTP ${o.status_code}` : null, typeof o.duration_ms === 'number' ? `${o.duration_ms} ms` : null, o.error, o.retryable ? 'retryable' : null].filter(Boolean).join(' · '),
          ' ', h('span', { class: 'muted' }, '(', fmtTime(outcomes[name].at), ')'));
      }),
      sub('Routing rules', table([
        { label: 'Name', get: (r) => r.name },
        { label: 'Enabled', get: (r) => yesno(r.enabled, 'enabled', 'disabled') },
        { label: 'Events', get: (r) => list(r.events) || 'all' },
        { label: 'Destinations', get: (r) => list(r.destinations) },
        { label: 'Stops processing', get: (r) => yesno(r.stop_processing) },
      ], d.routing_rules, { empty: 'No routing rules: events go to every subscribed integration.' })),
      sub('Automation rules', table([
        { label: 'Name', get: (r) => r.name },
        { label: 'Enabled', get: (r) => yesno(r.enabled, 'enabled', 'disabled') },
        { label: 'Action', get: (r) => r.action },
      ], d.automation_rules, { empty: 'No automation rules.' })),
    ];
  });
  const status = select([['', 'Any status'], 'pending', 'processing', 'retrying', 'delivered', 'dead'], { 'aria-label': 'Delivery status' });
  status.addEventListener('change', () => { f.offset = 0; deliveries.reload(true); });
  const deliveries = panel('Recent deliveries', async () => {
    const r = await api('/api/v1/deliveries', { query: { status: status.value, limit: 25, offset: f.offset } });
    return [table([
      { label: 'Created', get: (x) => fmtTime(x.created_at) },
      { label: 'Integration', get: (x) => x.integration },
      { label: 'Event', cls: 'mono', get: (x) => x.event_type },
      { label: 'Status', get: (x) => badge(x.status) },
      { label: 'Attempts', get: (x) => x.attempts },
      { label: 'Next attempt', get: (x) => fmtTime(x.next_attempt_at) },
      { label: 'Delivered', get: (x) => fmtTime(x.delivered_at) },
      { label: 'Last error', get: (x) => x.last_error },
      { label: 'Action', get: (x) => (x.status === 'dead' || x.status === 'retrying' ? button('Replay', replay(x.id), 'small') : null) },
    ], r.data, { empty: 'No deliveries.' }), pager(r.pagination, (o) => { f.offset = o; deliveries.reload(true); })];
  }, { tools: status });
  const dead = panel('Dead letters', async () => {
    const r = await api('/api/v1/dead-letters', { query: { limit: 25, offset: f.dead } });
    return [table([
      { label: 'Created', get: (x) => fmtTime(x.created_at) },
      { label: 'Kind', get: (x) => x.kind },
      { label: 'Integration', get: (x) => x.integration },
      { label: 'Reason', get: (x) => x.reason },
      { label: 'Reference', cls: 'mono', get: (x) => x.reference_id },
    ], r.data, { empty: 'The dead-letter queue is empty.' }), pager(r.pagination, (o) => { f.dead = o; dead.reload(true); })];
  });
  return mountPanels(root, [main, deliveries, dead]);
}

// ---- modem ---------------------------------------------------------------------------

const CAP_LABELS = { delivery_report_routing: 'Delivery report routing', voice: 'Voice calls', caller_id: 'Caller ID', ussd: 'USSD', gnss: 'GNSS', mms: 'MMS', bluetooth: 'Bluetooth' };

function capabilities(c) {
  const audio = c.audio || {};
  return [
    h('div', { class: 'caps' }, Object.keys(CAP_LABELS).map((k) => h('div', { class: 'cap' }, h('span', null, CAP_LABELS[k]), badge(c[k] || 'unknown'))),
      h('div', { class: 'cap' }, h('span', null, 'Call recording'), badge(audio.call_recording || 'unknown'))),
    kv([
      ['Detected from modem', yesno(c.detected)],
      ['SMS modes', list(c.sms_modes)],
      ['Character sets', list(c.character_sets)],
      ['SMS storages', list(c.sms_storages)],
      ['Network technologies', list(c.network_technologies)],
      ['Baud rates', list(c.baud_rates)],
      ['Modem audio interfaces', list(audio.modem_interfaces)],
      ['Host capture devices', list(audio.host_capture_devices)],
    ]),
  ];
}

export function modem(root) {
  const ussd = {};
  const info = () => app.cfg.show_modem_information !== false;
  const secret = (v) => (info() ? (v ? h('span', { class: 'mono' }, v) : null) : hidden());
  const view = panel(null, async () => {
    const [modems, sims, gnss] = await Promise.all([get('/modems'), get('/sims'), get('/gnss').catch((e) => ({ error: e }))]);
    if (!modems.length) return h('p', { class: 'muted' }, 'No modems configured.');
    return modems.map((m) => {
      const sim = sims.find((x) => x.modem === m.id) || {};
      const det = sim.detected || {}, conf = sim.configured;
      const st = m.storage || {}, net = m.network || {}, sg = m.signal || {};
      const out = ussd[m.id];
      return h('section', { class: 'card' },
        h('header', { class: 'card-head' }, h('h2', null, 'Modem ', h('span', { class: 'mono' }, m.id)), badge(m.state), h('span', { class: 'muted' }, 'since ', fmtTime(m.state_since)),
          h('span', { class: 'grow' }),
          button('Reconnect', async (e) => {
            if (!(await confirmBox('Reconnect modem', `Drop and re-establish the connection to ${m.id}? Sending and receiving pause until the modem is ready again.`, 'Reconnect'))) return;
            await busy(e.target, async () => {
              await post('/modem/reconnect', undefined, { modem: m.id });
              toast(`Reconnecting ${m.id}`, 'ok');
              setTimeout(() => view.reload(true), 1500);
            });
          }, 'small')),
        m.hint ? notice('warn hint', h('strong', null, 'Action needed: '), m.hint) : null,
        m.last_error ? notice('bad', h('strong', null, 'Last error: '), m.last_error) : null,
        h('div', { class: 'subgrid' },
          sub('Device', kv([['Serial device', h('span', { class: 'mono' }, m.device)], ['Baud rate', m.baud_rate], ['Profile', m.profile], ['Enabled', yesno(m.enabled)], ['Connected', fmtTime(m.connected_at)], ['Last seen', fmtTime(m.last_seen_at)], ['Reconnect attempts', m.reconnect_attempts]])),
          sub('Identification', kv([['Manufacturer', m.manufacturer], ['Model', m.model], ['Firmware', secret(m.firmware)], ['IMEI', secret(m.imei)]])),
          sub('Network', kv([['Registration', badge(net.registration)], ['Registered', yesno(net.registered)], ['Operator', net.operator], ['Technology', net.technology], ['Signal', signal(sg.percent, sg.rssi_dbm)], ['Raw signal value', sg.raw]])),
          sub('SIM (detected)', kv([['State', badge(sim.state || (m.sim && m.sim.state))], ['ICCID', secret(det.iccid)], ['IMSI', secret(det.imsi)], ['Number on SIM', det.msisdn], ['Service centre', det.smsc], ['Operator', det.operator]])),
          sub('SIM (configured)', conf ? kv([['Profile', conf.id], ['Name', conf.name], ['Gateway number', conf.mobile_number ? h('span', { class: 'mono' }, conf.mobile_number) : null], ['Carrier', conf.carrier], ['Owner', conf.owner], ['Country code', conf.country_code], ['PIN enabled', yesno(conf.pin_enabled)], ['Service centre', conf.smsc], ['Network selection', conf.network && conf.network.selection], ['Roaming', conf.network ? yesno(conf.network.roaming_enabled, 'allowed', 'not allowed') : null], ['Description', conf.description]]) : h('p', { class: 'muted' }, 'No SIM profile is bound to this modem.'),
            sim.note ? h('p', { class: 'muted small-text' }, sim.note) : null),
          sub('SMS storage', kv([['Storage', st.name], ['Used', st.used === null || st.used === undefined ? null : `${st.used} of ${st.total === null || st.total === undefined ? '?' : st.total}`]]))),
        sub('Capabilities', capabilities(m.capabilities || {})),
        gnss && gnss.modem === m.id ? sub('GNSS', kv([['Enabled', yesno(gnss.enabled)], ['Capability', badge(gnss.capability)], ['Last fix', fmtTime(gnss.last_fix_at)]]), gnss.position ? tree(gnss.position) : h('p', { class: 'muted' }, 'No position fix available.')) : null,
        sub('USSD',
          (app.cfg.ussd_commands || []).length
            ? h('div', { class: 'row wrap' }, app.cfg.ussd_commands.map((name) => button(name, (e) => busy(e.target, async () => {
              try {
                const r = await post('/ussd', { command: name, modem: m.id });
                ussd[m.id] = { name, code: r.code, text: r.response, at: new Date().toISOString() };
              } catch (err) {
                ussd[m.id] = { name, error: err.message, at: new Date().toISOString() };
              }
              view.reload(true);
            }), 'small')))
            : h('p', { class: 'muted' }, 'No USSD commands are configured ([ussd.commands]).'),
          out ? (out.error ? notice('bad', h('strong', null, `${out.name}: `), out.error) : h('div', { class: 'notice info' }, h('strong', null, `${out.name} (${out.code}) `), h('span', { class: 'muted' }, fmtTime(out.at)), h('pre', { class: 'body' }, out.text))) : null));
    });
  }, { cls: 'bare' });
  return mountPanels(root, [view]);
}

// ---- system ----------------------------------------------------------------------------

export function system(root) {
  const version = panel('Version and build', async () => {
    const [v, st] = await Promise.all([get('/version'), get('/status')]);
    return kv([['Name', v.name], ['Version', v.version], ['Commit', h('span', { class: 'mono' }, v.commit)], ['Build date', v.build_date], ['Target', v.target], ['Instance', `${st.daemon.instance_name} (${st.daemon.instance_id})`], ['Uptime', fmtDuration(st.daemon.uptime_seconds)], ['Started', fmtTime(st.daemon.started_at)]]);
  });
  const database = panel('Database and disk', async () => {
    const [st, stats] = await Promise.all([get('/status'), get('/stats')]);
    const d = st.database, k = st.disk, t = stats.tables || {};
    const used = k && k.total_bytes ? (100 * (k.total_bytes - k.free_bytes)) / k.total_bytes : null;
    return [
      kv([['Health', badge(d.healthy ? 'healthy' : 'unhealthy')], ['Driver', d.driver], ['Schema version', d.schema_version], ['Database size', fmtBytes(d.size_bytes)],
        ['Disk', k ? `${fmtBytes(k.free_bytes)} free of ${fmtBytes(k.total_bytes)}` + (used === null ? '' : ` (${used.toFixed(1)} % used)`) : null]]),
      sub('Rows per table', table([{ label: 'Table', cls: 'mono', get: (x) => x[0] }, { label: 'Rows', get: (x) => x[1] }], Object.entries(t))),
    ];
  });
  const stats = panel('Delivery statistics', async () => {
    const x = await get('/stats');
    const i = x.integrations || {}, m = x.messages || {};
    return kv([
      ['Integration deliveries', `${i.delivered} delivered · ${i.pending} pending · ${i.dead} dead`],
      ['Outbound messages', `${m.outbound_total} total · ${m.sent} sent · ${m.delivered} delivered · ${m.delivery_failed} not delivered · ${m.failed} failed · ${m.submission_unknown} unknown`],
      ['Inbound messages', m.received], ['Queue pending', x.queue && x.queue.pending], ['Calls', x.calls && x.calls.total],
    ]);
  });
  const caps = panel('Application capabilities', async () => {
    const g = (await get('/capabilities')).gateway || {};
    return kv(Object.keys(g).map((k) => [k.replace(/_/g, ' '), typeof g[k] === 'boolean' ? yesno(g[k]) : Array.isArray(g[k]) ? list(g[k]) : typeof g[k] === 'string' ? badge(g[k]) : g[k]]));
  }, { cls: 'span2' });
  const config = panel('Configuration', async () => {
    const c = await get('/config/status');
    const st = c.status || {}, files = c.files || {};
    return [
      kv([['Validation', badge(st.valid ? 'valid' : 'invalid')], ['Loaded', fmtTime(st.loaded_at)], ['Last reload', fmtTime(st.last_reload_at)],
        ['Restart required', (st.restart_required || []).length ? h('span', null, badge('restart required', 'warn'), ' ', st.restart_required.join(', ')) : 'no'],
        ['Files', h('span', { class: 'mono' }, Object.values(files).filter(Boolean).join(', '))]]),
      st.last_reload_error ? notice('bad', h('strong', null, 'Last reload failed: '), st.last_reload_error) : null,
      (st.warnings || []).length ? notice('warn', h('strong', null, 'Warnings'), h('ul', null, st.warnings.map((w) => h('li', null, w)))) : h('p', { class: 'muted' }, 'No configuration warnings.'),
    ];
  }, { tools: button('Reload configuration', async (e) => {
    if (!(await confirmBox('Reload configuration', 'Read the configuration files again and apply them? An invalid configuration is rejected and the current one stays active.', 'Reload'))) return;
    await busy(e.target, async () => {
      const r = await post('/config/reload');
      toast((r.restart_required || []).length ? 'Reloaded; restart needed for: ' + r.restart_required.join(', ') : 'Configuration reloaded', 'ok');
    });
    config.reload(true);
  }, 'small') });
  const book = panel('Address book', async () => {
    const b = await get('/addressbook');
    const a = b.anonymous || {};
    return [table([{ label: 'Name', get: (c) => c.name }, { label: 'Number', cls: 'mono', get: (c) => c.number }], b.contacts, { empty: 'The address book is empty.' }),
      h('p', { class: 'muted small-text' }, `${a.count || 0} number(s) are always anonymised towards integrations (shown as "${a.label}").`)];
  });
  const events = panel('Recent events', async () => table([
    { label: 'Time', get: (e) => fmtTime(e.timestamp) },
    { label: 'Event', cls: 'mono', get: (e) => e.type },
    { label: 'Summary', get: eventSummary },
  ], await get('/events', { limit: 25 }), { empty: 'No events.' }), { cls: 'span2' });
  const f = { offset: 0 };
  const audit = panel('Audit trail (administrative)', async () => {
    const r = await api('/api/v1/audit', { query: { limit: 25, offset: f.offset }, noAuthPrompt: true });
    return [table([
      { label: 'Time', get: (a) => fmtTime(a.created_at) },
      { label: 'Actor', get: (a) => a.actor },
      { label: 'Action', cls: 'mono', get: (a) => a.action },
      { label: 'Target', cls: 'mono', get: (a) => a.target },
      { label: 'Detail', get: (a) => a.detail },
    ], r.data, { empty: 'No audit records.' }), pager(r.pagination, (o) => { f.offset = o; audit.reload(true); })];
  }, { cls: 'span2' });
  const grid = h('div', { class: 'grid' });
  root.appendChild(grid);
  return mountPanels(grid, [version, database, stats, config, book, caps, events, audit]);
}

// ---- API documentation -------------------------------------------------------------------

const METHODS = ['get', 'post', 'put', 'patch', 'delete'];
const TEXTUAL = /json|text|csv|xml/i;

function operation(method, path, op) {
  const admin = op['x-admin'] === true;
  const det = h('details', { class: 'op' }, h('summary', null, h('span', { class: 'method m-' + method }, method.toUpperCase()), h('code', null, path), h('span', { class: 'op-sum' }, op.summary || ''), admin ? badge('administrative', 'warn') : null));
  let built = false;
  det.addEventListener('toggle', () => {
    if (!det.open || built) return;
    built = true;
    const params = (op.parameters || []).map((p) => ({ p, input: h('input', { type: 'text', 'aria-label': p.name, placeholder: p.required ? 'required' : '' }) }));
    const example = op.requestBody && op.requestBody.content && op.requestBody.content['application/json'];
    const bodyBox = example ? h('textarea', { rows: 6, class: 'mono', value: JSON.stringify(example.example === undefined ? {} : example.example, null, 2) }) : null;
    const out = h('div', { 'aria-live': 'polite' });
    const stream = path.endsWith('/events/stream');
    const run = async (e) => {
      const query = {}, headers = {};
      let url = path, missing = null;
      for (const { p, input } of params) {
        const v = input.value.trim();
        if (p.in === 'path') {
          if (!v) missing = p.name;
          url = url.replace('{' + p.name + '}', encodeURIComponent(v));
        } else if (v && p.in === 'query') query[p.name] = v;
        else if (v && p.in === 'header') headers[p.name] = v;
      }
      if (missing) return toast(`Path parameter "${missing}" is required.`, 'bad');
      if (bodyBox) {
        try {
          JSON.parse(bodyBox.value);
        } catch (err) {
          return toast('The request body is not valid JSON: ' + err.message, 'bad');
        }
      }
      if (method !== 'get' && !(await confirmBox(`Run ${method.toUpperCase()} ${url}?`, 'This request is executed for real against this gateway. It can change state and, for the SMS endpoints, transmit a real SMS.', 'Run request', true))) return;
      await busy(e.target, async () => {
        const t0 = performance.now();
        const res = await request(url, { method: method.toUpperCase(), query, headers, rawBody: bodyBox ? bodyBox.value : undefined });
        const ms = Math.round(performance.now() - t0);
        const type = res.headers.get('content-type') || '';
        let text;
        if (TEXTUAL.test(type)) {
          text = await res.text();
          if (/json/i.test(type)) {
            try {
              text = JSON.stringify(JSON.parse(text), null, 2);
            } catch (_) { /* shown as received */ }
          }
          if (text.length > 200000) text = text.slice(0, 200000) + '\n… (truncated)';
        } else {
          text = `(${type || 'unknown type'}, ${(await res.blob()).size} bytes; not displayed)`;
        }
        fill(out, h('div', { class: 'row wrap' }, badge(`${res.status} ${res.statusText}`.trim(), res.ok ? 'ok' : 'bad'), h('span', { class: 'muted' }, `${ms} ms · ${type}`), h('span', { class: 'muted mono' }, res.headers.get('x-request-id') || '')), h('pre', { class: 'response' }, text));
      });
    };
    det.append(
      op.description ? h('p', { class: 'muted' }, op.description) : '',
      params.length ? table([
        { label: 'Parameter', cls: 'mono', get: (x) => x.p.name + (x.p.required ? ' *' : '') },
        { label: 'In', get: (x) => x.p.in },
        { label: 'Value', get: (x) => x.input },
        { label: 'Description', get: (x) => x.p.description },
      ], params) : '',
      bodyBox ? field('Request body (JSON)', bodyBox) : '',
      stream ? h('p', { class: 'muted' }, 'This endpoint is an endless event stream and cannot be run from here; the dashboard already consumes it for live updates.')
        : h('div', { class: 'row' }, button(method === 'get' ? 'Try it' : 'Try it (asks for confirmation)', run, 'small ' + (method === 'get' ? 'primary' : 'danger'))),
      out);
  });
  return det;
}

export function apidocs(root) {
  const view = panel(null, async () => {
    const doc = await api('/api/v1/openapi.json');
    const groups = new Map();
    for (const [path, item] of Object.entries(doc.paths || {})) {
      for (const method of METHODS) {
        const op = item[method];
        if (!op) continue;
        const tag = (op.tags && op.tags[0]) || 'Other';
        if (!groups.has(tag)) groups.set(tag, []);
        groups.get(tag).push(operation(method, path, op));
      }
    }
    const info = doc.info || {};
    return [
      h('section', { class: 'card' }, h('header', { class: 'card-head' }, h('h2', null, info.title || 'API'), badge('version ' + info.version, 'info'), badge('OpenAPI ' + doc.openapi, 'info')),
        h('p', null, info.description || ''),
        h('p', { class: 'muted' }, app.cfg.authentication === 'bearer' ? 'This gateway requires a bearer token. ' : 'This gateway does not require a token for ordinary requests. ', app.token ? 'Requests from this page send the token stored for this browser session.' : 'No token is stored for this browser session.', ' The raw document is served at ', h('code', null, '/api/v1/openapi.json'), '.'),
        notice('warn', '"Try it" performs real requests. State-changing operations ask for confirmation first; responses are shown exactly as the API returns them.')),
      [...groups.entries()].map(([tag, ops]) => h('section', { class: 'card' }, h('header', { class: 'card-head' }, h('h2', null, tag), h('span', { class: 'muted' }, `${ops.length} operation${ops.length === 1 ? '' : 's'}`)), ops)),
    ];
  }, { cls: 'bare' });
  root.appendChild(view.el);
  view.reload(true);
  return {};
}

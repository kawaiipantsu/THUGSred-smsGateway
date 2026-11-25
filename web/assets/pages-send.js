// Send SMS composer and Contacts.

import {
  app, h, s, fill, api, get, post, fmtTime, badge, errorBox, notice, button, busy, kv, dialog, confirmBox, toast,
  field, select, check, debounce,
} from './core.js';
import { openMessage } from './pages-main.js';

const FINAL = ['delivered', 'failed', 'delivery_failed', 'cancelled', 'expired', 'submission_unknown', 'malformed'];

// ---- small building blocks ------------------------------------------------------------

const ICONS = {
  send: 'M22 2 11 13M22 2l-7 20-4-9-9-4 20-7Z',
  user: 'M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8Z',
  cloud: 'M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z',
  signal: 'M2 20h.01M7 20v-4M12 20v-8M17 20V8M22 4v16',
  search: 'M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16ZM21 21l-4.35-4.35',
  plus: 'M12 5v14M5 12h14',
  edit: 'M12 20h9M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5Z',
  trash: 'M3 6h18M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2',
  x: 'M18 6 6 18M6 6l12 12',
  check: 'M20 6 9 17l-5-5',
  save: 'M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2ZM17 21v-8H7v8M7 3v5h8',
};

function icon(name) {
  return s('svg', { viewBox: '0 0 24 24', class: 'ico', 'aria-hidden': 'true' }, s('path', { d: ICONS[name] }));
}

const digits = (v) => String(v || '').replace(/\D/g, '');
const looksLikeNumber = (v) => /^\+?[\d\s\-().\/]{3,}$/.test(v.trim()) && digits(v).length >= 3;

/** Round badge with the initials of a name; the colour follows the name. */
function avatar(name, cls) {
  const words = String(name || '').trim().split(/\s+/).filter(Boolean);
  const letters = /[\p{L}\p{N}]/u;
  let initials = words.slice(0, 2).map((w) => [...w].find((c) => letters.test(c)) || '').join('').toUpperCase();
  if (!initials) initials = '#';
  let hash = 0;
  for (const c of String(name || '')) hash = (hash * 31 + c.codePointAt(0)) >>> 0;
  return h('span', { class: `avatar av-${hash % 6} ${cls || ''}`, 'aria-hidden': 'true' }, initials);
}

/** Does a stored number refer to the same subscriber as `value`? */
function sameNumber(a, b) {
  const x = digits(a), y = digits(b);
  if (!x || !y) return false;
  if (x === y) return true;
  // National against international notation: compare the last eight digits.
  const plusBoth = String(a).trim().startsWith('+') && String(b).trim().startsWith('+');
  return !plusBoth && x.length >= 8 && y.length >= 8 && x.slice(-8) === y.slice(-8);
}

/** Add or edit a saved contact. Resolves with the saved contact, or null. */
function contactDialog(existing, preset) {
  return new Promise((resolve) => {
    let saved = null;
    const c = existing || preset || {};
    const name = h('input', { type: 'text', required: true, maxlength: 100, value: c.name || '', placeholder: 'Name', autocomplete: 'off' });
    const number = h('input', { type: 'tel', required: true, value: c.number || '', placeholder: '+4500000000', autocomplete: 'off', inputmode: 'tel' });
    const note = h('input', { type: 'text', maxlength: 500, value: c.note || '', placeholder: 'Optional', autocomplete: 'off' });
    const form = h('form', { class: 'stack', novalidate: true, onsubmit: async (e) => {
      e.preventDefault();
      const body = { name: name.value.trim(), number: number.value.trim(), note: note.value.trim() };
      if (!body.name || !body.number) return toast('Enter a name and a number.', 'bad');
      await busy(form.querySelector('button[type=submit]'), async () => {
        const path = existing && existing.id ? `/contacts/${encodeURIComponent(existing.id)}` : '/contacts';
        saved = await post(path, body);
        toast(existing ? 'Contact updated' : 'Contact saved', 'ok');
        d.close();
      });
    } },
      field('Name', name),
      field('Number', number, 'International format is best, for example +4500000000.'),
      field('Note', note),
      h('div', { class: 'row end' }, button('Cancel', () => d.close(), 'ghost'), h('button', { type: 'submit', class: 'btn primary' }, existing ? 'Save changes' : 'Save contact')));
    const d = dialog(existing ? 'Edit contact' : 'New contact', form, { onClose: () => resolve(saved) });
    (c.name ? note : name).focus();
  });
}

// ---- Send SMS ----------------------------------------------------------------------------

export function send(root) {
  let alive = true, timer = null, estSeq = 0;
  let sims = [], providers = [], contacts = [];
  let via = '';          // '' = modem, otherwise a provider name
  let recipient = null;  // chosen contact
  let active = -1;       // highlighted suggestion
  let shown = [];        // suggestions on screen

  const provider = () => providers.find((p) => p.name === via);
  const simOf = (id) => sims.find((x) => x.modem === id);
  const simNumber = (x) => (x && x.configured && x.configured.mobile_number) || 'SIM number not configured';

  // -- route ---------------------------------------------------------------
  const routeBar = h('div', { class: 'segmented', role: 'radiogroup', 'aria-label': 'Send via' });
  const routeRow = h('div', { class: 'compose-row' }, h('span', { class: 'compose-label' }, 'Via'), routeBar);
  routeRow.hidden = true;

  const sender = h('input', { type: 'text', class: 'bare-input', maxlength: 15, autocomplete: 'off', spellcheck: 'false', placeholder: 'Sender name', 'aria-label': 'Sender name' });
  const senderCount = h('span', { class: 'muted small-text' });
  const fromBox = h('div', { class: 'compose-value' });
  const fromRow = h('div', { class: 'compose-row' }, h('span', { class: 'compose-label' }, 'From'), fromBox);
  const modem = h('select', { 'aria-label': 'Modem' });

  function drawRoute() {
    const opts = [['', 'signal', 'Modem', sims.length === 1 ? simNumber(sims[0]) : 'SIM number']]
      .concat(providers.map((p) => [p.name, 'cloud', p.name, 'sender name']));
    fill(routeBar, opts.map(([value, ico, label, hint]) => h('button', {
      type: 'button', class: 'seg-btn', role: 'radio', 'aria-checked': String(via === value),
      onclick: () => {
        via = value;
        const p = provider();
        if (p && !sender.value) sender.value = p.default_sender || '';
        drawRoute();
        estimate();
      },
    }, icon(ico), h('span', null, h('strong', null, label), h('small', null, hint)))));
    routeRow.hidden = !providers.length;
    const p = provider();
    if (p) {
      fill(fromBox, sender, senderCount);
      fill(senderCount, `${[...sender.value].length}/11`);
    } else if (sims.length > 1) {
      fill(fromBox, modem);
    } else {
      fill(fromBox, h('span', { class: 'mono' }, sims.length ? simNumber(sims[0]) : '…'), sims.length ? h('span', { class: 'muted small-text' }, ` ${sims[0].modem}`) : null);
    }
    flash.el.hidden = !!p;
    report.closest('.field').hidden = !!p;
    drawPreview();
  }
  sender.addEventListener('input', () => {
    fill(senderCount, `${[...sender.value].length}/11`);
    drawPreview();
  });
  modem.addEventListener('change', () => drawPreview());

  // -- recipient -------------------------------------------------------------
  const to = h('input', { type: 'text', class: 'bare-input', placeholder: 'Name or number', autocomplete: 'off', spellcheck: 'false', role: 'combobox', 'aria-expanded': 'false', 'aria-controls': 'to-list', 'aria-autocomplete': 'list', 'aria-label': 'Recipient' });
  const list = h('ul', { class: 'combo-list', id: 'to-list', role: 'listbox', hidden: true });
  const toBox = h('div', { class: 'compose-value combo' }, to, list);
  const toHint = h('div', { class: 'compose-hint' });

  const currentTo = () => (recipient ? recipient.number : to.value.trim());
  const known = (number) => contacts.find((c) => !c.number.endsWith('*') && sameNumber(c.number, number));

  function closeList() {
    list.hidden = true;
    to.setAttribute('aria-expanded', 'false');
    active = -1;
  }

  function drawList() {
    const q = to.value.trim().toLowerCase();
    const qd = digits(q);
    shown = contacts.filter((c) => !c.number.endsWith('*') && (!q || c.name.toLowerCase().includes(q) || (qd && digits(c.number).includes(qd)))).slice(0, 8);
    if (!shown.length || recipient) return closeList();
    if (active >= shown.length) active = shown.length - 1;
    fill(list, shown.map((c, i) => h('li', {
      role: 'option', id: 'to-opt-' + i, 'aria-selected': String(i === active), class: i === active ? 'active' : '',
      // mousedown keeps the focus in the input, so the blur handler does not close the list first.
      onmousedown: (e) => {
        e.preventDefault();
        pick(c);
      },
    }, avatar(c.name, 'small'), h('span', { class: 'grow' }, h('strong', null, c.name), c.note ? h('small', { class: 'muted' }, ' ' + c.note) : null), h('span', { class: 'mono muted' }, c.number))));
    list.hidden = false;
    to.setAttribute('aria-expanded', 'true');
    if (active >= 0) to.setAttribute('aria-activedescendant', 'to-opt-' + active);
    else to.removeAttribute('aria-activedescendant');
  }

  function drawRecipient() {
    if (recipient) {
      fill(toBox, h('span', { class: 'chip' }, avatar(recipient.name, 'small'), h('span', null, h('strong', null, recipient.name), ' ', h('span', { class: 'mono muted' }, recipient.number)),
        h('button', { type: 'button', class: 'chip-x', 'aria-label': 'Remove recipient', onclick: () => {
          recipient = null;
          to.value = '';
          drawRecipient();
          to.focus();
        } }, icon('x'))));
      fill(toHint);
    } else {
      if (!toBox.contains(to)) fill(toBox, to, list);
      const v = to.value.trim();
      const match = v && looksLikeNumber(v) ? known(v) : null;
      if (match) fill(toHint, icon('user'), ' ', match.name);
      else if (v && looksLikeNumber(v)) {
        fill(toHint, h('button', { type: 'button', class: 'btn link small-text', onclick: async () => {
          const saved = await contactDialog(null, { number: v });
          if (!saved || !alive) return;
          await loadContacts();
          pick(contacts.find((c) => c.id === saved.id) || saved);
        } }, 'Save as contact'));
      } else fill(toHint);
    }
    drawPreview();
    drawQuick();
  }

  function pick(c) {
    recipient = { name: c.name, number: c.number };
    closeList();
    drawRecipient();
    msg.focus();
  }

  to.addEventListener('input', () => {
    active = -1;
    drawList();
    drawRecipient();
  });
  to.addEventListener('focus', drawList);
  to.addEventListener('blur', closeList);
  to.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      if (list.hidden) drawList();
      if (!shown.length) return;
      e.preventDefault();
      active = (active + (e.key === 'ArrowDown' ? 1 : -1) + shown.length) % shown.length;
      drawList();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (!list.hidden && active >= 0) pick(shown[active]);
      else if (!list.hidden && shown.length === 1 && !looksLikeNumber(to.value)) pick(shown[0]);
      else msg.focus();
    } else if (e.key === 'Escape') closeList();
  });

  // -- message -----------------------------------------------------------------
  const msg = h('textarea', { class: 'compose-text', rows: 5, placeholder: 'Write a message…', 'aria-label': 'Message' });
  const meterFill = h('span', { class: 'meter-fill ok' });
  const meter = h('span', { class: 'meter slim', 'aria-hidden': 'true' }, meterFill);
  const stats = h('span', { class: 'compose-stats', 'aria-live': 'polite' }, '0 characters');
  let lastEstimate = null;

  const estimate = debounce(async () => {
    const my = ++estSeq;
    if (!msg.value) {
      lastEstimate = null;
      meterFill.style.width = '0%';
      drawPreview();
      return fill(stats, '0 characters');
    }
    try {
      const e = await post('/sms/estimate', { message: msg.value, flash: !provider() && flash.input.checked });
      if (my !== estSeq || !alive) return;
      lastEstimate = e;
      const used = e.units_per_part ? (e.units_per_part - e.units_remaining) / e.units_per_part : 0;
      meterFill.style.width = Math.round(Math.max(0.03, Math.min(1, used)) * 100) + '%';
      meterFill.className = 'meter-fill ' + (e.exceeds_limit ? 'bad' : e.parts > 1 ? 'warn' : 'ok');
      fill(stats, `${e.characters} characters`, h('span', { class: 'dot' }, '·'), String(e.encoding).toUpperCase(), h('span', { class: 'dot' }, '·'),
        h('strong', null, `${e.parts} segment${e.parts === 1 ? '' : 's'}`), h('span', { class: 'dot' }, '·'), `${e.units_remaining} left`,
        e.exceeds_limit ? h('span', { class: 'bad-text' }, ` — too long, the limit is ${app.cfg.max_message_parts} segments`) : null);
      drawPreview();
    } catch (e) {
      if (my === estSeq && alive) fill(stats, h('span', { class: 'bad-text' }, e.message));
    }
  }, 300);
  msg.addEventListener('input', () => {
    drawPreview();
    estimate();
  });
  msg.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      form.requestSubmit();
    }
  });

  // -- options ------------------------------------------------------------------
  const report = select([['', 'Gateway default'], ['true', 'Request a report'], ['false', 'No report']]);
  const prio = select([['normal', 'Normal'], ['high', 'High'], ['low', 'Low']]);
  const when = h('input', { type: 'datetime-local' });
  const flash = check('Flash SMS (shown at once, usually not stored)');
  flash.input.addEventListener('change', estimate);
  const options = h('details', { class: 'more' }, h('summary', null, 'Options'),
    h('div', { class: 'cols' }, field('Schedule', when, 'Local time. Empty sends now.'), field('Priority', prio), field('Delivery report', report)),
    flash.el);

  // -- preview --------------------------------------------------------------------
  const pvName = h('strong', null);
  const pvAvatar = h('span', null);
  const pvBubble = h('div', { class: 'bubble' });
  const pvMeta = h('div', { class: 'bubble-meta' });
  const pvTo = h('div', { class: 'phone-to' });
  const preview = h('aside', { class: 'phone', 'aria-label': 'Preview' },
    h('div', { class: 'phone-notch', 'aria-hidden': 'true' }),
    h('div', { class: 'phone-head' }, pvAvatar, h('div', null, pvName, h('div', { class: 'muted small-text' }, 'as the recipient sees it'))),
    h('div', { class: 'phone-screen' }, pvBubble, pvMeta),
    pvTo);

  function senderShown() {
    const p = provider();
    if (p) return sender.value.trim() || p.default_sender || 'Sender';
    const sim = modem.value ? simOf(modem.value) : sims.length === 1 ? sims[0] : null;
    return sim ? simNumber(sim) : 'Gateway SIM';
  }

  function drawPreview() {
    const name = senderShown();
    fill(pvName, name);
    fill(pvAvatar, avatar(name));
    pvBubble.classList.toggle('empty', !msg.value);
    fill(pvBubble, msg.value || 'Your message appears here');
    fill(pvMeta, lastEstimate && msg.value ? `${lastEstimate.parts} segment${lastEstimate.parts === 1 ? '' : 's'} · ${String(lastEstimate.encoding).toUpperCase()}` : '');
    const t = currentTo();
    fill(pvTo, t ? ['To ', h('strong', null, recipient ? recipient.name : (known(t) || {}).name || t)] : 'No recipient yet');
  }

  // -- quick contacts ----------------------------------------------------------------
  const quick = h('div', { class: 'quick' });
  function drawQuick() {
    const usable = contacts.filter((c) => !c.number.endsWith('*'));
    fill(quick,
      h('div', { class: 'quick-head' }, h('h3', null, 'Contacts'), h('a', { href: '/contacts', class: 'small-text' }, 'Manage')),
      usable.length
        ? h('div', { class: 'quick-list' }, usable.slice(0, 12).map((c) => h('button', {
          type: 'button', class: 'quick-item' + (recipient && recipient.number === c.number ? ' on' : ''), title: c.number, onclick: () => pick(c),
        }, avatar(c.name, 'small'), h('span', null, c.name))))
        : h('p', { class: 'muted small-text' }, 'No contacts yet. Save one from the Contacts page or with “Save as contact”.'));
  }

  // -- result ----------------------------------------------------------------------
  const result = h('div', null);
  function track(id, label) {
    const steps = h('ol', { class: 'steps' });
    const detail = h('div', null);
    fill(result, h('section', { class: 'card sent-card' },
      h('header', { class: 'card-head' }, h('h2', null, 'Sent to ', label), button('Details', () => openMessage(id), 'small ghost')),
      steps, detail));
    let n = 0;
    const poll = async () => {
      if (!alive) return;
      let stop = false;
      try {
        const st = await get(`/messages/${encodeURIComponent(id)}/status`);
        const failed = ['failed', 'delivery_failed', 'expired', 'cancelled', 'submission_unknown'].includes(st.status);
        const reached = { queued: 0, scheduled: 0, processing: 0, retrying: 0, submitted: 1, delivered: 2 }[st.status];
        const at = failed ? (st.submitted_at ? 2 : 1) : reached === undefined ? 0 : reached;
        fill(steps, ['Queued', 'Sent', 'Delivered'].map((name, i) => {
          const state = failed && i === at ? 'bad' : i <= at ? 'done' : 'todo';
          return h('li', { class: 'step ' + state }, h('span', { class: 'step-dot' }, state === 'done' ? icon('check') : state === 'bad' ? icon('x') : null), h('span', null, failed && i === at ? st.status.replace(/_/g, ' ') : st.status === 'scheduled' && i === 0 ? 'Scheduled' : name));
        }));
        stop = FINAL.includes(st.status) || st.status === 'scheduled' || ++n > 80;
        fill(detail, st.error ? notice('bad', st.error) : null,
          !FINAL.includes(st.status) && stop ? h('p', { class: 'muted small-text' }, st.status === 'scheduled' ? 'Scheduled; see the queue for changes.' : 'No longer watching; see Outbox for later changes.') : null);
      } catch (e) {
        fill(detail, errorBox(e));
        stop = e.status === 404 || ++n > 80;
      }
      if (!stop) timer = setTimeout(poll, n < 10 ? 1000 : 3000);
    };
    clearTimeout(timer);
    poll();
  }

  // -- submit ------------------------------------------------------------------------
  const sendBtn = h('button', { type: 'submit', class: 'btn primary send-btn' }, icon('send'), h('span', null, 'Send'));
  const form = h('form', { class: 'composer card', novalidate: true, onsubmit: async (e) => {
    e.preventDefault();
    const body = { to: currentTo(), message: msg.value, priority: prio.value };
    if (!body.to) {
      to.focus();
      return toast('Choose a contact or enter a number.', 'bad');
    }
    if (!body.message) {
      msg.focus();
      return toast('Write a message.', 'bad');
    }
    const p = provider();
    if (p) {
      body.provider = p.name;
      if (sender.value.trim()) body.sender_name = sender.value.trim();
      else if (!p.default_sender) return toast('Enter a sender name.', 'bad');
    } else {
      if (flash.input.checked) body.flash = true;
      if (report.value) body.delivery_report = report.value === 'true';
      if (modem.value) body.modem = modem.value;
    }
    if (when.value) {
      const d = new Date(when.value);
      if (isNaN(d)) return toast('The schedule time is not valid.', 'bad');
      body.scheduled_at = d.toISOString();
    }
    const label = recipient ? recipient.name : (known(body.to) || {}).name || body.to;
    const ok = await confirmBox(body.scheduled_at ? 'Schedule this SMS?' : 'Send this SMS?', [
      kv([
        ['To', recipient ? `${recipient.name} (${recipient.number})` : body.to],
        ['From', p ? `${senderShown()} (provider ${p.name})` : senderShown()],
        body.scheduled_at ? ['When', fmtTime(body.scheduled_at)] : null,
        lastEstimate ? ['Size', `${lastEstimate.parts} segment${lastEstimate.parts === 1 ? '' : 's'}`] : null,
      ]),
      h('pre', { class: 'body' }, body.message),
      h('p', { class: 'muted small-text' }, p ? 'A real SMS is sent through the provider and billed to the provider account.' : 'A real SMS is transmitted through the modem.'),
    ], body.scheduled_at ? 'Schedule' : 'Send');
    if (!ok) return;
    const key = window.crypto && crypto.randomUUID ? crypto.randomUUID() : null;
    await busy(sendBtn, async () => {
      const r = await api('/api/v1/sms/send', { method: 'POST', body, headers: key ? { 'Idempotency-Key': key } : {} });
      toast(body.scheduled_at ? 'Message scheduled' : 'Message queued', 'ok');
      msg.value = '';
      lastEstimate = null;
      meterFill.style.width = '0%';
      fill(stats, '0 characters');
      drawPreview();
      track(r.message_id, label);
    });
  } },
    routeRow,
    fromRow,
    h('div', { class: 'compose-row' }, h('span', { class: 'compose-label' }, 'To'), toBox, toHint),
    msg,
    h('div', { class: 'compose-foot' }, h('div', { class: 'compose-meter' }, meter, stats), h('span', { class: 'muted small-text kbd-hint' }, 'Ctrl + Enter'), sendBtn),
    options);

  root.append(h('div', { class: 'send-layout' }, h('div', { class: 'send-main' }, form, result), h('div', { class: 'send-side' }, preview, quick)));

  // -- data --------------------------------------------------------------------------
  async function loadContacts() {
    try {
      contacts = ((await get('/contacts')) || {}).contacts || [];
    } catch (_) {
      contacts = [];
    }
    if (alive) drawQuick();
  }

  const preset = new URLSearchParams(location.search).get('to');
  loadContacts().then(() => {
    if (!alive || !preset) return;
    const c = known(preset);
    if (c) pick(c);
    else {
      to.value = preset;
      drawRecipient();
      msg.focus();
    }
  });
  get('/sims').then((d) => {
    if (!alive) return;
    sims = d || [];
    if (sims.length > 1) {
      fill(modem, h('option', { value: '' }, 'Automatic'), sims.map((x) => h('option', { value: x.modem }, `${x.modem} — ${simNumber(x)}${x.ready ? '' : ' (SIM ' + x.state + ')'}`)));
    }
    drawRoute();
  }).catch((e) => alive && fill(fromBox, errorBox(e)));
  get('/providers').then((d) => {
    if (!alive) return;
    providers = ((d && d.providers) || []).filter((p) => p.enabled);
    drawRoute();
  }).catch(() => {});

  drawRoute();
  drawRecipient();
  if (!preset) to.focus();

  return { destroy: () => { alive = false; clearTimeout(timer); } };
}

// ---- Contacts ------------------------------------------------------------------------------

export function contacts(root) {
  let alive = true, all = [];
  const search = h('input', { type: 'search', class: 'bare-input', placeholder: 'Search contacts', 'aria-label': 'Search contacts', autocomplete: 'off' });
  const count = h('span', { class: 'muted small-text' });
  const grid = h('div', { class: 'contact-grid' });

  async function edit(c) {
    if (await contactDialog(c)) load();
  }
  async function remove(c) {
    if (!(await confirmBox('Delete contact?', `${c.name} (${c.number}) is removed from the saved contacts. Messages are not affected.`, 'Delete', true))) return;
    try {
      await api(`/api/v1/contacts/${encodeURIComponent(c.id)}`, { method: 'DELETE' });
      toast('Contact deleted', 'ok');
    } catch (e) {
      toast(e.message, 'bad');
    }
    load();
  }

  function draw() {
    const q = search.value.trim().toLowerCase();
    const qd = digits(q);
    const rows = all.filter((c) => !q || c.name.toLowerCase().includes(q) || (c.note || '').toLowerCase().includes(q) || (qd && digits(c.number).includes(qd)));
    fill(count, all.length ? `${rows.length} of ${all.length}` : '');
    if (!all.length) {
      return fill(grid, h('div', { class: 'empty-state' }, avatar('+'), h('p', null, 'No contacts yet.'), h('p', { class: 'muted' }, 'Save numbers with a name to pick them quickly when sending, and to see names instead of numbers in the inbox.'), button('New contact', add, 'primary')));
    }
    if (!rows.length) return fill(grid, h('p', { class: 'muted empty' }, 'No contact matches the search.'));
    fill(grid, rows.map((c) => {
      const pattern = c.number.endsWith('*');
      return h('article', { class: 'contact-card' },
        avatar(c.name, 'large'),
        h('div', { class: 'contact-main' },
          h('strong', { class: 'contact-name' }, c.name),
          h('span', { class: 'mono muted' }, c.number),
          c.note ? h('span', { class: 'small-text contact-note' }, c.note) : null,
          h('span', { class: 'contact-tags' },
            c.editable ? null : badge('addressbook.conf', 'muted', 'Defined in the address book file; change it there.'),
            c.anonymous ? badge('anonymous', 'info', 'On the anonymous list: never disclosed to integrations.') : null)),
        h('div', { class: 'contact-actions' },
          pattern ? null : h('a', { class: 'btn small primary', href: '/send?to=' + encodeURIComponent(c.number) }, icon('send'), 'Send SMS'),
          c.editable ? h('button', { type: 'button', class: 'btn small ghost icon-btn', 'aria-label': `Edit ${c.name}`, title: 'Edit', onclick: () => edit(c) }, icon('edit')) : null,
          c.editable ? h('button', { type: 'button', class: 'btn small ghost icon-btn', 'aria-label': `Delete ${c.name}`, title: 'Delete', onclick: () => remove(c) }, icon('trash')) : null));
    }));
  }

  async function load() {
    try {
      all = ((await get('/contacts')) || {}).contacts || [];
      if (alive) draw();
    } catch (e) {
      if (alive) fill(grid, errorBox(e));
    }
  }
  async function add() {
    if (await contactDialog(null)) load();
  }

  search.addEventListener('input', draw);
  root.append(
    h('div', { class: 'toolbar' }, h('label', { class: 'search-box' }, icon('search'), search), count, h('span', { class: 'grow' }),
      h('button', { type: 'button', class: 'btn primary', onclick: add }, icon('plus'), 'New contact')),
    grid);
  load();
  return { refresh: () => load(), destroy: () => { alive = false; } };
}

// API Tokens: create, list and revoke managed bearer tokens (admin only).

import {
  h, fill, get, post, api, fmtTime, dash, badge, button, busy, table, dialog, confirmBox, toast, field, select, notice, errorBox,
} from './core.js';

/** Show a freshly created token once, with a copy control. */
function showToken(t) {
  const box = h('input', { type: 'text', readonly: true, value: t.token, class: 'mono', 'aria-label': 'New token' });
  const copy = button('Copy', async () => {
    box.focus();
    box.select();
    try {
      await navigator.clipboard.writeText(t.token);
      toast('Copied to clipboard', 'ok');
    } catch (_) {
      document.execCommand && document.execCommand('copy');
    }
  }, 'small');
  dialog('Token created', h('div', { class: 'stack' },
    notice('warn', h('strong', null, 'Copy this token now. '), 'It is shown only once and cannot be retrieved later.'),
    field(`${t.name} · ${t.scope}`, h('div', { class: 'row' }, box, copy)),
    h('p', { class: 'muted small-text' }, 'Use it as ', h('code', null, 'Authorization: Bearer <token>'), '. The client’s source IP must also be allow-listed (api.allowed_cidrs).')),
    { actions: (close) => [button('Done', close, 'primary')] });
}

function createDialog(onCreated) {
  const name = h('input', { type: 'text', required: true, maxlength: 100, placeholder: 'e.g. honeypot-collector', autocomplete: 'off' });
  const scope = select([['read', 'Read-only (recommended)'], ['admin', 'Admin (full control)']]);
  const expires = h('input', { type: 'datetime-local' });
  const form = h('form', { class: 'stack', novalidate: true, onsubmit: async (e) => {
    e.preventDefault();
    const body = { name: name.value.trim(), scope: scope.value };
    if (!body.name) return toast('Enter a name for the token.', 'bad');
    if (expires.value) {
      const d = new Date(expires.value);
      if (isNaN(d)) return toast('The expiry time is not valid.', 'bad');
      body.expires_at = d.toISOString();
    }
    await busy(form.querySelector('button[type=submit]'), async () => {
      const t = await post('/tokens', body);
      d.close();
      showToken(t);
      onCreated();
    });
  } },
    field('Name', name, 'What integration uses this token.'),
    field('Scope', scope, 'Read-only tokens cannot change anything or run admin actions.'),
    field('Expires (optional)', expires, 'Local time; leave empty for no expiry.'),
    h('div', { class: 'row end' }, button('Cancel', () => d.close(), 'ghost'), h('button', { type: 'submit', class: 'btn primary' }, 'Create token')));
  const d = dialog('New API token', form);
  name.focus();
}

export function tokens(root) {
  let alive = true;
  const body = h('div', null, h('p', { class: 'muted' }, 'Loading…'));

  async function load() {
    try {
      const d = (await api('/api/v1/tokens', { noAuthPrompt: true })).data;
      if (!alive) return;
      const rows = d.tokens || [];
      fill(body, table([
        { label: 'Name', get: (t) => h('strong', null, t.name) },
        { label: 'Prefix', cls: 'mono', get: (t) => t.prefix + '…' },
        { label: 'Scope', get: (t) => badge(t.scope === 'admin' ? 'admin' : 'read', t.scope === 'admin' ? 'warn' : 'info') },
        { label: 'Status', get: (t) => (t.enabled ? badge('active', 'ok') : badge('disabled', 'muted')) },
        { label: 'Created', get: (t) => fmtTime(t.created_at) },
        { label: 'Last used', get: (t) => (t.last_used_at ? fmtTime(t.last_used_at) : h('span', { class: 'muted' }, 'never')) },
        { label: 'Expires', get: (t) => (t.expires_at ? fmtTime(t.expires_at) : dash(null)) },
        { label: '', get: (t) => h('div', { class: 'row' },
          button(t.enabled ? 'Disable' : 'Enable', async (e) => {
            await busy(e.target, async () => { await post(`/tokens/${encodeURIComponent(t.id)}`, { enabled: !t.enabled }); });
            load();
          }, 'small ghost'),
          button('Revoke', async () => {
            if (!(await confirmBox('Revoke token?', `${t.name} stops working immediately. This cannot be undone.`, 'Revoke', true))) return;
            try { await api(`/api/v1/tokens/${encodeURIComponent(t.id)}`, { method: 'DELETE' }); toast('Token revoked', 'ok'); } catch (err) { toast(err.message, 'bad'); }
            load();
          }, 'small danger')) },
      ], rows, { empty: 'No managed tokens yet. Create one for each integration so you can revoke them individually.' }));
    } catch (e) {
      if (!alive) return;
      if (e.status === 401 || e.status === 403) {
        fill(body, notice('warn', h('strong', null, 'Admin access required. '), 'Managing tokens needs an administrative token. Use “Set token” (top right) with your admin token.'));
      } else {
        fill(body, errorBox(e));
      }
    }
  }

  root.append(
    h('div', { class: 'toolbar' },
      h('p', { class: 'muted' }, 'Bearer tokens for API clients and integrations. Only a hash is stored; revoking one takes effect at once — no reload.'),
      h('span', { class: 'grow' }),
      button('New token', () => createDialog(load), 'primary')),
    h('section', { class: 'card' }, body));
  load();
  return { refresh: () => load(), destroy: () => { alive = false; } };
}

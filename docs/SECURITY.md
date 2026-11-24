# Security

This document describes the gateway's threat model and every security control it has. To
report a vulnerability, see [../SECURITY.md](../SECURITY.md).

- [Threat model](#threat-model)
- [Hardening checklist](#hardening-checklist)
- [Access controls](#access-controls)
- [CSRF, Host and Origin handling](#csrf-host-and-origin-handling)
- [Reverse proxy guidance](#reverse-proxy-guidance)
- [Secrets handling and redaction](#secrets-handling-and-redaction)
- [Logging privacy defaults](#logging-privacy-defaults)
- [Data at rest](#data-at-rest)
- [SMS spending protection](#sms-spending-protection)
- [Raw AT and raw PDU gates](#raw-at-and-raw-pdu-gates)
- [SIM protection](#sim-protection)
- [systemd sandboxing](#systemd-sandboxing)
- [Untrusted SMS content](#untrusted-sms-content)
- [Outbound requests](#outbound-requests)
- [Call recording: legal note](#call-recording-legal-note)
- [Secret scanning and supply chain](#secret-scanning-and-supply-chain)

## Threat model

The gateway is designed for a **trusted internal network**: a host that a small group of
administrators and internal systems can reach, not a service on the public internet.

What it protects:

| Asset | Why it matters |
| --- | --- |
| The ability to send SMS | Every message costs money and carries your number's reputation |
| Stored messages, call logs, recordings | Personal data and often one-time codes |
| The SIM | A blocked SIM takes the gateway out of service |
| Secrets in the configuration | Webhook URLs, tokens, database credentials, the SIM PIN |

Who it defends against, and how far:

| Adversary | Position |
| --- | --- |
| Anyone who can send an SMS to the gateway | Fully untrusted. Message content is never executed, interpreted as a template, or used unescaped; see [Untrusted SMS content](#untrusted-sms-content). |
| Hosts outside the allowed networks | Rejected by address before anything else is evaluated |
| A web page open in an administrator's browser (cross-site request forgery, DNS rebinding) | State-changing cross-site requests are rejected; the `Host` check stops rebinding when `allowed_hosts` is set |
| Hosts inside the allowed networks | **Trusted by default.** With `authentication = "none"` they can send messages and read everything. Enable bearer tokens to change that. |
| Receivers of integration traffic | Receive only what the integration's settings allow; anonymisation and body redaction limit it further |
| Local users on the gateway host | File permissions and the service sandbox limit access; root is trusted |

Not in scope:

- **Transport encryption.** The gateway serves plain HTTP only. Tokens and message content
  travel unencrypted unless a reverse proxy adds TLS.
- **The mobile network.** SMS is not confidential or authenticated; sender addresses can be
  forged.
- **User accounts.** There are tokens, not users: no per-user permissions beyond the
  ordinary/administrative split.
- **Encryption at rest.** See [Data at rest](#data-at-rest).

> **An unauthenticated gateway must never be reachable from the internet.** With the default
> settings anyone who can reach the port can send SMS at your expense and read every stored
> message.

## Hardening checklist

In rough order of importance:

1. Keep `api.allowed_cidrs` as narrow as you can. Never add `0.0.0.0/0` with authentication
   disabled (the gateway warns when you do).
2. Set `api.authentication = "bearer"` with long random tokens.
3. Set separate `api.admin_tokens`, and `api.admin_allowed_cidrs` if administration happens
   from few hosts.
4. Bind to `127.0.0.1` and put a TLS-terminating reverse proxy in front for anything that
   leaves the host; set `api.trusted_proxies` and `api.allowed_hosts`.
5. Set SIM quotas (`[limits]` in `sim.conf`) and, where the use case allows, a
   `security.destination_allowlist`.
6. Leave `security.allow_raw_at_commands` and `security.allow_raw_pdu_submission` off.
7. Keep secrets in `smsgateway.env` or `${file:...}` files, not in the `.conf` files.
8. Decide on retention (`[retention]`), and whether integrations need message text at all
   (`include_message_body`, `anonymize`).
9. Keep `logging.log_message_content = false` and `logging.redact_sensitive_values = true`.

A token configuration:

```toml
[api]
bind = "127.0.0.1"
authentication = "bearer"
tokens = ["${SMSGW_API_TOKEN}"]
admin_tokens = ["${SMSGW_ADMIN_TOKEN}"]
admin_allowed_cidrs = ["192.0.2.10/32"]
trusted_proxies = ["127.0.0.1"]
allowed_hosts = ["smsgateway.example.invalid"]
```

Tokens can be created with, for example, `openssl rand -hex 32`.

## Access controls

Every request passes these checks in order; the first failure ends it.

| # | Control | Setting | Default | Enable or tighten |
| --- | --- | --- | --- | --- |
| 1 | Network allowlist | `api.allowed_cidrs` | Loopback and the private IPv4 ranges (`127.0.0.0/8`, `::1/128`, `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`) | List only the networks or hosts that need access |
| 2 | Host header check | `api.allowed_hosts` | Off (empty) | List the names and addresses clients use |
| 3 | Cross-site request rejection | built in; `api.cors_enabled`, `api.cors_allowed_origins` | On; no cross-origin access | Leave CORS off unless a browser application on another origin needs the API |
| 4 | Bearer token | `api.authentication`, `api.tokens` | `none` | Set `bearer` and at least one token of 16 or more characters |
| 5 | Request rate limit | `api.rate_limit_per_minute` | 120 per client address | Lower it, or 0 to disable |

Further limits: request bodies up to `api.max_request_size_kb` (256), requests up to
`api.request_timeout_seconds` (30).

Details:

- **Client address behind a proxy.** `X-Forwarded-For` is honoured only when the connecting
  peer is listed in `api.trusted_proxies`. The right-most address that is not itself a
  trusted proxy is used. A forwarded header from any other peer is ignored, so a client
  cannot talk its way past `allowed_cidrs`. IPv4-mapped IPv6 addresses are compared as IPv4.
- **Tokens** are sent as `Authorization: Bearer <token>` and compared in constant time.
  Tokens shorter than 16 characters are a configuration error. There is no token expiry or
  rotation mechanism; to rotate, change the configuration and reload (tokens apply on
  reload).
- **Paths that need no token** even with bearer authentication: the dashboard shell (`/`,
  `/favicon.svg`, `/assets/*`), the probes `/health` and `/ready`, `/api/v1/ui/config`, and
  `OPTIONS` requests. `/api/v1/ui/config` returns the dashboard title, theme, refresh
  interval, display settings, the authentication mode, instance name, version, the maximum
  part count, the **names** of configured USSD commands and the modem ids; it holds no
  secrets and exists so that the dashboard can render its login prompt. These paths still
  pass the network and Host checks.
- **Metrics** (`/metrics`) require a token like any API path. Give your Prometheus server a
  token, or keep the gateway unauthenticated only on a network where that is acceptable.
- **The rate limit** is per client address and applies to API calls; probes, dashboard
  assets and `/api/v1/ui/config` are exempt. It is a guard against runaway clients, not
  against a distributed attacker.

### Administrative endpoints

A second layer for the endpoints that change or reveal more:

| Setting | Default | Effect |
| --- | --- | --- |
| `api.admin_enabled` | `true` | `false` disables all administrative endpoints (`403`) |
| `api.admin_allowed_cidrs` | empty | When set, administrative endpoints additionally require the client address to be in this list |
| `api.admin_tokens` | empty | When set, administrative endpoints require one of these tokens. An admin token is also accepted for ordinary endpoints. |

Administrative endpoints: modem reconnect, raw AT, SIM PIN counter reset, raw PDU
submission, sending with `override_allowlist`, message deletion, queue retry, integration
test, delivery replay, configuration view and reload, audit trail, diagnostics, database
backup.

**With the defaults, administrative endpoints are no more protected than the others.** Set
`admin_tokens` (and ideally `admin_allowed_cidrs`) to separate "may send and read" from "may
administer".

Not administrative, and therefore open to every caller that passes the general checks:
sending, reading messages, events and the event stream, call logs, **recording and
attachment downloads**, history export, USSD with configured commands, and cancelling
queued messages. If those must be restricted, restrict access to the API as a whole.

The command-line tool uses the first administrative token, or else the first ordinary token,
from the configuration it loads.

### Audit trail

State-changing and sensitive calls are recorded in the `audit_events` table and in
`audit.log` with the client address, the action, the target and the request id: submissions,
raw PDU and raw AT commands (including the command text), deletions, retries, cancellations,
reconnects, PIN counter resets, USSD requests, integration tests and replays, recording and
attachment downloads, exports, diagnostics, backups, and configuration reloads (successful
and rejected). Automation firings are logged to `audit.log`. Read it with
`GET /api/v1/audit` (administrative). Entries are kept for `retention.audit_days`.

## CSRF, Host and Origin handling

An unauthenticated API on a private network is exposed to a classic attack: a web page on
the internet, open in the browser of someone inside the network, makes the browser send
requests to the gateway. The gateway defends against this without depending on tokens:

- **State-changing methods** (everything except `GET`, `HEAD`, `OPTIONS`) with an `Origin`
  header are accepted only when the origin's host and port equal the request's `Host`
  header, or the origin is listed in `api.cors_allowed_origins` with CORS enabled.
  Everything else is `403 cross-origin request rejected`.
- Without an `Origin` header, a request that the browser marks `Sec-Fetch-Site: cross-site`
  is rejected.
- Requests without either header (command-line clients, servers) are not affected.
- **JSON only.** Bodies must be `application/json` (`415` otherwise), which a cross-site HTML
  form cannot send.
- **No CORS by default.** Without CORS headers a foreign page cannot read responses.
  `Access-Control-Allow-Origin` is only ever set to an explicitly listed origin, never `*`.
- **Host check against DNS rebinding.** With `api.allowed_hosts` set, a request whose `Host`
  header is not listed gets `421`. Rebinding attacks rely on the browser sending the
  attacker's host name; listing your real names and addresses stops them. This check is
  **off by default**: set it.
- Read-only `GET` requests are not origin-checked. A cross-site page can trigger them but
  cannot read the result (no CORS). No `GET` endpoint changes state.

Response headers on everything: `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
`Referrer-Policy: no-referrer`, and
`Content-Security-Policy: default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'`.
API responses are `Cache-Control: no-store`.

The dashboard is plain ES modules served from the binary, loads nothing from other hosts,
uses no inline scripts, and renders all API data through text nodes, so message content
cannot become markup. A bearer token entered in the dashboard is kept in `sessionStorage`
(per browser tab) and sent only to the gateway. `web.show_message_content` and
`web.show_modem_information` are display settings of the dashboard; they are not access
controls, and the REST API returns those fields regardless.

## Reverse proxy guidance

The gateway has no TLS support. For access from other hosts, terminate TLS on a reverse
proxy on the same machine.

```toml
[api]
bind = "127.0.0.1"
port = 8080
trusted_proxies = ["127.0.0.1"]
allowed_hosts = ["smsgateway.example.invalid"]
```

- **`bind = "127.0.0.1"`** so that the plain-HTTP port is not reachable from the network.
- **`trusted_proxies`** must list the proxy, or every request appears to come from the proxy
  itself and `allowed_cidrs`, the admin CIDR list and the rate limit stop distinguishing
  clients. List only proxies you control: a trusted proxy's `X-Forwarded-For` is believed.
- The proxy must **set** `X-Forwarded-For` to the client address rather than pass a
  client-supplied header through. The example uses `$remote_addr`.
- The proxy must pass the original **`Host`** header (`proxy_set_header Host $http_host`).
  The gateway compares `Origin` with `Host`; a rewritten `Host` makes the dashboard's own
  requests look cross-origin.
- For `/api/v1/events/stream` disable response buffering and use a long read timeout.
- Keep an address restriction or authentication on the proxy as well.

[scripts/nginx-smsgateway.conf.example](../scripts/nginx-smsgateway.conf.example) shows all
of this for nginx. It listens on port 80; add your `listen 443 ssl` and certificate
directives. `X-Forwarded-Proto` is set by the example but not used by the gateway.

## Secrets handling and redaction

**Where secrets go.** Use references instead of literal values:

| Reference | Source |
| --- | --- |
| `"${NAME}"` | The process environment, then the file `smsgateway.env` next to the main configuration file |
| `"${file:/path}"` | A file |

- `/etc/thugsred/smsgateway.env` is installed with mode `0600`, owner root. systemd reads it
  as root and passes the variables to the service. The command-line tool reads the same file
  directly, which works only for root. An unprivileged user, and the service account itself,
  cannot read it; they get an error that says the variable was not found and that the file
  may need root to read.
- The `.conf` files are `0640`, owner root, group `thugsred-smsgateway`: the service reads
  them but cannot modify them, and other users cannot read them. The gateway warns when
  `sim.conf` or `anonymous.conf` is accessible by "other".
- Files used with `${file:...}` must be readable by the service account.
- Secrets passed through the environment are visible to root and to the service's own
  processes (for example through `/proc/<pid>/environ`).

**Where secrets never appear.**

| Output | Protection |
| --- | --- |
| `check-config --show`, `GET /api/v1/config`, diagnostics | `pin`, `password`, `mysql_url`, `tokens`, `admin_tokens`, `signing_secret`, `bot_token`, `token`, `webhook_url`, `chat_id` and all custom header values are shown as `***`; `url` values are reduced to scheme and host; `iccid` and `imsi` are masked except the last four characters |
| `GET /api/v1/sim` | The PIN and APN password are never included |
| `GET /api/v1/integrations` | No URLs or tokens |
| Diagnostics | IMEI, ICCID, IMSI and MSISDN reduced to the last four characters; recent events are listed by type and time only |
| Logs | The SIM PIN command is logged as redacted. HTTP client errors are stripped of their URL, so tokens embedded in webhook URLs (Telegram, Discord, Slack) do not reach the logs. Third-party library output is limited to warnings and errors, which keeps SQL statements out of verbose logs. |
| Build | Release builds remap source paths; the binary embeds only the commit id, build date and target |

## Logging privacy defaults

| Setting | Default | Effect |
| --- | --- | --- |
| `logging.log_message_content` | `false` | Message text is **not** written to `messages.log` |
| `logging.redact_sensitive_values` | `true` | Telephone numbers in the logs are masked (country prefix and last two digits remain, for example `+45******00`) |
| Anonymous list | — | Numbers in `anonymous.conf` are replaced by the label in the logs even when masking is off |
| Log file mode | `0640` | In a directory with mode `0750` owned by the service account |
| `logging.retention_days` | 30 | Rotated log files are deleted after this time |

What the logs do contain: message ids, direction, type, status, modem, part count, encoding,
errors, call ids, masked numbers, integration names, HTTP status codes, and audit entries
with client addresses. The audit entry of a raw AT command or a USSD request contains the
command or code.

Metrics never use telephone numbers or message content as labels.

## Data at rest

- Messages, events, call logs and the audit trail are stored **unencrypted** in the
  database. Recordings and attachments are plain files. Protect them with file permissions
  (the installer uses `0750` directories owned by the service account), disk encryption if
  the host warrants it, and retention settings.
- The database keeps real numbers and full text regardless of anonymisation settings, which
  apply to integrations and logs only.
- **Deleting a message does not delete the events emitted for it.** The `events` table keeps
  the message data until `retention.integration_event_days` has passed, and
  `GET /api/v1/events` returns it. The same applies to `sms.store_sent_messages = false`.
  See [DATABASE.md](DATABASE.md#retention).
- Backups contain everything the database contains. Store them accordingly.
- Recording file names carry no caller information (`call-<random id>.wav`).
- Downloads are served only from inside `paths.recordings` and `paths.attachments`, after
  resolving symbolic links, and retention deletes files only inside those directories.

## SMS spending protection

Layers, from the request to the modem:

| Layer | Setting | Default | Behaviour |
| --- | --- | --- | --- |
| Who may call the API | Access controls above | Private networks, no token | |
| Destination blocklist | `security.destination_blocklist` | empty | Matching destinations are refused (`403`) |
| Destination allowlist | `security.destination_allowlist` | empty (no restriction) | When set, only matching destinations are accepted. Exact numbers or prefixes such as `"+45*"`. |
| Allowlist override | `security.allow_destination_overrides` | `false` | Only when true, and only for administrative callers sending `override_allowlist: true` |
| Message size | `sms.max_message_parts`, `sms.allow_multipart` | 10 parts | Longer messages are refused |
| Feature switches | `sms.allow_binary`, `sms.allow_flash_sms`, `sms.allow_unicode` | all on | |
| Queue size | `security.max_outbound_queue_size` | 10000 | Further submissions get `429` |
| Rate per modem | `sms.outbound_rate_limit_per_minute` | 10 segments per minute | Excess waits in the queue |
| Rate per destination | `sms.per_destination_interval_seconds` | 0 (off) | Minimum gap between messages to one number |
| SIM quotas | `daily_sms_limit`, `monthly_sms_limit` in `sim.conf` | 0 (unlimited) | Counted in segments, per day and month in `general.timezone` |
| Roaming | `roaming_enabled` in `sim.conf` | `false` | Nothing is sent while roaming |
| Expiry | `sms.queue_expiry_seconds` | 86400 | Messages that could not be sent in time are dropped instead of going out late |
| Retries | `sms.max_send_attempts` | 3 | Bounded; an uncertain submission is never retried automatically |
| Idempotency | `Idempotency-Key` header | — | A repeated request does not send again |
| Automation | `cooldown_seconds`, `max_per_hour`, loop rules | off | See [INTEGRATIONS.md](INTEGRATIONS.md#loop-prevention) |

How quotas are enforced, and their limits:

- A request pinned to a modem (`modem` or `from`) is refused with `429` when the SIM's quota
  would be exceeded.
- For unpinned messages, and again at sending time for all messages, the sender worker
  checks the quota. A message that would exceed it is **deferred, not failed**: it stays
  queued and is offered again later (to another modem if there is one), until it is sent in
  a new quota period or expires.
- The count is the number of segments the modem submitted since the start of the day or
  month, taken from the database.
- Quotas need a **configured** SIM binding: `sim =` on the modem entry, or the single-modem,
  single-SIM case. A SIM that is bound only by ICCID is not quota-checked.
- Raw PDU submission bypasses the queue and therefore the rate limit and the quotas (it
  still counts towards later quota checks). One more reason to leave it disabled.
- With a `retention.message_days` shorter than a month the monthly count undercounts.
- The gateway cannot know your tariff. Quotas limit volume, not cost; premium-rate and
  international destinations are best excluded with the allowlist or blocklist.

**The allowlist is the strongest single control.** If the gateway only ever needs to message
a known set of numbers or one country, say so.

## Raw AT and raw PDU gates

Both are off by default, administrative, and audited.

| Setting | Enables | Risk |
| --- | --- | --- |
| `security.allow_raw_at_commands` | `POST /api/v1/modem/at` | Direct control of the modem: dialling, reconfiguration, reading the phonebook |
| `security.allow_raw_pdu_submission` | `POST /api/v1/sms/send-pdu`, and a non-zero `protocol_identifier` in normal send requests | Messages the gateway did not build; bypasses the queue, rate limit and quotas |

Even when raw AT is enabled, a command must start with `AT`, be at most 200 characters and
contain no control characters, and commands starting with these prefixes are always refused:

| Blocked prefix | Reason |
| --- | --- |
| `AT+CPIN=` | PIN and PUK entry; could block the SIM |
| `AT+CLCK` | Facility locks, including the SIM PIN lock |
| `AT+CPWD` | Password changes |
| `AT+CMGS`, `AT+CMGW`, `AT+CMSS` | Sending or storing messages outside the gateway's records |
| `AT+CFUN=0` | Switching the radio off |
| `AT+CPOWD` | Powering the module down |
| `AT&F`, `ATZ` | Resetting the modem's configuration |
| `AT+IPR=` | Changing the baud rate |

The blocklist is a prefix match on the upper-cased command. It is a safety net against
accidents, not a sandbox: other vendor-specific commands can still reconfigure or disrupt
the modem, and `ATD` (dialling) is allowed and logged as an outgoing call. Enable raw AT only
for diagnosis, behind an administrative token, and switch it off again.

The raw PDU endpoint accepts only a well-formed SMS-SUBMIT and checks its destination
against the allow and block lists.

The offline PDU tools (`/api/v1/pdu/encode`, `/api/v1/pdu/decode`) transmit nothing and are
not gated.

## SIM protection

- A configured PIN is entered **once**. A rejected PIN is never retried, and the attempt is
  recorded in the database before it is made so that a crash cannot hide it. See
  [MODEMS.md](MODEMS.md#sim-pin-and-puk-policy).
- A PUK is never entered.
- PIN-related AT commands are blocked on the raw AT endpoint.
- The PIN is never logged and never returned by the API.

## systemd sandboxing

The shipped unit runs the daemon as the unprivileged user `thugsred-smsgateway` with these
restrictions:

| Directive | Effect |
| --- | --- |
| `NoNewPrivileges=true` | No privilege escalation through setuid programs |
| `CapabilityBoundingSet=`, `AmbientCapabilities=` (empty) | No capabilities at all |
| `ProtectSystem=strict` | The whole filesystem is read-only except the paths below |
| `StateDirectory`, `LogsDirectory`, `RuntimeDirectory` | `/var/lib`, `/var/log` and `/run` subdirectories named `thugsred-smsgateway`, mode `0750` |
| `ReadWritePaths=` | `recordings` and `backups` under the installation prefix |
| `ProtectHome=true` | Home directories are inaccessible |
| `PrivateTmp=true` | Private `/tmp` |
| `ProtectKernelTunables`, `ProtectKernelModules`, `ProtectKernelLogs`, `ProtectControlGroups`, `ProtectClock`, `ProtectHostname` | Kernel and host settings cannot be changed |
| `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK` | Only these socket types |
| `RestrictNamespaces`, `RestrictRealtime`, `RestrictSUIDSGID`, `LockPersonality` | |
| `MemoryDenyWriteExecute=true` | No writable and executable memory |
| `SystemCallArchitectures=native` | |
| `UMask=0027` | New files are not accessible by "other" |
| `SupplementaryGroups=dialout audio` | Serial devices; ALSA for recording |

Deliberately **not** set: `PrivateDevices`, because the modem is a device. The service can
therefore open any device its groups permit. If the gateway does not record calls you can
remove `audio` from the groups with a drop-in.

The configuration directory is read-only for the service both through file ownership and
through `ProtectSystem=strict`; the daemon never writes its configuration.

If you relocate data directories, add `ReadWritePaths=` for them
([INSTALLATION.md](INSTALLATION.md#the-systemd-unit)).

## Untrusted SMS content

Anyone can send the gateway an SMS, with any content and a forged sender. The content is
treated as data at every step:

| Where | Measure |
| --- | --- |
| PDU decoding | Every length is bounds-checked; malformed PDUs are stored as raw records instead of crashing or being dropped; user data is limited to 140 octets and a PDU to 1024 hexadecimal characters |
| MMS notifications | Parsed defensively; control characters are removed and strings limited to 512 characters. The announced URL is fetched only when retrieval is enabled, only from the configured MMSC host, without credentials and without following redirects. Downloaded content is stored as an opaque file and never interpreted. |
| Database | All queries use bound parameters; search input is escaped for `LIKE` |
| Templates | Placeholders are substituted once. Message text is never re-read as a template, and there are no expressions to inject into. |
| Chat integrations | Values are escaped for the destination's markup, and `@` mentions are neutralised so that a message cannot ping `@everyone`, `@here` or users. Discord posts carry an empty `allowed_mentions`. Telegram messages are sent as plain text. |
| Webhooks | JSON is produced by a serialiser; custom body templates are escaped according to their content type |
| Syslog | Line breaks are replaced, so a message cannot forge additional log lines |
| CSV export | Fields are quoted, and fields that start with `=`, `+`, `-`, `@`, tab or carriage return are prefixed with `'` so that spreadsheets do not evaluate them as formulas |
| Dashboard | Content is rendered through text nodes; the Content Security Policy forbids inline and foreign scripts |
| Logs | Content is not logged by default; structured (JSON) logging keeps fields separate |
| File names | Names derived from message data are reduced to letters, digits, `-`, `_`, `.` |
| Automation | Never reacts to alphanumeric senders or short codes; replies need an explicit sender allowlist or pattern; bounded by cooldown and hourly cap |
| USSD | Text sent by the network is recorded and reported, never acted on |

What remains your responsibility: systems that **receive** integration traffic must treat it
as untrusted too, and a human reading a forwarded SMS can still be deceived by its text.
Sender addresses are not authenticated by the mobile network; do not use an allowlisted
sender as proof of identity for anything that matters.

## Outbound requests

The gateway makes outbound connections only to destinations you configure:

- integration URLs and the syslog server;
- the MMSC host, when experimental MMS retrieval is enabled;
- the database server, for MySQL/MariaDB.

Properties: TLS certificates are verified with the bundled web PKI roots (the system trust
store is not used, so internal certificate authorities need `verify_tls = false`, which the
gateway warns about, or a publicly trusted certificate); redirects are never followed;
every integration has a timeout. Integration URLs come from the configuration only and
cannot be influenced by message content, except for values you deliberately place into a
webhook's query parameters, which are URL-encoded.

## Call recording: legal note

Automatically answering and recording telephone calls is regulated in many jurisdictions.
Depending on where you and the caller are, recording may require the consent of one or all
parties, a prior announcement, or may be prohibited; data-protection law usually applies to
the recordings and call logs as personal data. The gateway does not play an announcement and
cannot obtain consent.

- Both `calls.auto_answer` and `calls.recording_enabled` are off by default.
- Find out which rules apply to you before enabling either. This documentation is not legal
  advice.
- Limit retention (`retention.recording_days`), and remember that recording downloads are
  not an administrative endpoint: everyone with API access can fetch them.

See [CALLS.md](CALLS.md).

## Secret scanning and supply chain

| Command | Purpose |
| --- | --- |
| `make secret-scan` | Scans the working tree for credentials. Uses `gitleaks` when installed, otherwise the built-in pattern scan `scripts/secret-scan.sh`. |
| `make hooks` | Installs `scripts/pre-commit` as the Git pre-commit hook |
| `make audit` | `cargo audit`: known vulnerabilities in dependencies |
| `make deny` | `cargo deny check`: licences, advisories, yanked crates, crate sources (`deny.toml`) |
| `make sbom` | CycloneDX software bill of materials from `Cargo.lock` |

The built-in scan looks for common token formats (GitHub, Slack, Discord and Slack webhook
URLs, Telegram bot tokens, private key headers, AWS access key ids) and for database URLs
with embedded passwords. Lines that only contain documented placeholders are ignored:
`example.invalid` hosts, `@127.0.0.1`, `@host`, `user:password@`, `change-me` and
`ci-only-password`. Keep example connection strings within those forms.

The pre-commit hook scans staged changes and additionally refuses to commit deployment files
by name: `*.db`, `*.sqlite`, `*.sqlite3`, `*.wav`, `*.pem`, `*.key`, `.env`,
`smsgateway.conf`, `sim.conf`, `integrations.conf`, `addressbook.conf`, `anonymous.conf`,
`smsgateway.env`. The same names are listed in `.gitignore`.

CI runs the formatter, Clippy with warnings as errors, the tests (SQLite, MariaDB, MySQL),
release builds for both architectures, `cargo-deny` and `gitleaks` on every push to `main`
and every pull request.

Dependencies are pinned by `Cargo.lock` and built with `--locked`. TLS uses rustls; there is
no dependency on OpenSSL.

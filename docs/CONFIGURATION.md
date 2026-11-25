# Configuration reference

- [Files and loading order](#files-and-loading-order)
- [Overrides](#overrides)
- [Secret references](#secret-references)
- [Validation and `check-config`](#validation-and-check-config)
- [Reload semantics](#reload-semantics)
- [smsgateway.conf](#smsgatewayconf)
- [sim.conf](#simconf)
- [integrations.conf](#integrationsconf)
- [addressbook.conf](#addressbookconf)
- [anonymous.conf](#anonymousconf)
- [smsgateway.env](#smsgatewayenv)

All files use TOML. Every setting is optional and has a default, so an empty
`smsgateway.conf` is valid. The shipped examples in `config/` list every setting with its
default value; a test keeps them in step with the code.

The **Applies** column in the tables below uses:

| Value | Meaning |
| --- | --- |
| reload | Takes effect with the next configuration reload |
| restart | Needs a restart of the service. Marked `(reported)` when a reload lists the setting under `restart_required`. |
| reconnect | Takes effect when the affected modem connects the next time. Changes to a modem's own settings, and to the SIM profile bound to it, trigger that reconnect automatically within a few seconds of a reload. |

## Files and loading order

| Order | File | Default path | Set by | If missing |
| --- | --- | --- | --- | --- |
| 1 | Main configuration | `/etc/thugsred/smsgateway.conf` | `--config` or `THUGSRED_SMSGW_CONFIG` | Error |
| 2 | SIM configuration | `/etc/thugsred/sim.conf` | `paths.sim_config` | Warning; one default SIM profile is used |
| 3 | Integrations | `/etc/thugsred/integrations.conf` | `integrations.config_file` | Warning; no integrations |
| 4 | Address book | `/etc/thugsred/addressbook.conf` | `paths.addressbook` | Silently skipped |
| 5 | Anonymous list | `/etc/thugsred/anonymous.conf` | `paths.anonymous` | Silently skipped |
| 6 | SMS providers | `/etc/thugsred/providers.conf` | `paths.providers` | Silently skipped; no providers |

Processing steps:

1. The main file is parsed.
2. Environment overrides, then `--set` overrides, are applied to the main file's values.
3. Secret references are resolved, in each file separately.
4. Sections are converted to their typed form; unknown keys become warnings.
5. `[[modems]]` entries are layered over `[modem]`.
6. The other files are read from the paths the (overridden) main file names.
7. Everything is validated together.

The modem profile directory (`paths.modem_profiles`) is read at startup only; see
[MODEMS.md](MODEMS.md#adding-a-custom-profile).

## Overrides

Overrides apply to settings of the **main file** only.

**Environment:** `THUGSRED_SMSGW__SECTION__FIELD=value`. The name after the prefix is split
at double underscores and lower-cased.

```sh
THUGSRED_SMSGW__API__PORT=9090
THUGSRED_SMSGW__GENERAL__LOG_LEVEL=debug
THUGSRED_SMSGW__API__ALLOWED_CIDRS='["127.0.0.0/8"]'
```

**Command line:** `--set section.field=value`, repeatable, applied after the environment.

```sh
thugsred-smsgateway --set api.port=9090 --set 'api.tokens=["0123456789abcdef-example"]' check-config
```

In both forms the value is parsed as a TOML literal (number, boolean, quoted string, array);
anything that does not parse is taken as a plain string, so `debug` and `"debug"` are
equivalent. Overrides given to `run` stay in effect across reloads. Overrides that name a
section or key that does not exist produce the usual unknown-key warning.

## Secret references

Any string value in any of the configuration files may contain references:

| Form | Resolved to |
| --- | --- |
| `"${NAME}"` | The environment variable `NAME`; if it is not set, the entry `NAME` in the file `smsgateway.env` located **next to the main configuration file** |
| `"${file:/absolute/path}"` | The content of that file, without trailing line breaks |

A reference may be part of a longer string.

```toml
[database]
mysql_url = "${SMSGW_MYSQL_URL}"

[api]
tokens = ["${file:/etc/thugsred/secrets/api-token}"]
```

How the two sources of `${NAME}` work together:

- The **service** receives the variables from systemd, which loads
  `/etc/thugsred/smsgateway.env` through `EnvironmentFile=` in the unit.
- The **command-line tool** (`check-config`, `status`, `doctor`, `sms send`, ...) reads the
  same file directly when a variable is not in its environment. The file is installed with
  mode `0600`, owner root, so this works for root (`sudo`) and fails for everybody else with
  `environment variable NAME is not set (and not found in smsgateway.env, which may need
  root to read)`.
- The process environment always takes precedence over the file.
- The file is parsed as simple `NAME=value` lines; blank lines and lines starting with `#`
  are ignored, and one pair of surrounding single or double quotes is removed from the value.

Rules:

- An unresolved reference is an error, except inside an integration, routing rule or
  automation rule that has `enabled = false` (integrations are disabled unless they say
  otherwise), where it resolves to an empty string. This lets you keep prepared but disabled
  entries whose secrets are not provisioned yet.
- Files referenced with `${file:...}` must be readable by the service account, for example
  owner `root`, group `thugsred-smsgateway`, mode `0640`.
- Resolved secrets are never printed: see [redaction](#redaction).

## Validation and `check-config`

```sh
thugsred-smsgateway check-config            # validate
thugsred-smsgateway check-config --show     # also print the effective configuration, redacted
thugsred-smsgateway --json check-config     # {"valid": true, "warnings": [...], "config": null}
make check-config CONFIG=/path/to/smsgateway.conf
```

Example output:

```text
warning: unknown configuration key `api.tokenz` ignored
Configuration is valid (1 modem(s), 1 SIM profile(s), 10 integration(s), 0 routing rule(s), 3 contact(s)).
  main:         /etc/thugsred/smsgateway.conf
  sim:          /etc/thugsred/sim.conf
  integrations: /etc/thugsred/integrations.conf
```

An invalid configuration lists **all** problems at once and exits with a non-zero status:

```text
error: configuration is invalid:
  - api.port: must be greater than zero
  - api.authentication: invalid value "basic" (expected one of: none, bearer)
```

### Errors

Errors make the configuration unusable: at startup the daemon does not start, on reload the
new configuration is rejected. They include TOML syntax errors, wrong value types, values
outside the allowed set or range (listed in the tables below), unresolved secret references
and broken cross-references:

- `[[modems]]`: duplicate ids, the same device on two enabled modems, `sim` naming an
  undefined SIM profile, `init_commands` entries that are not a single AT command;
- `sms.default_modem`, `gnss.modem`, automation `modem`: must name a configured modem;
- routing rule `destinations`: must name defined integrations;
- duplicate SIM ids, integration names, routing rule names or automation rule names;
- enabled integrations missing the fields their type requires.

### Warnings

Warnings do not prevent loading. They are printed by `check-config`, logged at startup and
after a reload, shown by `doctor`, and returned by `GET /api/v1/config/status`.

| Warning | Cause |
| --- | --- |
| `unknown configuration section ... ignored` | A top-level table the gateway does not know (typo) |
| `unknown configuration key ... ignored` | A key the section does not have (typo) |
| `SIM configuration ... not found; using defaults` | `sim.conf` missing |
| `integrations configuration ... not found; no integrations loaded` | `integrations.conf` missing |
| `... is accessible by other users; restrict it with chmod 640` | `sim.conf` or `anonymous.conf` is readable, writable or executable by "other" |
| `several modems and SIM profiles are configured; set sim on each [[modems]] entry ...` | Ambiguous SIM binding |
| `api: authentication is disabled and allowed_cidrs admits every address ...` | `authentication = "none"` together with `0.0.0.0/0` or `::/0` |
| `calls.recording_enabled has no effect unless calls.auto_answer is also enabled` | |
| `<integration>: enabled but subscribed to no events` | |
| `<integration>: GET webhooks place message data in the URL ...` | |
| `<integration>: TLS certificate verification is disabled` | `verify_tls = false` |

### Redaction

`check-config --show`, `GET /api/v1/config` and the diagnostics report print the effective
configuration with secrets masked:

| Keys | Shown as |
| --- | --- |
| `pin`, `password`, `mysql_url`, `tokens`, `admin_tokens`, `signing_secret`, `bot_token`, `token`, `webhook_url`, `chat_id` | `***` |
| every value under an integration's `headers` | `***` |
| `iccid`, `imsi` | Masked except the last four characters |
| `url` | Scheme, host and port only, followed by `/***` |
| `proxy`, `mmsc_url` | Reduced the same way when they contain credentials or a query string |

## Reload semantics

Trigger a reload with any of:

```sh
sudo systemctl reload thugsred-smsgateway     # SIGHUP
thugsred-smsgateway reload                    # administrative API call
curl -X POST http://127.0.0.1:8080/api/v1/config/reload
```

What happens:

1. All files are read and validated again, with the same `--config` and `--set` options the
   daemon was started with.
2. **If the result is invalid, nothing changes.** The running configuration stays in effect,
   the error is logged to the audit log, recorded as `last_reload_error`, written to the
   audit trail (`config.reload_failed`), and a `gateway.error` event is emitted. The API
   call returns `422` with code `invalid_configuration`.
3. If it is valid, the new configuration replaces the old one in a single atomic step. No
   request or task ever sees a mixture of old and new settings.
4. The log level is applied, sender workers are woken, the audit trail records
   `config.reload`, and a `gateway.config_reloaded` event is emitted.
5. Changed settings that only take effect after a restart are returned and logged as
   `restart_required`.

`restart_required` reports changes to: `database`, `api.enabled/bind/port`,
`api.max_request_size_kb/request_timeout_seconds`, `web.enabled`, `metrics`, the log files,
format and rotation, `paths`, the set of modems (adding or removing a modem),
`integrations.persistent_queue`, `integrations.worker_count` and `general.instance_id`.

Worth knowing:

- Enabling or disabling an already configured modem is applied live. Only adding or removing
  a `[[modems]]` entry needs a restart.
- PIN, SMSC, network selection and roaming settings of a SIM profile are applied when the
  modem connects. After a reload the supervisor notices that the SIM profile bound to its
  modem changed and reconnects the modem on its own, within a few seconds.
- Modem profile files (`paths.modem_profiles`) are read at startup only; changing a file is
  not detected by a reload.

Check the outcome with `GET /api/v1/config/status` or the `config` object in
`GET /api/v1/status`:

```json
{
  "valid": true,
  "loaded_at": "2026-11-24T10:00:00.000Z",
  "last_reload_at": "2026-11-24T11:30:00.000Z",
  "last_reload_error": null,
  "warnings": [],
  "restart_required": ["api.enabled/bind/port"]
}
```

## smsgateway.conf

Top-level key:

| Key | Type | Default | Valid values | Applies |
| --- | --- | --- | --- | --- |
| `config_version` | integer | `1` | `1`; any other value is an error | restart |

### [general]

| Field | Type | Default | Valid values | Applies |
| --- | --- | --- | --- | --- |
| `instance_name` | string | `"THUGSred SMS Gateway"` | Free text; used in integration messages | reload |
| `instance_id` | string | `"gateway-01"` | Non-empty; letters, digits, `-`, `_`, `.` | restart (reported) |
| `timezone` | string | `"UTC"` | IANA time zone name. Defines where a day and a month start for SMS quotas. | reload |
| `log_level` | string | `"info"` | `trace`, `debug`, `info`, `warn`, `error` | reload |
| `shutdown_timeout_seconds` | integer | `30` | Seconds to wait for tasks at shutdown (at least 1 is used) | reload |

### [paths]

All changes need a restart (reported). `home`, `data`, `runtime`, `recordings`, `attachments`
and `backups` must be absolute paths.

| Field | Type | Default | Purpose |
| --- | --- | --- | --- |
| `home` | string | `"/opt/THUGSred-smsGateway"` | Installation directory. Informational in this release. |
| `data` | string | `"/var/lib/thugsred-smsgateway"` | Data directory; its filesystem is the one monitored for free space |
| `runtime` | string | `"/run/thugsred-smsgateway"` | Runtime directory (created at start; not otherwise used in this release) |
| `recordings` | string | `"/opt/THUGSred-smsGateway/recordings"` | Call recordings; downloads are served only from inside it |
| `attachments` | string | `"/var/lib/thugsred-smsgateway/attachments"` | Downloaded MMS content; downloads are served only from inside it |
| `backups` | string | `"/opt/THUGSred-smsGateway/backups"` | Default target of `db backup` |
| `modem_profiles` | string | `"/etc/thugsred/modem-profiles"` | Directory with custom modem profiles |
| `sim_config` | string | `"/etc/thugsred/sim.conf"` | SIM configuration file |
| `addressbook` | string | `"/etc/thugsred/addressbook.conf"` | Address book file |
| `anonymous` | string | `"/etc/thugsred/anonymous.conf"` | Always-anonymous list |
| `providers` | string | `"/etc/thugsred/providers.conf"` | SMS provider file |

The daemon creates `data`, `runtime`, `recordings`, `attachments` and `backups` at start when
it is allowed to. If you move directories, extend the unit's `ReadWritePaths`
([INSTALLATION.md](INSTALLATION.md#the-systemd-unit)).

### [modem] and [[modems]]

`[modem]` holds the settings of the modem, or the defaults for all modems when `[[modems]]`
entries exist. Each `[[modems]]` entry inherits `[modem]` (except `id` and `sim`) and may
override any field. Changes to a modem's settings are applied by reconnecting that modem.

| Field | Type | Default | Valid values | Applies |
| --- | --- | --- | --- | --- |
| `id` | string | `"modem-1"` (`modem-N` for the N-th `[[modems]]` entry) | Letters, digits, `-`, `_`; unique. Adding, removing or renaming a modem needs a restart. | restart (reported) |
| `sim` | string | `""` | Id of a SIM profile in `sim.conf`; empty for automatic binding | reconnect |
| `enabled` | boolean | `true` | `false`: the modem is not opened (state `DISABLED`) | reload |
| `profile` | string | `"auto"` | `auto`, `generic`, `simcom-2g`, `simcom-lte`, `quectel`, or a custom profile name | reconnect |
| `device` | string | `"/dev/ttyS0"` | Serial device path; non-empty; unique among enabled modems | reconnect |
| `baud_rate` | integer | `115200` | Greater than 0 | reconnect |
| `data_bits` | integer | `8` | 5 to 8 | reconnect |
| `stop_bits` | integer | `1` | 1 or 2 | reconnect |
| `parity` | string | `"none"` | `none`, `even`, `odd` | reconnect |
| `flow_control` | string | `"none"` | `none`, `hardware`, `software` | reconnect |
| `command_timeout_seconds` | integer | `10` | Greater than 0. Time allowed for an ordinary AT command. | reconnect |
| `message_timeout_seconds` | integer | `60` | Greater than 0. Time allowed for the network to accept an SMS. | reconnect |
| `poll_interval_seconds` | integer | `15` | Greater than 0. How often stored messages are fetched as a safety net. | reconnect |
| `healthcheck_interval_seconds` | integer | `30` | Greater than 0 | reconnect |
| `reconnect_delay_seconds` | integer | `5` | Greater than 0. First retry delay; doubles per attempt. | reconnect |
| `max_reconnect_delay_seconds` | integer | `300` | Not smaller than `reconnect_delay_seconds` | reconnect |
| `auto_reconnect` | boolean | `true` | `false`: after a failure the modem stays `DISCONNECTED` until `modem reconnect` | reconnect |
| `auto_detect_capabilities` | boolean | `true` | | reconnect |
| `preferred_network` | string | `"auto"` | `auto`, `2g`, `3g`, `4g`. Needs a profile with `[network_modes]`; otherwise ignored with a warning in the log. | reconnect |
| `init_commands` | list of strings | `[]` | Each a single AT command (starts with `AT`, no line breaks). Sent after the profile's own commands. | reconnect |
| `sms_storage` | string | `""` | For example `"SM"` or `"ME"`; empty keeps the profile or modem default | reconnect |

See [MODEMS.md](MODEMS.md#multi-modem-setup) for a multi-modem example.

### [sms]

All fields apply on reload.

| Field | Type | Default | Valid values and meaning |
| --- | --- | --- | --- |
| `enabled` | boolean | `true` | `false` disables sending and receiving |
| `mode` | string | `"pdu"` | `pdu` only |
| `receive_enabled` | boolean | `true` | `false`: notifications and stored messages are not fetched |
| `send_enabled` | boolean | `true` | `false`: submissions are refused with `503` and the queue is paused |
| `auto_encoding` | boolean | `true` | Pick GSM 7-bit or UCS-2 automatically for requests with `encoding: "auto"` |
| `default_encoding` | string | `"gsm7"` | `gsm7`, `ucs2`. Used for `auto` requests when `auto_encoding` is false. |
| `allow_unicode` | boolean | `true` | `false`: text that needs UCS-2 is rejected |
| `allow_multipart` | boolean | `true` | `false`: messages that need more than one part are rejected |
| `allow_binary` | boolean | `true` | Allow 8-bit payloads through the API |
| `allow_flash_sms` | boolean | `true` | Allow class 0 messages to be sent |
| `allow_service_messages` | boolean | `true` | `false`: received service and WAP Push messages are stored but produce no event |
| `request_delivery_reports` | boolean | `true` | Default for requests that do not set `delivery_report` |
| `store_raw_pdu` | boolean | `true` | Keep the hexadecimal PDU with each message and delivery report |
| `store_sent_messages` | boolean | `true` | `false`: text, payload and PDUs of a sent message are erased from the message record after submission |
| `delete_from_modem_after_persist` | boolean | `true` | Delete a message from modem storage once it is stored in the database |
| `multipart_timeout_seconds` | integer | `600` | Greater than 0. Incomplete multipart messages are stored as `incomplete` after this time. |
| `max_message_parts` | integer | `10` | 1 to 255 |
| `max_send_attempts` | integer | `3` | Greater than 0. Rejected submissions before a message fails. |
| `send_retry_delay_seconds` | integer | `10` | First retry delay; doubles per attempt, at most one hour |
| `outbound_rate_limit_per_minute` | integer | `10` | SMS segments per modem and minute; 0 = unlimited |
| `deduplication_enabled` | boolean | `true` | Skip PDUs whose fingerprint is already stored. See [DATABASE.md](DATABASE.md#deduplication-fingerprints) for what `false` means. |
| `default_modem` | string | `""` | A configured modem id. The modem for messages that name none; empty = any ready modem. |
| `per_destination_interval_seconds` | integer | `0` | Minimum gap between two messages to the same number; 0 disables |
| `default_validity_minutes` | integer | `0` | Default validity period; 0 lets the network decide. Requests may pass 1 to 635040. |
| `queue_expiry_seconds` | integer | `86400` | Unsent messages expire after this time (counted from the scheduled time for scheduled messages); 0 = never |
| `delivery_report_window_days` | integer | `7` | How long after submission a delivery report is still matched to its message |

`store_sent_messages = false` erases the message record only. The `sms.queued` and `sms.sent`
events, which are kept for `retention.integration_event_days`, still contain the text.

### [mms]

All fields apply on reload.

| Field | Type | Default | Valid values and meaning |
| --- | --- | --- | --- |
| `enabled` | boolean | `false` | Enables retrieval (together with `download_attachments`). Notifications are decoded regardless. |
| `provider_profile` | string | `"auto"` | Not used by this release |
| `apn` | string | `""` | Not used by this release |
| `mmsc_url` | string | `""` | Carrier MMSC URL (`http` or `https`). Required when retrieval is enabled. Retrieval only ever contacts this host. |
| `proxy` | string | `""` | Optional HTTP(S) proxy URL for retrieval |
| `download_attachments` | boolean | `false` | Experimental; see [MODEMS.md](MODEMS.md#mms-notes) |
| `max_attachment_size_mb` | integer | `10` | Larger downloads are rejected (at least 1 is used) |

### [ussd]

All fields apply on reload.

| Field | Type | Default | Valid values and meaning |
| --- | --- | --- | --- |
| `enabled` | boolean | `true` | `false`: the USSD endpoint answers `403` |
| `timeout_seconds` | integer | `30` | Greater than 0. Time to wait for the network's answer. |
| `allow_arbitrary_codes` | boolean | `false` | `false`: only the codes listed under `[ussd.commands]` can be run |
| `[ussd.commands]` | table of strings | empty | Name to code, for example `balance = "*101#"`. A code starts with `*` or `#`, contains only digits, `*` and `#`, and ends with `#`. |

### [database]

All changes need a restart (reported).

| Field | Type | Default | Valid values and meaning |
| --- | --- | --- | --- |
| `driver` | string | `"sqlite"` | `sqlite`, `mysql` (also for MariaDB) |
| `sqlite_path` | string | `"/var/lib/thugsred-smsgateway/messages.db"` | Non-empty when the driver is `sqlite`. The parent directory is created when missing. |
| `mysql_url` | string | `""` | Must start with `mysql://` when the driver is `mysql`: `mysql://user:password@host:3306/database` |
| `max_connections` | integer | `5` | Greater than 0 |
| `auto_migrate` | boolean | `true` | Apply schema migrations at start. `false`: the daemon refuses to start when the schema is not current. |
| `sqlite_wal` | boolean | `true` | Write-ahead logging; `false` uses the rollback journal |

### [retention]

All fields apply on reload. Values are days; `0` keeps data indefinitely.

| Field | Type | Default | Covers |
| --- | --- | --- | --- |
| `message_days` | integer | `0` | Messages with their parts, status history, delivery reports and finished queue entries |
| `recording_days` | integer | `30` | Call recordings (database rows and files) |
| `attachment_days` | integer | `30` | Attachments (database rows and files) |
| `integration_event_days` | integer | `30` | Events, finished integration deliveries and their attempts, dead letters, modem health samples, automation log |
| `audit_days` | integer | `90` | Audit trail |
| `cleanup_interval_seconds` | integer | `3600` | Greater than 0. How often retention runs. |

See [DATABASE.md](DATABASE.md#retention) for exactly what is and is not removed.

### [api]

| Field | Type | Default | Valid values and meaning | Applies |
| --- | --- | --- | --- | --- |
| `enabled` | boolean | `true` | `false` disables the API, the dashboard, metrics and the command-line tool's access to the daemon | restart (reported) |
| `bind` | string | `"0.0.0.0"` | An IP address. Use `"127.0.0.1"` behind a reverse proxy. | restart (reported) |
| `port` | integer | `8080` | 1 to 65535 | restart (reported) |
| `authentication` | string | `"none"` | `none`, `bearer` | reload |
| `tokens` | list of strings | `[]` | Bearer tokens, each at least 16 characters; at least one when `authentication = "bearer"` | reload |
| `allowed_cidrs` | list of strings | `["127.0.0.0/8", "::1/128", "10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]` | CIDR networks or single addresses allowed to connect. An empty list admits nobody. | reload |
| `trusted_proxies` | list of strings | `[]` | CIDR networks or addresses of reverse proxies whose `X-Forwarded-For` header is honoured | reload |
| `allowed_hosts` | list of strings | `[]` | Accepted `Host` header values (with or without port); empty disables the check | reload |
| `cors_enabled` | boolean | `false` | | reload |
| `cors_allowed_origins` | list of strings | `[]` | Exact origins, for example `"https://app.example.invalid"`; at least one when CORS is enabled | reload |
| `max_request_size_kb` | integer | `256` | Greater than 0 | restart (reported) |
| `request_timeout_seconds` | integer | `30` | Greater than 0 | restart (reported) |
| `rate_limit_per_minute` | integer | `120` | Requests per client address and minute; 0 = unlimited | reload |
| `admin_enabled` | boolean | `true` | `false` disables every administrative endpoint | reload |
| `local_networks` | list of strings | `[]` | Networks that use the dashboard/API **without a token** and with full access, even when `authentication = "bearer"`; external callers outside the list still need a token. Must be within `allowed_cidrs`. | reload |
| `admin_allowed_cidrs` | list of strings | `[]` | Additional restriction for administrative endpoints; empty = same as `allowed_cidrs` | reload |
| `admin_tokens` | list of strings | `[]` | Tokens required for administrative endpoints, each at least 16 characters; empty = same access as other endpoints | reload |

See [SECURITY.md](SECURITY.md#access-controls) for how these combine.

### [web]

| Field | Type | Default | Valid values and meaning | Applies |
| --- | --- | --- | --- | --- |
| `enabled` | boolean | `true` | Serve the dashboard | restart (reported) |
| `title` | string | `"THUGSred SMS Gateway"` | Dashboard title | reload |
| `theme` | string | `"dark"` | `dark`, `light`. Default theme; a viewer can switch in the browser. | reload |
| `refresh_interval_seconds` | integer | `10` | Greater than 0. Polling interval of the dashboard. | reload |
| `show_message_content` | boolean | `true` | Display setting of the dashboard only. The REST API still returns message text. | reload |
| `show_modem_information` | boolean | `true` | Display setting of the dashboard only. The REST API still returns modem details. | reload |

### [logging]

| Field | Type | Default | Valid values and meaning | Applies |
| --- | --- | --- | --- | --- |
| `system_log` | string | `"/var/log/thugsred-smsgateway/system.log"` | Everything that is not in one of the other three logs | restart (reported) |
| `messages_log` | string | `"/var/log/thugsred-smsgateway/messages.log"` | Message and call lifecycle | restart (reported) |
| `integrations_log` | string | `"/var/log/thugsred-smsgateway/integrations.log"` | Integration deliveries | restart (reported) |
| `audit_log` | string | `"/var/log/thugsred-smsgateway/audit.log"` | Administrative and state-changing actions | restart (reported) |
| `format` | string | `"json"` | `json`, `text` | restart (reported) |
| `journal_enabled` | boolean | `true` | Also log to standard error, which systemd stores in the journal | restart (reported) |
| `rotation` | string | `"daily"` | `daily`, `hourly`, `never`. Periods are in UTC; a finished file is renamed to `<name>.<period>`. | restart (reported) |
| `retention_days` | integer | `30` | Rotated files older than this are deleted; 0 keeps them | restart (reported) |
| `log_message_content` | boolean | `false` | Include message text in `messages.log`. Off by default because it may be personal data. | reload |
| `redact_sensitive_values` | boolean | `true` | Mask telephone numbers in the logs (for example `+45******00`) | reload |

Log files are created with mode `0640`. A log file that cannot be opened is disabled with a
warning on standard error; the daemon still starts.

### [calls]

All fields apply on reload.

| Field | Type | Default | Valid values and meaning |
| --- | --- | --- | --- |
| `enabled` | boolean | `true` | `false`: call signals are ignored |
| `log_incoming` | boolean | `true` | Record incoming calls |
| `log_outgoing` | boolean | `true` | Record calls dialled through the raw AT endpoint |
| `auto_answer` | boolean | `false` | Answer incoming calls. Opt-in; check consent and recording laws first. |
| `recording_enabled` | boolean | `false` | Record answered calls. Needs `auto_answer` and a capture device wired to the modem. |
| `recording_duration_seconds` | integer | `15` | 1 to 300. Also the time an auto-answered call is held when recording is off. |
| `audio_backend` | string | `"auto"` | `auto`, `alsa`, `none`. `auto` and `alsa` both use ALSA; `none` disables recording. |
| `audio_device` | string | `""` | ALSA capture device, for example `"plughw:1,0"`. Empty: recording is reported as `unavailable`. |
| `recording_format` | string | `"wav"` | `wav` only |
| `hangup_after_recording` | boolean | `true` | Hang up when the recording (or hold time) ends |
| `ring_timeout_seconds` | integer | `8` | Seconds without a `RING` before an unanswered call is logged as missed (at least 2 is used) |

See [CALLS.md](CALLS.md).

### [gnss]

| Field | Type | Default | Valid values and meaning | Applies |
| --- | --- | --- | --- | --- |
| `enabled` | boolean | `false` | Power the GNSS receiver on and poll positions | reconnect |
| `poll_interval_seconds` | integer | `60` | Greater than 0 | reconnect |
| `store_location_history` | boolean | `false` | Keep each fix in the database | reload |
| `modem` | string | `""` | A configured modem id; empty = first modem | reconnect |

### [integrations]

| Field | Type | Default | Valid values and meaning | Applies |
| --- | --- | --- | --- | --- |
| `enabled` | boolean | `true` | Master switch: `false` means no event is routed to any integration | reload |
| `config_file` | string | `"/etc/thugsred/integrations.conf"` | Integrations file | reload |
| `worker_count` | integer | `4` | 1 to 64. Concurrent deliveries. | restart (reported) |
| `persistent_queue` | boolean | `true` | `false`: deliveries that were pending when the daemon stopped are discarded at the next start | restart (reported) |
| `max_attempts` | integer | `10` | Greater than 0. Default attempts per delivery. | reload |
| `retry_initial_seconds` | integer | `5` | Greater than 0. First retry delay; doubles per attempt. | reload |
| `retry_max_seconds` | integer | `3600` | Upper bound of the retry delay | reload |

### [metrics]

| Field | Type | Default | Valid values and meaning | Applies |
| --- | --- | --- | --- | --- |
| `enabled` | boolean | `true` | Serve Prometheus metrics | restart (reported) |
| `path` | string | `"/metrics"` | Must start with `/` | restart (reported) |

### [security]

All fields apply on reload.

| Field | Type | Default | Valid values and meaning |
| --- | --- | --- | --- |
| `allow_raw_at_commands` | boolean | `false` | Enables `POST /api/v1/modem/at` |
| `allow_raw_pdu_submission` | boolean | `false` | Enables `POST /api/v1/sms/send-pdu` and a non-zero `protocol_identifier` in send requests |
| `allow_destination_overrides` | boolean | `false` | Lets administrative callers bypass `destination_allowlist` with `override_allowlist: true` |
| `max_outbound_queue_size` | integer | `10000` | Greater than 0. New submissions are refused with `429` while this many messages are waiting. |
| `destination_allowlist` | list of strings | `[]` | When non-empty, only matching destinations can be messaged. Exact numbers or prefixes ending in `*` (for example `"+45*"`). |
| `destination_blocklist` | list of strings | `[]` | Matching destinations are always refused |

## sim.conf

Install with mode `0640`: it may contain a PIN. Two layouts exist and may be combined.

**Single SIM:** the sections `[sim]`, `[network]`, `[identity]` and `[limits]` define one
profile with the id `default`.

**Several SIMs:** `[[sims]]` entries with the sub-tables `[sims.network]`, `[sims.identity]`
and `[sims.limits]`. Without an `id`, entries are named `sim-1`, `sim-2`, ... in file order.

When to expect changes: everything applies with a reload. `pin`, `pin_enabled`,
`pin_max_attempts`, `smsc`, `iccid` and the settings under `[network]` take effect when the
modem connects; a reload that changes the SIM profile bound to a connected modem makes that
modem reconnect automatically within a few seconds. (Any change to the bound profile does,
including a changed limit or description.)

### [sim] / [[sims]]

| Field | Type | Default | Valid values and meaning |
| --- | --- | --- | --- |
| `id` | string | `"default"` / `"sim-N"` | Unique. Referenced by `sim =` in `[[modems]]`. |
| `name` | string | `""` | Descriptive |
| `owner` | string | `""` | Descriptive |
| `description` | string | `""` | Descriptive |
| `mobile_number` | string | `""` | Digits with an optional leading `+`. Gateway metadata: shown in the dashboard, used by the `from` selector. It does not change the number recipients see. |
| `country_code` | string | `""` | Descriptive |
| `carrier` | string | `""` | Descriptive |
| `pin` | string | `""` | 4 to 8 digits when `pin_enabled` is true. Prefer `"${file:/etc/thugsred/secrets/sim-pin}"`. |
| `pin_enabled` | boolean | `false` | Enter the PIN when the SIM asks for one |
| `pin_max_attempts` | integer | `1` | 1 or 2. A rejected PIN is never retried automatically. |
| `smsc` | string | `""` | Service centre number, digits with an optional `+`. Empty keeps the one stored on the SIM. |
| `iccid` | string | `""` | Binds this profile to a SIM regardless of the modem it sits in |
| `imsi` | string | `""` | Descriptive; not used by this release |

### [network] / [sims.network]

| Field | Type | Default | Valid values and meaning |
| --- | --- | --- | --- |
| `selection` | string | `"automatic"` | `automatic`, `manual` |
| `operator_code` | string | `""` | Numeric MCC+MNC (at least five digits, for example `"00101"`); required for manual selection |
| `roaming_enabled` | boolean | `false` | `false`: nothing is sent while the SIM is roaming |
| `apn` | string | `""` | Descriptive; shown by the API, not used by this release |
| `username` | string | `""` | Not used by this release |
| `password` | string | `""` | Not used by this release |

### [identity] / [sims.identity]

Descriptive metadata shown by `GET /api/v1/sim`: `manufacturer`, `model`, `asset_id`,
`location`, `notes`. All strings, default empty.

### [limits] / [sims.limits]

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `daily_sms_limit` | integer | `0` | SMS segments per day; 0 = unlimited |
| `monthly_sms_limit` | integer | `0` | SMS segments per month; 0 = unlimited |

Days and months start at midnight in `general.timezone`. A multipart message counts one
segment per part. See [SECURITY.md](SECURITY.md#sms-spending-protection) for how quotas are
enforced and their limits.

## integrations.conf

The file may contain `[[integration]]`, `[[routing.rule]]` and `[[automation.rule]]` entries.
Everything applies on reload. Semantics, payloads and examples are in
[INTEGRATIONS.md](INTEGRATIONS.md); this section is the field reference.

### [[integration]]

| Field | Type | Default | Used by | Valid values and meaning |
| --- | --- | --- | --- | --- |
| `name` | string | required | all | Unique; letters, digits, `-`, `_` |
| `type` | string | `"webhook"` | all | `webhook`, `discord`, `slack`, `teams`, `telegram`, `ntfy`, `gotify`, `syslog` |
| `enabled` | boolean | `false` | all | Nothing is forwarded unless this is `true` |
| `events` | list of strings | `[]` | all | Event names; `*` and prefix patterns such as `sms.*` are accepted |
| `url` | string | `""` | webhook, ntfy, gotify, telegram | Target URL (webhook), server base URL (ntfy, gotify), optional API base URL (telegram) |
| `webhook_url` | string | `""` | discord, slack, teams | Incoming webhook or workflow URL |
| `method` | string | `"POST"` | webhook | `GET`, `POST`, `PUT` |
| `content_type` | string | `"application/json"` | webhook | Content type of a `body_template`; also selects how template values are escaped |
| `timeout_seconds` | integer | `10` | all | Greater than 0 |
| `verify_tls` | boolean | `true` | HTTP types | `false` disables certificate verification (warning) |
| `max_attempts` | integer | `0` | all | 0 = `integrations.max_attempts` |
| `retry_initial_seconds` | integer | `0` | all | 0 = `integrations.retry_initial_seconds` |
| `retry_max_seconds` | integer | `0` | all | 0 = `integrations.retry_max_seconds` |
| `rate_limit_per_minute` | integer | `0` | all | Deliveries per minute; 0 = unlimited |
| `include_raw_pdu` | boolean | `false` | webhook | Include `raw_pdu` in the event envelope |
| `anonymize` | boolean | `false` | all | Replace every telephone number and address book name |
| `include_message_body` | boolean | `true` | all | `false`: message text is replaced by `[redacted]` |
| `sign_payload` | boolean | `false` | webhook | Add the `X-THUGSred-Signature` header |
| `signing_secret` | string | `""` | webhook | Required when `sign_payload` is true |
| `[integration.headers]` | table of strings | empty | webhook | Extra request headers (sent as written, not templated) |
| `[integration.query]` | table of strings | empty | webhook | Query parameters; values are templates |
| `body_template` | string | `""` | webhook | Custom request body; replaces the JSON envelope |
| `template` | string | `""` | chat types, syslog | Message text template; empty uses the built-in text for the event |
| `username` | string | `""` | discord | Display name of the webhook post |
| `bot_token` | string | `""` | telegram | Required |
| `chat_id` | string | `""` | telegram | Required |
| `token` | string | `""` | ntfy, gotify | Access token (optional for ntfy, required for gotify) |
| `topic` | string | `""` | ntfy | Required |
| `priority` | integer | `0` | ntfy, gotify | ntfy: 1 to 5, 0 sends none. gotify: 0 means 5. |
| `server` | string | `""` | syslog | `host:port` (UDP) |
| `facility` | string | `"local0"` | syslog | `user`, `daemon`, `local0` to `local7`; anything else is treated as `local0` |
| `[integration.filter]` | table | empty | all | See the filter fields below |

Requirements checked only for enabled integrations: a valid `http(s)` URL in `url`
(webhook, ntfy, gotify) or `webhook_url` (discord, slack, teams); `bot_token` and `chat_id`
(telegram); `topic` (ntfy); `token` (gotify); `server` as `host:port` (syslog);
`signing_secret` when signing.

### Filter fields

Used in `[integration.filter]`, and directly (not in a sub-table) in routing and automation
rules. Empty lists and strings match everything.

| Field | Type | Meaning |
| --- | --- | --- |
| `sender_allowlist` | list of strings | Exact numbers or names, or prefixes ending in `*` |
| `sender_blocklist` | list of strings | |
| `recipient_allowlist` | list of strings | The gateway number that received the message |
| `recipient_blocklist` | list of strings | |
| `sender_regex` | string | Regular expression on the sender |
| `message_regex` | string | Regular expression on the message text |
| `message_contains` | list of strings | Case-insensitive substrings; any one matches |
| `directions` | list of strings | `inbound`, `outbound` |
| `message_types` | list of strings | `sms`, `flash`, `binary`, `service`, `mms_notification`, `wap_push` |
| `message_classes` | list of integers | Message class 0 to 3 |
| `application_ports` | list of integers | Source or destination port |
| `encodings` | list of strings | `gsm7`, `ucs2`, `8bit` |
| `instance_ids` | list of strings | `general.instance_id` values |
| `modem_ids` | list of strings | |
| `sim_ids` | list of strings | |
| `delivery_statuses` | list of strings | Delivery status, or message status when there is none |

Regular expressions are validated at load time.

### [[routing.rule]]

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | Unique |
| `enabled` | boolean | `true` | |
| `events` | list of strings | `[]` | Event patterns the rule applies to. A rule with no events never matches. |
| `destinations` | list of strings | `[]` | Names of defined integrations |
| `stop_processing` | boolean | `false` | Stop evaluating later rules when this one matches |
| filter fields | | | Written directly in the rule |

### [[automation.rule]]

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | Unique |
| `enabled` | boolean | `false` | Opt-in |
| `action` | string | `"reply"` | `reply`, `forward` |
| `to` | list of strings | `[]` | Forward destinations; required for `forward`, each a valid number |
| `template` | string | `""` | Text of the message to send; required when enabled |
| `modem` | string | `""` | A configured modem id; empty reuses the receiving modem |
| `cooldown_seconds` | integer | `3600` | Minimum time between two automatic messages triggered by the same sender; 0 disables |
| `max_per_hour` | integer | `10` | Greater than 0. Upper bound per rule and hour. |
| filter fields | | | Written directly in the rule. An enabled `reply` rule must set `sender_allowlist` or `sender_regex`. |

## addressbook.conf

Optional. Applies on reload. Contacts can also be saved from the dashboard's Contacts page or
through `/api/v1/contacts`; those are stored in the database, need no reload, and take
precedence over an entry of this file for the same number. Entries of this file are shown
as read-only there. Two notations, which may be mixed:

```toml
[contacts]
"+4500000000" = "Example Person"
"ExampleService" = "Example service (alphanumeric sender)"

[[contact]]
name = "Monitoring system"
number = "+4500000001"
```

| Field | Type | Meaning |
| --- | --- | --- |
| `[contacts]` | table | Number (or alphanumeric sender, or prefix ending in `*`) to name |
| `[[contact]] name` | string | Required |
| `[[contact]] number` | string | Required |

Matching: first by exact or prefix match (numbers are normalised: separators removed, a
leading `00` becomes `+`; names compare case-insensitively), then by the last eight digits
when at least one of the two numbers is in national notation (no leading `+`), so that
national and international notations of the same number both match. Two numbers that both
carry an international prefix must be identical.

Names appear in the dashboard, in the API (`sender_name`, `recipient_name`) and in
integration messages (`{{message.sender_display}}` renders as `Name (+number)`).

## anonymous.conf

Optional. Applies on reload. Install with mode `0640`.

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `numbers` | list of strings | `[]` | Exact numbers or prefixes ending in `*` |
| `label` | string | `"Anonymous"` | Text shown in place of an anonymised number; must not be empty |
| `redact_body` | boolean | `false` | Also withhold the message text of these numbers from integrations |

Exact entries also bridge national and international notation by the last eight digits,
like the address book; two international numbers must be identical. See
[INTEGRATIONS.md](INTEGRATIONS.md#anonymisation) for what is and is not hidden.

## providers.conf

Optional. Applies on reload. Install with mode `0640`; it holds an API token. Each
`[[provider]]` entry is an outbound route through an SMS provider's API, used by requests
that name `provider` or `sender_name`. Fields, callbacks and failure handling are described
in [PROVIDERS.md](PROVIDERS.md#configuration).

## smsgateway.env

`/etc/thugsred/smsgateway.env`, mode `0600`, owner root. Plain `NAME=value` lines. The unit
file loads it with `EnvironmentFile=-/etc/thugsred/smsgateway.env` (the file is optional),
and the command-line tool reads the file next to the main configuration file when a
referenced variable is not in its environment.

```sh
SMSGW_MYSQL_URL=mysql://smsgateway:change-me@db.example.invalid:3306/smsgateway
SMS_WEBHOOK_SECRET=<random string>
DISCORD_WEBHOOK_URL=<webhook URL>
```

Two uses:

- **Secret references:** `"${NAME}"` in a configuration file ([above](#secret-references)).
- **Overrides:** a line such as `THUGSRED_SMSGW__API__PORT=9090` overrides a setting of the
  main file for the service, because systemd places it in the service's environment. The
  command-line tool does not treat entries of the file as overrides, only as values for
  `${NAME}` references.

Changes to the file reach the service only when it is restarted: systemd reads environment
files when it starts the process, not on reload.

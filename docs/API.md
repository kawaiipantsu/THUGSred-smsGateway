# REST API

The API, the dashboard, the metrics endpoint and the event stream share one plain-HTTP
listener (`api.bind`, `api.port`; default `0.0.0.0:8080`). All API paths start with
`/api/v1`.

- [Conventions](#conventions)
- [Authentication and access control](#authentication-and-access-control)
- [Errors](#errors)
- [Health and metrics](#health-and-metrics)
- [Status and statistics](#status-and-statistics)
- [Modem and SIM](#modem-and-sim)
- [Sending](#sending)
- [Messages](#messages)
- [Queue](#queue)
- [PDU tools](#pdu-tools)
- [Calls, recordings and attachments](#calls-recordings-and-attachments)
- [Integrations and deliveries](#integrations-and-deliveries)
- [Events and the live stream](#events-and-the-live-stream)
- [History export](#history-export)
- [USSD and GNSS](#ussd-and-gnss)
- [Administration](#administration)
- [OpenAPI document](#openapi-document)

Examples use `http://127.0.0.1:8080` and no authentication. Identifiers and timestamps in the
examples are illustrative.

## Conventions

### Response envelope

Successful responses are JSON objects with `"success": true` and the result in `data`:

```json
{"success": true, "data": {"...": "..."}}
```

List endpoints add `pagination`:

```json
{"success": true, "data": [], "pagination": {"total": 0, "limit": 50, "offset": 0}}
```

Exceptions:

| Endpoint | Shape |
| --- | --- |
| `POST /api/v1/sms/send`, `POST /api/v1/sms/schedule` | Fields at the top level next to `success` (no `data`), status `202` |
| `GET /health`, `GET /ready` | Plain objects without `success` |
| `GET /metrics` | Prometheus text format |
| `GET /api/v1/openapi.json` | The OpenAPI document itself |
| `GET /api/v1/history/export`, recording and attachment downloads | File downloads |
| `GET /api/v1/events/stream` | Server-Sent Events |

### Pagination

`limit` (1 to 500, default 50) and `offset` (default 0). Results are newest first unless
stated otherwise.

### Timestamps and date filters

Timestamps are UTC in RFC 3339 format with milliseconds, for example
`2026-11-24T11:00:00.000Z`. Date filters accept an RFC 3339 timestamp or a plain
`YYYY-MM-DD` date, which is interpreted in UTC (start of day for lower bounds, end of day for
upper bounds).

### Request bodies

Bodies are JSON and must be sent with `Content-Type: application/json`. The maximum size is
`api.max_request_size_kb` (default 256 KiB). Send and PDU-encode requests reject unknown
fields.

### X-Request-Id

Every response carries an `X-Request-Id` header. A caller-supplied `X-Request-Id` is echoed
when it is 1 to 64 characters of letters, digits, `-`, `_` or `.`; otherwise the gateway
creates one. The id appears in the audit trail and becomes the `correlation_id` of a
submitted message when the request does not set one.

### Idempotency-Key

`POST /api/v1/sms/send` and `POST /api/v1/sms/schedule` accept an `Idempotency-Key` header of
1 to 128 printable ASCII characters. A request with a key that was used before returns the
**original** message with `"duplicate": true` and queues nothing new, even if the body
differs. Keys are remembered for as long as the message record exists.

### Other response headers

All responses carry `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
`Referrer-Policy: no-referrer` and a restrictive `Content-Security-Policy`. API responses are
sent with `Cache-Control: no-store`.

## Authentication and access control

Checks run in this order for every request. The first failing check ends the request.

| # | Check | Setting | Failure |
| --- | --- | --- | --- |
| 1 | Client address is in the allowed networks | `api.allowed_cidrs` | `403 forbidden` |
| 2 | `Host` header is accepted | `api.allowed_hosts` (empty disables) | `421 invalid_host` |
| 3 | Not a cross-site request (methods other than `GET`, `HEAD`, `OPTIONS`) | `Origin` must match `Host` or be listed in `api.cors_allowed_origins`; without `Origin`, `Sec-Fetch-Site: cross-site` is rejected | `403 forbidden` |
| 4 | Bearer token | `api.authentication = "bearer"`: a token from `api.tokens` or `api.admin_tokens` | `401 unauthorized` |
| 5 | Rate limit per client address | `api.rate_limit_per_minute` | `429 rate_limited` |

Details:

- **Client address.** The address of the TCP peer, unless the peer is listed in
  `api.trusted_proxies`; then the right-most address in `X-Forwarded-For` that is not itself
  a trusted proxy is used. A forwarded header from an untrusted peer is ignored.
- **Token.** `Authorization: Bearer <token>`. Tokens are compared in constant time.
- **No token needed** (even with bearer authentication): `/`, `/favicon.svg`, `/assets/*`,
  `/health`, `/ready`, `/api/v1/ui/config` and `OPTIONS` requests. `/api/v1/ui/config` holds
  no secrets; the dashboard needs it to render its login prompt. These paths are still
  subject to checks 1 and 2.
- **Rate limit.** Counted per client address in one-minute windows. `/health`, `/ready`, the
  dashboard assets and `/api/v1/ui/config` are not counted.
- **Metrics.** `/metrics` is treated like any API path: it needs a token when bearer
  authentication is on.
- **CORS.** With `api.cors_enabled`, responses to listed origins carry
  `Access-Control-Allow-Origin`; allowed methods are `GET, POST, DELETE, OPTIONS`, allowed
  headers `Authorization, Content-Type, Idempotency-Key, X-Request-Id`.

### Administrative endpoints

Endpoints marked **admin** below additionally require:

1. `api.admin_enabled = true`, else `403`;
2. the client address to be in `api.admin_allowed_cidrs` when that list is not empty, else
   `403`;
3. a token from `api.admin_tokens` when that list is not empty, else `401`.

With the defaults (`admin_enabled = true`, both lists empty) administrative endpoints are as
open as all others. The administrative endpoints are: modem reconnect, raw AT, PIN counter
reset, raw PDU submission, send with `override_allowlist`, message deletion, queue retry,
integration test, delivery replay, configuration view and reload, audit trail, diagnostics
and database backup.

State-changing calls are written to the audit trail with the client address and request id.

## Errors

```json
{"success": false, "error": {"code": "invalid_request", "message": "`to` must be a telephone number: digits with an optional leading '+'"}}
```

| HTTP status | `code` | Meaning |
| --- | --- | --- |
| 400 | `invalid_request` | Malformed body, query or field value |
| 401 | `unauthorized` | Missing or wrong bearer token |
| 403 | `forbidden` | Address not allowed, cross-site request, feature disabled, destination not permitted |
| 404 | `not_found` | Unknown endpoint, message, modem, integration, recording |
| 409 | `conflict` | The object is in a state that does not allow the action |
| 413 | `payload_too_large` | Body exceeds `api.max_request_size_kb` |
| 415 | `unsupported_media_type` | Body not sent as `application/json` |
| 421 | `invalid_host` | `Host` header not in `api.allowed_hosts` |
| 422 | `invalid_pdu` | The PDU could not be decoded |
| 422 | `invalid_configuration` | Reload rejected; the message lists the problems |
| 429 | `limit_reached` | Outbound queue full, or a SIM quota is exhausted |
| 429 | `rate_limited` | Too many requests from this address |
| 500 | `database_error` | The database is unavailable or returned an error |
| 500 | `backup_failed` | The backup could not be created |
| 500 | `internal_error` | Unexpected failure |
| 502 | `ussd_failed` | The modem or network did not complete the USSD request |
| 503 | `unavailable` | Sending is disabled, or the modem task is not running |
| 504 | `timeout` | The request took longer than `api.request_timeout_seconds` |

## Health and metrics

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/health` | Liveness. Always `200` while the process serves requests. |
| GET | `/ready` | Readiness: `200` when the database answers, `503` otherwise. Modem states are reported but do not affect readiness. |
| GET | `/metrics` | Prometheus metrics (path configurable with `metrics.path`; absent when `metrics.enabled = false`) |

```sh
curl -sS http://127.0.0.1:8080/health
```

```json
{"status": "ok", "version": "0.1.0", "uptime_seconds": 3600}
```

```sh
curl -sS http://127.0.0.1:8080/ready
```

```json
{"ready": true, "database": true, "modems": [{"id": "modem-1", "state": "READY"}]}
```

Metric names:

| Metric | Type | Labels |
| --- | --- | --- |
| `smsgw_messages_received_total` | counter | `modem`, `type` |
| `smsgw_messages_submitted_total` | counter | `modem` |
| `smsgw_message_parts_submitted_total` | counter | `modem` |
| `smsgw_messages_delivered_total` | counter | `modem` |
| `smsgw_message_failures_total` | counter | `modem`, `reason` |
| `smsgw_queue_depth` | gauge | |
| `smsgw_integration_attempts_total` | counter | `integration` |
| `smsgw_integration_failures_total` | counter | `integration` |
| `smsgw_modem_connected` | gauge | `modem` (1 when `READY`) |
| `smsgw_modem_signal_dbm` | gauge | `modem` |
| `smsgw_modem_reconnect_attempts_total` | counter | `modem` |
| `smsgw_database_up` | gauge | |
| `smsgw_process_uptime_seconds` | gauge | |
| `smsgw_api_requests_total` | counter | `method`, `route`, `status` |
| `smsgw_api_request_duration_seconds` | histogram | `route` |
| `smsgw_disk_free_bytes`, `smsgw_disk_total_bytes` | gauge | |
| `smsgw_events_total` | counter | `type` |
| `smsgw_calls_total` | counter | `modem`, `direction` |

Telephone numbers and message content are never used as labels.

## Status and statistics

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/v1/status` | Daemon, modems, database, queue, integrations, disk, configuration status |
| GET | `/api/v1/version` | Name, version, commit, build date, target, homepage |
| GET | `/api/v1/capabilities` | What the gateway and each modem can do |
| GET | `/api/v1/stats` | Message, queue, integration and call counters, table row counts, uptime |
| GET | `/api/v1/stats/activity?range=24h\|7d\|30d` | Inbound and outbound message counts per hour (`24h`) or day, UTC |
| GET | `/api/v1/ui/config` | Settings the dashboard needs (no secrets, no token required) |

`GET /api/v1/status`:

```json
{
  "success": true,
  "data": {
    "daemon": {
      "status": "running",
      "instance_id": "gateway-01",
      "instance_name": "THUGSred SMS Gateway",
      "version": "0.1.0",
      "commit": "unknown",
      "started_at": "2026-11-24T10:00:00.000Z",
      "uptime_seconds": 3600
    },
    "modems": [
      {
        "id": "modem-1",
        "state": "READY",
        "enabled": true,
        "manufacturer": "SIMCOM_Ltd",
        "model": "SIMCOM_SIM868",
        "operator": "Example Operator",
        "registration": "home",
        "signal_percent": 58,
        "rssi_dbm": -77,
        "sim_state": "ready",
        "sim_id": "default",
        "mobile_number": "+4500000000",
        "last_error": null,
        "hint": null
      }
    ],
    "database": {"healthy": true, "driver": "sqlite", "schema_version": 1, "size_bytes": 4096000},
    "queue": {"pending": 0},
    "integrations": [
      {"name": "discord-sms", "type": "discord", "enabled": true, "healthy": true, "dead": 0, "pending": 0}
    ],
    "disk": {"free_bytes": 20000000000, "total_bytes": 30000000000},
    "config": {
      "valid": true,
      "loaded_at": "2026-11-24T10:00:00.000Z",
      "last_reload_at": null,
      "last_reload_error": null,
      "warnings": [],
      "restart_required": []
    }
  }
}
```

An integration is `healthy` when it is disabled, has never been attempted, or its most recent
attempt succeeded. `size_bytes` is `null` for MySQL/MariaDB.

`GET /api/v1/capabilities` returns `gateway` (feature switches derived from the
configuration, the `encodings`, `integration_types`, `events` and `modem_profiles` lists,
`mms_retrieval` as `"experimental"` or `"disabled"`, `mms_sending` as `"unsupported"`) and
`modems` (identification plus the `capabilities` object described in
[MODEMS.md](MODEMS.md#capability-detection)).

`GET /api/v1/stats` `data`:

```json
{
  "messages": {
    "received": 12, "outbound_total": 8, "sent": 7, "delivered": 6, "delivery_failed": 0,
    "failed": 1, "submission_unknown": 0, "delivery_success_rate_percent": 100.0
  },
  "queue": {"pending": 0},
  "integrations": {"pending": 0, "delivered": 15, "dead": 0},
  "calls": {"total": 2},
  "tables": {"messages": 20, "message_parts": 22, "...": 0},
  "uptime_seconds": 3600
}
```

`GET /api/v1/stats/activity` `data`:

```json
{"range": "24h", "bucket": "hour", "timezone": "UTC",
 "buckets": [{"bucket": "2026-11-24T10", "inbound": 2, "outbound": 1}]}
```

Every bucket of the range is present, including empty ones.

## Modem and SIM

Endpoints that take `?modem=<id>` default to the first configured modem; an unknown id gives
`404`.

| Method | Path | Admin | Purpose |
| --- | --- | --- | --- |
| GET | `/api/v1/modems` | | Full status of every modem |
| GET | `/api/v1/modem?modem=` | | Full status of one modem |
| GET | `/api/v1/modem/signal?modem=` | | Signal |
| GET | `/api/v1/modem/network?modem=` | | Registration, operator, technology |
| GET | `/api/v1/modem/storage?modem=` | | SMS storage usage |
| GET | `/api/v1/modem/health?modem=&limit=` | | Recent health samples, newest first |
| POST | `/api/v1/modem/reconnect?modem=` | yes | Drop and re-establish the connection |
| POST | `/api/v1/modem/at` | yes | Raw AT command (disabled by default) |
| POST | `/api/v1/modem/reset-pin-counter?modem=` | yes | Permit one new SIM PIN attempt |
| GET | `/api/v1/sim?modem=` | | SIM state, detected and configured data for one modem |
| GET | `/api/v1/sims` | | The same for every modem |

`GET /api/v1/modem` `data`:

```json
{
  "id": "modem-1",
  "state": "READY",
  "state_since": "2026-11-24T10:00:12.000Z",
  "enabled": true,
  "device": "/dev/ttyUSB0",
  "baud_rate": 115200,
  "profile": "simcom-2g",
  "manufacturer": "SIMCOM_Ltd",
  "model": "SIMCOM_SIM868",
  "firmware": "<firmware revision>",
  "imei": "<IMEI>",
  "sim": {"state": "ready", "profile": "default", "iccid": "<ICCID>", "imsi": "<IMSI>", "msisdn": null, "smsc": "+4500000001"},
  "network": {"registration": "home", "registered": true, "operator": "Example Operator", "technology": "GSM"},
  "signal": {"raw": 18, "rssi_dbm": -77, "percent": 58},
  "storage": {"name": "SM", "used": 0, "total": 20},
  "capabilities": {
    "detected": true,
    "sms_modes": ["pdu", "text"],
    "character_sets": ["IRA", "GSM", "UCS2", "HEX"],
    "baud_rates": [9600, 19200, 38400, 57600, 115200],
    "sms_storages": ["SM", "ME"],
    "delivery_report_routing": "supported",
    "network_technologies": ["GSM", "GPRS"],
    "voice": "supported",
    "caller_id": "supported",
    "ussd": "supported",
    "gnss": "supported",
    "mms": "unverified",
    "bluetooth": "supported",
    "audio": {"modem_interfaces": ["analog (headset jack / MIC and SPK pins)"], "host_capture_devices": [], "call_recording": "unsupported"}
  },
  "gnss": null,
  "gnss_last_fix_at": null,
  "last_error": null,
  "hint": null,
  "reconnect_attempts": 0,
  "connected_at": "2026-11-24T10:00:10.000Z",
  "last_seen_at": "2026-11-24T11:00:00.000Z"
}
```

Values: `state` is one of the states in
[ARCHITECTURE.md](ARCHITECTURE.md#modem-state-machine); `sim.state` is `ready`,
`pin_required`, `puk_required`, `missing`, `error` or `unknown`; `network.registration` is
`home`, `roaming`, `searching`, `denied`, `not_registered`, `sms_only`, `emergency_only` or
`unknown`. `signal.percent` is the raw `+CSQ` value scaled to 0–100.

Smaller views:

```json
{"modem": "modem-1", "state": "READY", "signal": {"raw": 18, "rssi_dbm": -77, "percent": 58}, "updated_at": "2026-11-24T11:00:00.000Z"}
```

```json
{"modem": "modem-1", "state": "READY", "network": {"registration": "home", "registered": true, "operator": "Example Operator", "technology": "GSM"}, "technologies": ["GSM", "GPRS"]}
```

```json
{"modem": "modem-1", "state": "READY", "storage": {"name": "SM", "used": 0, "total": 20}, "available_storages": ["SM", "ME"]}
```

Health samples have the fields `state`, `rssi_dbm`, `signal_percent`, `registration`,
`operator`, `network_type`, `created_at`.

`POST /api/v1/modem/reconnect` → `{"modem": "modem-1", "reconnecting": true}`.

`POST /api/v1/modem/reset-pin-counter` → `{"modem": "modem-1", "pin_attempts": 0, "note": "..."}`.

### Raw AT

Requires `security.allow_raw_at_commands = true` (otherwise `403`).

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/modem/at \
  -H 'Content-Type: application/json' \
  -d '{"command": "AT+CSQ", "modem": "modem-1"}'
```

```json
{"success": true, "data": {"modem": "modem-1", "command": "AT+CSQ", "ok": true, "response": ["+CSQ: 18,0"]}}
```

A command the modem rejects still returns `200`, with `"ok": false` and an `error` text. The
command must start with `AT`, be at most 200 characters and contain no control characters.
Blocked prefixes are listed in [SECURITY.md](SECURITY.md#raw-at-and-raw-pdu-gates). Every
command is written to the audit trail. An `ATD...` command is additionally recorded as an
outgoing call.

### SIM

`GET /api/v1/sim` `data`:

```json
{
  "modem": "modem-1",
  "state": "ready",
  "ready": true,
  "detected": {"iccid": "<ICCID>", "imsi": "<IMSI>", "msisdn": null, "smsc": "+4500000001", "operator": "Example Operator"},
  "configured": {
    "id": "default", "name": "Primary SMS Gateway", "owner": "THUGSred", "description": "",
    "mobile_number": "+4500000000", "country_code": "", "carrier": "",
    "pin_enabled": false, "smsc": "",
    "network": {"selection": "automatic", "operator_code": "", "roaming_enabled": false, "apn": ""},
    "identity": {"manufacturer": "", "model": "", "asset_id": "", "location": "", "notes": ""},
    "limits": {"monthly_sms_limit": 0, "daily_sms_limit": 0}
  },
  "note": "mobile_number is descriptive gateway metadata; the network determines the number recipients see"
}
```

The PIN and the APN password are never returned.

## Sending

| Method | Path | Admin | Purpose |
| --- | --- | --- | --- |
| POST | `/api/v1/sms/send` | only with `override_allowlist` | Queue a message |
| POST | `/api/v1/sms/schedule` | only with `override_allowlist` | The same, but `scheduled_at` is required |
| POST | `/api/v1/sms/estimate` | | Encoding and part count for a text, without queueing |
| POST | `/api/v1/sms/send-pdu` | yes | Submit a caller-built SMS-SUBMIT PDU (disabled by default) |

### Request fields

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `to` | string | required | Destination: digits with an optional leading `+`, at least 3 and at most 20 digits. Spaces, `-`, `(`, `)`, `.` and `/` are removed; a leading `00` becomes `+`. |
| `message` | string | | Text. Exactly one of `message` and `binary` is required. |
| `binary` | string | | Hexadecimal 8-bit payload (needs `sms.allow_binary`) |
| `encoding` | string | `"auto"` | `auto`, `gsm7`, `ucs2`, `binary` (alias `8bit`) |
| `delivery_report` | boolean | `sms.request_delivery_reports` | Request a delivery report |
| `priority` | string or integer | `"normal"` | `high` (10), `normal` (0), `low` (-10), or an integer from -100 to 100. Higher is sent first. |
| `flash` | boolean | `false` | Class 0 message (needs `sms.allow_flash_sms`) |
| `message_class` | integer | | 0 to 3 |
| `validity_minutes` | integer | `sms.default_validity_minutes` | 1 to 635040; encoded as a relative validity period |
| `scheduled_at` | string | | RFC 3339 time, at most one year ahead. A time in the past sends immediately. |
| `modem` | string | | Send through this modem |
| `from` | string | | Send through the modem whose SIM profile has this `mobile_number`. Selects one of the gateway's own SIMs; it cannot set an arbitrary sender. |
| `destination_port`, `source_port` | integer | | Application ports (0 to 65535) |
| `protocol_identifier` | integer | `0` | A non-zero value needs `security.allow_raw_pdu_submission` |
| `correlation_id` | string | request id | At most 128 characters; copied to events |
| `override_allowlist` | boolean | `false` | Bypass `security.destination_allowlist`. Administrative; effective only with `security.allow_destination_overrides`. |

### Checks, in order

1. Sending enabled (`sms.enabled`, `sms.send_enabled`), else `503`.
2. `Idempotency-Key` well formed (`400`); a known key returns the original message.
3. `to` valid (`400`).
4. Destination not on the blocklist, and on the allowlist when one is set (`403`).
5. `scheduled_at` not more than a year ahead (`400`).
6. `modem` known and enabled, or `from` matching a configured SIM (`400`).
7. Payload, flash, binary, validity rules (`400` or `403`); encoding and segmentation, at
   most `sms.max_message_parts` parts (`400`).
8. Queue below `security.max_outbound_queue_size` (`429`).
9. For a message pinned to a modem: SIM quota not exhausted (`429`).

Rate limits, the per-destination interval, roaming and quotas for unpinned messages are
applied later, by the sender worker; they delay a queued message rather than reject the
request.

### Example

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/sms/send \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: order-1001-confirmation' \
  -d '{"to": "+4500000000", "message": "Hello from THUGSred SMS Gateway", "priority": "high"}'
```

`202 Accepted`:

```json
{
  "success": true,
  "message_id": "0b9c2f4e-5a1d-4c3b-9e8f-000000000001",
  "status": "queued",
  "encoding": "gsm7",
  "parts": 1,
  "duplicate": false,
  "modem": null
}
```

`status` is `queued` or `scheduled`. `modem` is the pinned modem, or `null`. Repeating the
request with the same key returns the same `message_id` with `"duplicate": true` and the
message's current status.

Scheduled:

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/sms/schedule \
  -H 'Content-Type: application/json' \
  -d '{"to": "+4500000000", "message": "Reminder", "scheduled_at": "2026-12-01T08:00:00Z", "from": "+4500000001"}'
```

### Estimate

`to` is not needed.

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/sms/estimate \
  -H 'Content-Type: application/json' -d '{"message": "Hello"}'
```

```json
{"success": true, "data": {"encoding": "gsm7", "parts": 1, "characters": 5, "units": 5, "units_per_part": 160, "units_remaining": 155, "exceeds_limit": false}}
```

`units` are septets for GSM 7-bit, UTF-16 code units for UCS-2 and octets for binary data.

### Raw PDU

Requires `security.allow_raw_pdu_submission = true`. The PDU must be an SMS-SUBMIT in modem
format (with the service centre prefix). The destination is checked against the allow and
block lists. The PDU is handed to the modem immediately, bypassing the queue, rate limits and
SIM quotas, and the call waits for the modem's answer. The submission itself runs
independently of the HTTP request: if the request times out first (`504`), the submission
still completes and its outcome is recorded on the message, which never stays in
`processing`. Read the result from the message's status before deciding to send again.

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/sms/send-pdu \
  -H 'Content-Type: application/json' \
  -d '{"pdu": "0001000A91540000000000001FC8329BFD0699E5EF36888AAC1EA7F23219346D4E41C730BD7C0FE701", "modem": "modem-1"}'
```

```json
{"success": true, "data": {"message_id": "0b9c2f4e-5a1d-4c3b-9e8f-000000000002", "status": "submitted", "error": null, "message_reference": 12}}
```

`status` is `submitted`, `failed` or `submission_unknown`. The message is stored with type
`raw_pdu`.

## Messages

| Method | Path | Admin | Purpose |
| --- | --- | --- | --- |
| GET | `/api/v1/messages` | | Search and list |
| GET | `/api/v1/messages/{id}` | | One message with decoded PDU, delivery reports, attachments, queue entry |
| GET | `/api/v1/messages/{id}/parts` | | The message's parts |
| GET | `/api/v1/messages/{id}/status` | | Status, history and delivery reports |
| DELETE | `/api/v1/messages/{id}` | yes | Delete a message and its parts, history, queue entry and delivery reports |

### Query parameters of the list

| Parameter | Meaning |
| --- | --- |
| `direction` | `inbound` or `outbound` |
| `type` | `sms`, `flash`, `binary`, `service`, `mms_notification`, `wap_push`, `raw_pdu`, `unknown` |
| `status` | Exact message status ([DATABASE.md](DATABASE.md#message-states)) |
| `sender`, `recipient` | Substring match, at most 200 characters |
| `modem` | Modem id |
| `search` | Substring of the text, sender or recipient, at most 200 characters |
| `from`, `to` | Created at or after / at or before |
| `order` | `asc` or `desc` (default) by creation time |
| `limit`, `offset` | Pagination |

```sh
curl -sS 'http://127.0.0.1:8080/api/v1/messages?direction=inbound&search=code&from=2026-11-01&limit=20'
```

### Message object

```json
{
  "id": "0b9c2f4e-5a1d-4c3b-9e8f-000000000003",
  "direction": "inbound",
  "message_type": "sms",
  "instance_id": "gateway-01",
  "modem_id": "modem-1",
  "sim_id": "default",
  "sender": "+4500000002",
  "recipient": "+4500000000",
  "original_address": "+4500000002",
  "normalized_number": "+4500000002",
  "body": "Hello",
  "raw_pdu": "06915400000010040A91540000002000006211422100004005C8329BFD06",
  "payload": null,
  "encoding": "gsm7",
  "message_class": null,
  "protocol_id": 0,
  "data_coding": 0,
  "smsc": "+4500000001",
  "concat_ref": null,
  "concat_total": null,
  "parts": 1,
  "src_port": null,
  "dst_port": null,
  "network_timestamp": "2026-11-24T11:00:00.000Z",
  "received_at": "2026-11-24T11:00:03.120Z",
  "submitted_at": null,
  "delivered_at": null,
  "status": "received",
  "delivery_status": null,
  "error": null,
  "retry_count": 0,
  "metadata": null,
  "integration_status": "delivered",
  "correlation_id": null,
  "idempotency_key": null,
  "modem_ref": null,
  "delivery_report": false,
  "created_at": "2026-11-24T11:00:03.120Z",
  "updated_at": "2026-11-24T11:00:03.120Z",
  "sender_name": "Example Person",
  "recipient_name": null
}
```

Notes: `payload` is uppercase hexadecimal for binary messages; `metadata` carries extras such
as `origin`, `validity_minutes`, `missing_parts`, `message_waiting`, `mms` and decoder
`warnings`; `sender_name` and `recipient_name` come from the address book;
`integration_status` is `pending`, `delivered` or `failed`. The API returns real numbers and
full text regardless of anonymisation settings, which apply to integrations and logs only.

`GET /api/v1/messages/{id}` adds `decoded_pdu` (see [PDU.md](PDU.md)), `delivery_reports`,
`attachments` and `queue`.

`GET /api/v1/messages/{id}/status` `data`:

```json
{
  "message_id": "0b9c2f4e-5a1d-4c3b-9e8f-000000000001",
  "direction": "outbound",
  "status": "delivered",
  "delivery_status": "delivered",
  "error": null,
  "retry_count": 0,
  "submitted_at": "2026-11-24T11:05:01.000Z",
  "delivered_at": "2026-11-24T11:05:06.000Z",
  "integration_status": null,
  "history": [
    {"status": "submitted", "detail": null, "created_at": "2026-11-24T11:05:01.000Z"},
    {"status": "delivered", "detail": null, "created_at": "2026-11-24T11:05:06.000Z"}
  ],
  "delivery_reports": [
    {"id": "...", "part_id": "...", "modem_ref": 12, "recipient": "+4500000000", "status": 0,
     "category": "delivered", "description": "received by the recipient",
     "smsc_timestamp": "2026-11-24T11:05:05.000Z", "discharge_time": "2026-11-24T11:05:05.000Z",
     "received_at": "2026-11-24T11:05:06.000Z"}
  ]
}
```

Part objects have the fields `id`, `message_id`, `direction`, `modem_id`, `address`,
`concat_ref`, `concat_total`, `sequence`, `wide_ref`, `encoding`, `shifts`, `pdu`, `payload`,
`tpdu_length`, `status`, `modem_ref`, `error`, `submitted_at`, `delivered_at`, `created_at`.

`DELETE` returns `{"deleted": true, "message_id": "..."}`. A message that is being
transmitted (`processing`) cannot be deleted (`409`). The events that reference the message
(by correlation id) and their integration delivery records are deleted with it; attachments
are not. See [DATABASE.md](DATABASE.md#retention).

## Queue

| Method | Path | Admin | Purpose |
| --- | --- | --- | --- |
| GET | `/api/v1/queue?status=` | | Queue entries |
| GET | `/api/v1/queue/{id}` | | One entry with its parts (`id` is the message id) |
| POST | `/api/v1/queue/{id}/retry` | yes | Queue a failed, expired, cancelled, uncertain or retrying message again |
| POST | `/api/v1/queue/{id}/cancel` | | Cancel a message that has not been handed to a modem |

Without `status` the list shows entries in the states `queued`, `scheduled`, `processing`,
`retrying` and `submission_unknown`. `status=all` shows everything; any other value filters
by that state.

Queue entry:

```json
{
  "message_id": "0b9c2f4e-5a1d-4c3b-9e8f-000000000001",
  "status": "retrying",
  "priority": 0,
  "modem_id": null,
  "claimed_by": "modem-1",
  "not_before": null,
  "expires_at": "2026-11-25T11:05:00.000Z",
  "attempts": 1,
  "next_attempt_at": "2026-11-24T11:05:20.000Z",
  "last_error": "+CMS ERROR: 38",
  "created_at": "2026-11-24T11:05:00.000Z",
  "updated_at": "2026-11-24T11:05:10.000Z",
  "recipient": "+4500000000",
  "parts": 1,
  "encoding": "gsm7"
}
```

`GET /api/v1/queue/{id}` returns `{"queue": {...}, "parts": [...]}` (the queue object without
the last three fields).

- **Retry** is possible from `failed`, `expired`, `cancelled`, `submission_unknown`,
  `delivery_failed` and `retrying`; otherwise `409`. Attempts are reset and a new expiry is
  set. Parts that the network already accepted are not sent again, except after a delivery
  failure, where every part is sent again. Retrying a `submission_unknown` message may
  deliver it twice. Response: `{"message_id": "...", "status": "queued"}`.
- **Cancel** is possible from `queued`, `scheduled` and `retrying`; otherwise `409`.
  Response: `{"message_id": "...", "status": "cancelled"}`.

## PDU tools

Both endpoints are offline: nothing is sent to a modem. See [PDU.md](PDU.md) for the fields.

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/api/v1/pdu/encode` | Build SMS-SUBMIT PDUs |
| POST | `/api/v1/pdu/decode` | Decode a PDU |

Encode request fields: `to` (required), `message` or `binary` (exactly one), `encoding`,
`smsc`, `flash`, `message_class`, `delivery_report`, `validity_minutes`,
`protocol_identifier`, `destination_port`, `source_port`, `reference` (0 to 255, the
concatenation reference).

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/pdu/encode \
  -H 'Content-Type: application/json' \
  -d '{"to": "+4500000000", "message": "Hello from THUGSred SMS Gateway"}'
```

```json
{
  "success": true,
  "data": {
    "transmitted": false,
    "encoded": {
      "encoding": "gsm7",
      "data_coding_scheme": 0,
      "reference": null,
      "parts": [
        {
          "sequence": 1,
          "pdu": "0001000A91540000000000001FC8329BFD0699E5EF36888AAC1EA7F23219346D4E41C730BD7C0FE701",
          "tpdu_length": 40,
          "text": "Hello from THUGSred SMS Gateway",
          "payload": "48656C6C6F2066726F6D20544855475372656420534D532047617465776179"
        }
      ]
    }
  }
}
```

Decode request fields: `pdu` (required), `direction` (`auto` default, `deliver` or `mt`,
`submit` or `mo`), `smsc` (boolean, default `true`: the PDU starts with a service centre
address; ignored for `auto`).

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/pdu/decode \
  -H 'Content-Type: application/json' \
  -d '{"pdu": "06915400000010040A91540000002000006211422100004005C8329BFD06"}'
```

The response is `{"decoded": {...}, "mms_notification": null}`; `decoded` is shown in
[PDU.md](PDU.md#decoding), and `mms_notification` is filled when the PDU is a WAP Push that
carries an MMS notification. An undecodable PDU gives `422 invalid_pdu`.

## Calls, recordings and attachments

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/v1/calls` | Call log |
| GET | `/api/v1/calls/{id}` | One call |
| GET | `/api/v1/recordings` | Recording list |
| GET | `/api/v1/recordings/{id}/download` | Download a recording (`audio/wav`) |
| GET | `/api/v1/attachments/{id}/download` | Download a stored attachment (`application/octet-stream`) |

Call object:

```json
{
  "id": "0b9c2f4e-5a1d-4c3b-9e8f-000000000010",
  "modem_id": "modem-1",
  "direction": "incoming",
  "number": "+4500000002",
  "state": "missed",
  "answered": 0,
  "missed": 1,
  "started_at": "2026-11-24T11:10:00.000Z",
  "answered_at": null,
  "ended_at": "2026-11-24T11:10:12.000Z",
  "duration_seconds": null,
  "termination_reason": "ring_timeout",
  "recording_status": "disabled",
  "recording_id": null
}
```

Recording object: `id`, `call_id`, `format`, `duration_seconds`, `size_bytes`, `created_at`.

Downloads are sent with `Content-Disposition: attachment`. A file is served only when it
lies inside `paths.recordings` or `paths.attachments` respectively (`403` otherwise, `404`
when the file is gone), and each download is written to the audit trail. Attachment ids are
listed in the `attachments` array of `GET /api/v1/messages/{id}`
(`id`, `filename`, `content_type`, `size_bytes`, `sha256`, `created_at`).

## Integrations and deliveries

| Method | Path | Admin | Purpose |
| --- | --- | --- | --- |
| GET | `/api/v1/integrations` | | Integrations with statistics, routing rules, automation rules |
| GET | `/api/v1/integrations/{name}/status` | | One integration with its 20 most recent deliveries |
| POST | `/api/v1/integrations/{name}/test` | yes | Deliver a synthetic `integration.test` event now |
| GET | `/api/v1/deliveries?integration=&status=` | | Delivery list |
| POST | `/api/v1/deliveries/{id}/replay` | yes | Queue a `dead` or `retrying` delivery again |
| GET | `/api/v1/dead-letters` | | Dead letters (integration deliveries and failed SMS) |

`GET /api/v1/integrations` `data`:

```json
{
  "enabled": true,
  "integrations": [
    {
      "name": "discord-sms", "type": "discord", "enabled": true,
      "events": ["sms.received"], "method": null,
      "stats": {
        "delivered": 15, "pending": 0, "retrying": 0, "dead": 0,
        "last_attempt": {"success": 1, "status_code": 204, "error": null, "duration_ms": 180, "created_at": "2026-11-24T11:00:04.000Z"},
        "last_success_at": "2026-11-24T11:00:04.000Z"
      }
    }
  ],
  "routing_rules": [{"name": "infrastructure-alerts", "enabled": true, "events": ["sms.received"], "destinations": ["discord-sms"], "stop_processing": false}],
  "automation_rules": [{"name": "status-reply", "enabled": false, "action": "reply"}]
}
```

No URLs, tokens or other secrets are returned.

Test delivery:

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/integrations/discord-sms/test
```

```json
{"success": true, "data": {"integration": "discord-sms", "enabled": true,
  "outcome": {"success": true, "status_code": 204, "error": null, "retryable": false, "duration_ms": 180}}}
```

The test bypasses the queue, routing and filters, and also works for a disabled integration.
A failed delivery still returns `200`; look at `outcome.success`.

Delivery object: `id`, `event_id`, `event_type`, `integration`, `status` (`pending`,
`processing`, `retrying`, `delivered`, `dead`), `attempts`, `next_attempt_at`, `last_error`,
`created_at`, `updated_at`, `delivered_at`.

Replay → `{"delivery_id": "...", "status": "pending"}`; `409` when the delivery is not `dead`
or `retrying`.

Dead letter object: `id`, `kind` (`integration` or `sms`), `reference_id` (delivery id or
message id), `integration`, `event_id`, `reason`, `created_at`.

## Events and the live stream

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/v1/events?type=&since=` | Stored events, newest first |
| GET | `/api/v1/events/stream?type=` | Live events as Server-Sent Events |

`type` filters by exact event type in the list and by **prefix** in the stream (`type=sms.`
selects all SMS events). Event objects:

```json
{
  "id": "0b9c2f4e-5a1d-4c3b-9e8f-000000000020",
  "type": "sms.received",
  "instance_id": "gateway-01",
  "correlation_id": "0b9c2f4e-5a1d-4c3b-9e8f-000000000003",
  "timestamp": "2026-11-24T11:00:03.150Z",
  "data": {"message": {"id": "...", "sender": "+4500000002", "body": "Hello", "...": "..."}}
}
```

Event types and their `data` are listed in [INTEGRATIONS.md](INTEGRATIONS.md#events). Events
contain real numbers and message text; treat access to these endpoints like access to the
messages themselves.

### Server-Sent Events

```sh
curl -sS -N 'http://127.0.0.1:8080/api/v1/events/stream?type=sms.'
```

```text
event: sms.received
id: 0b9c2f4e-5a1d-4c3b-9e8f-000000000020
data: {"id":"0b9c2f4e-5a1d-4c3b-9e8f-000000000020","type":"sms.received","instance_id":"gateway-01","correlation_id":"...","timestamp":"2026-11-24T11:00:03.150Z","data":{"message":{"...":"..."}}}

```

- The SSE `event` field is the event type, `id` the event id, `data` the event object.
- A keep-alive comment is sent every 15 seconds.
- A subscriber that falls behind receives an event named `stream.lagged` with data `{}`;
  events were skipped, so re-read `GET /api/v1/events?since=...` to catch up.
- The stream is live only: it does not replay past events and does not honour
  `Last-Event-ID`.
- With bearer authentication the token goes in the `Authorization` header. The browser
  `EventSource` API cannot set headers; use `fetch` with a streamed body instead, as the
  dashboard does.
- WebSocket is not offered.

## History export

`GET /api/v1/history/export` accepts the same filters as `GET /api/v1/messages` plus
`format=json|csv` (default `json`). Without `limit`, up to 10 000 rows are exported; `limit`
may be raised to 50 000. The export is written to the audit trail.

```sh
curl -sS -OJ 'http://127.0.0.1:8080/api/v1/history/export?format=csv&direction=inbound&from=2026-11-01&to=2026-11-30'
```

The response is a download named `smsgateway-messages-<timestamp>.csv` or `.json`.

**CSV:** columns `id, direction, type, status, sender, recipient, body, encoding, parts,
modem, network_timestamp, received_at, submitted_at, delivered_at, created_at, error`; every
field is quoted, quotes are doubled, lines end with CRLF. **Formula-injection protection:** a
field that starts with `=`, `+`, `-`, `@`, a tab or a carriage return is prefixed with a
single quote (`'`) so that spreadsheet programs do not evaluate SMS content as a formula.
This also affects telephone numbers, which start with `+`: `+4500000000` is exported as
`'+4500000000`.

**JSON:** `{"exported_at": "...", "total_matching": 120, "count": 120, "messages": [...]}`
with full message objects (without the address book names).

## USSD and GNSS

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/api/v1/ussd` | Run a USSD code and wait for the answer |
| GET | `/api/v1/ussd` | USSD history |
| GET | `/api/v1/gnss?limit=` | GNSS status, current position, optional history |

USSD request: exactly one of `command` (a name from `[ussd.commands]`) and `code` (a literal
code; allowed only when it equals a configured code or `ussd.allow_arbitrary_codes` is true),
plus optional `modem`.

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/ussd \
  -H 'Content-Type: application/json' -d '{"command": "balance"}'
```

```json
{"success": true, "data": {"modem": "modem-1", "code": "*101#", "response": "<text from the network>"}}
```

Failures: `403` (USSD disabled, or code not allowed), `404` (unknown command name), `400`
(invalid code), `502 ussd_failed` (modem not ready, another request in progress, rejected, or
no answer within `ussd.timeout_seconds`). Interactive sessions are not supported: if the
network expects a reply the session is closed after the first answer. The call can take as
long as the USSD timeout; with the defaults that equals `api.request_timeout_seconds`, so
consider raising the latter if you see `504` responses.

History objects: `id`, `modem_id`, `code`, `response`, `status` (`pending` while waiting, then
`completed`, `failed` or `timeout`), `created_at`, `completed_at`.

`GET /api/v1/gnss` `data`:

```json
{
  "enabled": true,
  "modem": "modem-1",
  "capability": "supported",
  "position": {"fix": false, "latitude": null, "longitude": null, "altitude_m": null, "speed_kmh": null, "heading_deg": null, "satellites": 0, "utc_time": null},
  "last_fix_at": null,
  "history": null
}
```

`history` is an array (fields `modem_id`, `latitude`, `longitude`, `altitude`, `speed_kmh`,
`heading`, `satellites`, `fix_time`, `created_at`) only when `gnss.store_location_history`
is true.

## Administration

| Method | Path | Admin | Purpose |
| --- | --- | --- | --- |
| GET | `/api/v1/config` | yes | Effective configuration with secrets redacted |
| GET | `/api/v1/config/status` | | Validation and reload status, configuration file paths |
| POST | `/api/v1/config/reload` | yes | Reload the configuration from disk |
| GET | `/api/v1/audit` | yes | Audit trail |
| GET | `/api/v1/diagnostics` | yes | Redacted diagnostic report |
| POST | `/api/v1/database/backup` | yes | Create and verify a database backup (SQLite) |
| GET | `/api/v1/addressbook` | | Contacts and a summary of the anonymous list |

Reload:

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/config/reload
```

```json
{"success": true, "data": {"reloaded": true, "restart_required": []}}
```

An invalid configuration gives `422 invalid_configuration`; the previous configuration stays
active.

`GET /api/v1/config/status` `data`: `{"status": {<configuration status as in /status>},
"files": {"main": "...", "sim": "...", "integrations": "..."}}`.

Audit entries: `id`, `actor` (client address, or `system`), `action`, `target`, `detail`,
`request_id`, `created_at`. Actions: `sms.submit`, `sms.raw_pdu`, `message.delete`,
`queue.retry`, `queue.cancel`, `modem.reconnect`, `modem.raw_at`, `sim.reset_pin_counter`,
`ussd.request`, `integration.test`, `integration.replay`, `recording.download`,
`attachment.download`, `history.export`, `diagnostics.export`, `database.backup`,
`config.reload`, `config.reload_failed`.

`GET /api/v1/diagnostics` returns version, system, the redacted configuration, the
configuration status, modem status (IMEI, ICCID, IMSI and MSISDN reduced to their last four
characters), database verification and table counts, statistics, disk usage, and the type and
time of the 50 most recent events (without their content). It is meant to be attached to bug
reports; review it before sharing.

Backup → `{"path": "/opt/THUGSred-smsGateway/backups/messages-20261124T110000Z.db", "verified": true}`;
`500 backup_failed` on MySQL/MariaDB.

`GET /api/v1/addressbook` `data`:

```json
{"contacts": [{"name": "Example Person", "number": "+4500000000"}],
 "anonymous": {"count": 1, "label": "Anonymous", "redact_body": false}}
```

The numbers on the anonymous list are not returned, only their count.

## OpenAPI document

`GET /api/v1/openapi.json` serves an OpenAPI 3.0.3 description of the API. It lists paths,
query and path parameters, example request bodies for the main operations, the error schema
and the bearer security scheme; administrative operations carry `"x-admin": true`. It does
not describe response bodies in detail, it does not list `/api/v1/ui/config` or itself, and
it lists the metrics endpoint under the fixed path `/metrics`; this document covers those. The dashboard's API
Documentation page renders it.

```sh
curl -sS http://127.0.0.1:8080/api/v1/openapi.json
```

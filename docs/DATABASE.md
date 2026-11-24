# Database

- [Engines](#engines)
- [Schema overview](#schema-overview)
- [Message states](#message-states)
- [Migrations](#migrations)
- [Deduplication fingerprints](#deduplication-fingerprints)
- [Retention](#retention)
- [SQLite specifics](#sqlite-specifics)
- [MySQL and MariaDB specifics](#mysql-and-mariadb-specifics)
- [Sizing notes](#sizing-notes)

## Engines

| | SQLite | MySQL / MariaDB |
| --- | --- | --- |
| Select with | `database.driver = "sqlite"` (default) | `database.driver = "mysql"` |
| Location | `database.sqlite_path` | `database.mysql_url` |
| Setup | None; the file is created at first start | Create a database and user first |
| Tested with | The complete automated test suite | The same suite, run against MariaDB 10.11.11, MariaDB 11.4 and MySQL 8.4 |
| Built-in backup and restore | Yes | No; use `mysqldump` |
| Integrity check (`db verify`) | `PRAGMA integrity_check` | Connection and schema version only |
| Size reported in status | Yes | No (`null`) |

Both engines are served by one code path through the sqlx `Any` driver. The schema uses only
text, integer, real and blob columns, so behaviour is the same on both. Timestamps are stored
as UTC text in RFC 3339 format with millisecond precision (`2026-11-24T11:00:03.150Z`), which
sorts chronologically as text.

The gateway is designed for **one daemon per database**. Queue claims and delivery claims
are guarded against concurrent workers inside one process; running two gateways against the
same database is not a supported configuration.

Changing any `[database]` setting needs a restart.

## Schema overview

| Table | Purpose | Key columns and indexes |
| --- | --- | --- |
| `messages` | One row per logical message, inbound or outbound | Primary key `id` (UUID). Unique indexes on `idempotency_key` and `fingerprint`. Indexes on `created_at`, `(direction, created_at)`, `sender`, `recipient`, `normalized_number`, `status`, `correlation_id`, `(modem_id, modem_ref)`. |
| `message_parts` | One row per SMS segment: received segments (also while waiting for the rest of a multipart message) and segments to send | Primary key `id`. Unique index on `fingerprint`. Indexes on `(message_id, sequence)`, `(direction, status, address, concat_ref)`, `(modem_id, modem_ref, status)`. Holds the PDU, the payload, the TPDU length, the modem's message reference and timestamps. |
| `message_status_history` | Every state change of a message, with a detail text | Index on `(message_id, id)` |
| `outbound_queue` | Queue entry of an outbound message: state, priority, pinned modem, schedule, expiry, attempts, next attempt | Primary key `message_id`. Index on `(status, priority, created_at)`. |
| `delivery_reports` | Every received status report, correlated or not | Unique index on `fingerprint`; index on `message_id` |
| `events` | Every emitted event with its JSON data | Indexes on `created_at`, `(event_type, created_at)` |
| `integration_events` | One delivery per event and destination integration (the outbox) | Indexes on `(status, next_attempt_at)`, `(integration, created_at)`, `event_id` |
| `integration_attempts` | One row per delivery attempt: success, status code, error, duration | Indexes on `delivery_id`, `created_at` |
| `dead_letter_queue` | Deliveries and SMS that failed for good | Index on `created_at` |
| `calls` | Call log | Index on `started_at` |
| `recordings` | Call recordings: file path, format, duration, size | Index on `created_at` |
| `attachments` | Downloaded MMS content: file path, name, size, SHA-256 | Index on `message_id` |
| `modem_health` | Periodic health samples: state, signal, registration, operator | Index on `(modem_id, created_at)` |
| `sim_information` | Last detected SIM data per modem (ICCID, IMSI, MSISDN, SMSC) | Primary key `modem_id` |
| `ussd_sessions` | USSD requests and answers | Index on `created_at` |
| `gnss_fixes` | Position history (only with `gnss.store_location_history`) | Index on `created_at` |
| `audit_events` | Audit trail of administrative and state-changing actions | Index on `created_at` |
| `automation_log` | Firings of automation rules (for cooldowns and hourly caps) | Index on `(rule_name, created_at)` |
| `kv_state` | Small persistent state: SIM PIN attempt counters, recording outcomes | Primary key `k` |
| `schema_migrations` | Applied migrations | Primary key `version` |

There are no foreign key constraints; related rows are removed by the application.

Important columns of `messages`:

| Column | Meaning |
| --- | --- |
| `direction`, `message_type`, `status` | See [Message states](#message-states) |
| `sender`, `recipient` | As shown to users |
| `original_address` | The address exactly as received or as given in the request |
| `normalized_number` | Normalised form of the other party's number, used for lookups |
| `body`, `payload`, `raw_pdu` | Text, binary payload, hexadecimal PDU (of the first part) |
| `encoding`, `message_class`, `protocol_id`, `data_coding` | Decoded PDU properties |
| `concat_ref`, `concat_total`, `parts` | Multipart information |
| `src_port`, `dst_port` | Application ports |
| `network_timestamp` | Service centre timestamp of a received message (UTC) |
| `received_at`, `submitted_at`, `delivered_at` | Lifecycle times |
| `delivery_status` | `pending`, `delivered` or `failed`, for outbound messages |
| `retry_count`, `error` | Submission attempts and the last error |
| `metadata` | JSON extras (origin, validity, MMS notification, warnings, missing parts) |
| `integration_status` | Latest integration outcome for the message |
| `correlation_id`, `idempotency_key`, `fingerprint` | Correlation, idempotent submission, deduplication |
| `modem_ref` | Message reference the modem assigned to the first part |
| `delivery_report` | Whether a delivery report was requested |

## Message states

### Outbound

```mermaid
stateDiagram-v2
    [*] --> queued: accepted
    [*] --> scheduled: accepted with a future scheduled_at
    scheduled --> processing: due and claimed
    queued --> processing: claimed by a sender worker
    processing --> queued: deferred without counting an attempt
    processing --> submitted: all parts accepted by the network
    processing --> retrying: rejected and attempts remain
    retrying --> processing: retry due
    processing --> failed: rejected and attempts used up
    processing --> submission_unknown: outcome of a part unknown
    queued --> expired: not sent before expiry
    scheduled --> expired: not sent before expiry
    retrying --> expired: not sent before expiry
    queued --> cancelled: cancelled
    scheduled --> cancelled: cancelled
    retrying --> cancelled: cancelled
    submitted --> delivered: delivery reports for all parts
    submitted --> delivery_failed: a delivery report reports failure
    failed --> queued: operator retry
    expired --> queued: operator retry
    cancelled --> queued: operator retry
    submission_unknown --> queued: operator retry
    delivery_failed --> queued: operator retry
```

| State | Meaning |
| --- | --- |
| `queued` | Waiting for a sender worker. Also after a deferral (modem not ready, roaming blocked, quota, per-destination interval); the reason is in the status history. |
| `scheduled` | Waiting for its `scheduled_at` time |
| `processing` | Claimed by a worker; parts are being handed to the modem |
| `retrying` | A submission was rejected; the next attempt is at `next_attempt_at` |
| `submitted` | Every part was accepted by the network. This is the final state when no delivery report was requested or none arrives. |
| `delivered` | Delivery reports confirmed every part |
| `delivery_failed` | A delivery report reported a temporary or permanent failure |
| `failed` | Rejected `sms.max_send_attempts` times; a dead letter of kind `sms` is recorded |
| `expired` | Not submitted within `sms.queue_expiry_seconds` |
| `cancelled` | Cancelled before it was handed to a modem |
| `submission_unknown` | A part was handed to the modem but no result arrived. The message may or may not have been sent. Never retried automatically; see [ARCHITECTURE.md](ARCHITECTURE.md#the-submission_unknown-rule). |

Messages submitted through the raw PDU endpoint are created as `processing` and end as
`submitted`, `failed` or `submission_unknown`; they have no queue entry.

### Inbound

| State | Meaning |
| --- | --- |
| `received` | Complete message |
| `incomplete` | A multipart message stored with parts missing (timeout or reference reuse); `metadata.missing_parts` lists them and the text shows `[...]` in their place |
| `malformed` | A PDU that could not be decoded, kept as a raw record with the error text; `message_type` is `unknown` |

### Part states

| State | Used for |
| --- | --- |
| `pending` | Received segment waiting for the rest of its message |
| `assembled` | Received segment that is part of a stored message |
| `queued` | Segment waiting to be sent |
| `sending` | Being handed to the modem right now (persisted first, so that a crash is recognisable) |
| `submitted` | Accepted by the network |
| `delivered`, `delivery_failed` | Outcome reported by a delivery report |
| `failed` | Rejected |
| `unknown` | Handed to the modem, outcome unknown |

### Delivery states (integrations)

`pending` → `processing` → `delivered`, `retrying` or `dead`. See
[INTEGRATIONS.md](INTEGRATIONS.md#retries-backoff-and-dead-letters).

## Migrations

Migrations are SQL files embedded in the binary, one set per engine
(`migrations/sqlite/`, `migrations/mysql/`). This release has one migration: `0001_initial`
(schema version 1).

- With `database.auto_migrate = true` (default) pending migrations are applied when the
  daemon starts.
- With `auto_migrate = false` the daemon refuses to start unless the schema is current. Apply
  migrations yourself:

  ```sh
  thugsred-smsgateway db migrate
  ```

  It prints either `applied migrations: [...]` or
  `database schema is up to date (version 1)`.
- Applied versions are recorded in `schema_migrations`. `GET /api/v1/status` and `db verify`
  report the schema version; `doctor` warns when it differs from the version the binary
  expects.
- Migrations only move forward. There are no down-migrations: to go back to an older release
  after a schema change, restore the backup taken before the upgrade
  ([BACKUP-RESTORE.md](BACKUP-RESTORE.md#rollback)).

Statements of a migration are executed one by one. A migration that fails part-way is not
recorded as applied; on MySQL/MariaDB, where schema changes are not transactional, restore
the backup before trying again.

## Deduplication fingerprints

Modems can present the same PDU more than once: an unsolicited notification followed by the
periodic poll of the storage, a message that could not be deleted from the modem, a
reconnect. The gateway recognises repeats by a fingerprint:

```text
fingerprint = SHA-256( "<modem id>:<complete PDU in uppercase hexadecimal>" )
```

The PDU contains the sender, the service centre timestamp, all headers and the content, so
two messages only share a fingerprint when they are the same PDU on the same modem. The
identical text sent twice by the same sender has different timestamps and is stored twice.

- For single-part messages the fingerprint is stored on the `messages` row, for multipart
  segments on the `message_parts` row, for status reports on the `delivery_reports` row. All
  three columns have a **unique index**.
- Before processing, the gateway checks whether the fingerprint is known and skips the PDU
  when it is (`sms.deduplication_enabled`, default true). A skipped duplicate counts as
  handled, so it is deleted from the modem.
- Two copies arriving at the same moment are caught by the unique index.
- `sms.deduplication_enabled = false` only switches off the check before processing. The
  unique indexes remain, so an identical PDU is still not stored a second time. The setting
  does not allow duplicates into the database.
- Fingerprints live as long as their rows. Once retention has removed a message, the same
  PDU would be accepted again; this matters only if you keep messages on the modem
  (`sms.delete_from_modem_after_persist = false`) **and** use a message retention period.

Outbound duplicates are a separate matter, handled by the `Idempotency-Key` header and the
unique index on `idempotency_key`.

## Retention

Housekeeping runs retention every `retention.cleanup_interval_seconds` (default hourly). A
value of 0 days keeps that category indefinitely.

| Setting | Default | Removes rows older than the period from |
| --- | --- | --- |
| `message_days` | 0 (keep) | `messages`, their `message_parts` and `message_status_history`, `delivery_reports`, finished `outbound_queue` entries. Messages still waiting in the queue (`queued`, `scheduled`, `processing`, `retrying`) are never removed. |
| `recording_days` | 30 | `recordings` rows and their files |
| `attachment_days` | 30 | `attachments` rows and their files |
| `integration_event_days` | 30 | `integration_attempts`; `integration_events` in the states `delivered` and `dead`; `dead_letter_queue`; `events` that have no remaining delivery; `modem_health`; `automation_log` |
| `audit_days` | 90 | `audit_events` |

Files are deleted only when they lie inside `paths.recordings` or `paths.attachments`.

Not covered by retention in this release — these tables grow until you clean them yourself:
`calls` (the call log; only its recordings are removed), `ussd_sessions`, `gnss_fixes`,
`sim_information` and `kv_state` (both small and bounded).

Privacy-relevant details:

- **Events keep message content.** The `events` table stores the full event data, including
  message text, numbers and (when stored) the raw PDU, for `integration_event_days`.
  Deleting a message through the API, `message_days`, and `sms.store_sent_messages = false`
  remove or erase the *message* rows only; the events that were emitted for the message
  remain readable through `GET /api/v1/events` until event retention removes them. Set
  `integration_event_days` with that in mind.
- `message_days = 0` means messages are kept forever by default. Set a period if you must
  limit how long personal data is stored.
- Monthly SIM quotas are counted from `message_parts`. A `message_days` shorter than a month
  removes rows the monthly count relies on and makes it undercount.
- Deleting a message does not delete its attachments; those follow `attachment_days`.
- Log files have their own retention (`logging.retention_days`).

## SQLite specifics

- Connections use `busy_timeout = 10000`, `foreign_keys = ON`, `synchronous = NORMAL` and,
  with `database.sqlite_wal = true`, `journal_mode = WAL`. In WAL mode two companion files
  exist next to the database while it is open: `messages.db-wal` and `messages.db-shm`.
- The file is created, with its parent directory, when it does not exist. It is created by
  whoever opens it first; make sure that is the service account (see
  [INSTALLATION.md](INSTALLATION.md#running-the-command-line-tool)).
- Online backup uses `VACUUM INTO`, which produces a consistent, compact copy while the
  daemon runs, followed by an integrity check of the copy.
- Do not place the database on a network filesystem.

## MySQL and MariaDB specifics

Create the database and user (placeholders):

```sql
CREATE DATABASE smsgateway CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE USER 'smsgateway'@'192.0.2.%' IDENTIFIED BY 'change-me';
GRANT ALL PRIVILEGES ON smsgateway.* TO 'smsgateway'@'192.0.2.%';
FLUSH PRIVILEGES;
```

```sh
# /etc/thugsred/smsgateway.env
SMSGW_MYSQL_URL=mysql://smsgateway:change-me@db.example.invalid:3306/smsgateway
```

```toml
[database]
driver = "mysql"
mysql_url = "${SMSGW_MYSQL_URL}"
```

- The user needs the rights to create tables and indexes (for migrations) and to read and
  write rows.
- Tables are created as InnoDB with `utf8mb4` / `utf8mb4_unicode_ci`. Identifier-like
  columns are `VARCHAR(191)`, long text `MEDIUMTEXT`, binary payloads `MEDIUMBLOB`, integers
  `BIGINT`, timestamps `VARCHAR(32)`.
- Characters in the password that are special in URLs must be percent-encoded in
  `mysql_url`.
- TLS to the database server is available through the URL's `ssl-mode` parameter as
  supported by sqlx; the gateway links rustls with the bundled web PKI roots. This has not
  been exercised by the test suite.
- `db backup`, `db restore`, `make backup`, `make restore` and
  `POST /api/v1/database/backup` do not work with this engine; see
  [BACKUP-RESTORE.md](BACKUP-RESTORE.md#mysql-and-mariadb).
- `db verify` checks connectivity and the schema version only. Use the server's own tools
  (`CHECK TABLE`, `mysqlcheck`) for integrity.
- The systemd watchdog reports liveness only while the database answers. If the database
  server is unreachable for longer than the watchdog period (120 seconds in the shipped
  unit), systemd restarts the gateway.

## Sizing notes

No measurements have been published for this release, so this section describes what drives
size rather than quoting numbers.

- Each SMS segment produces one `message_parts` row, and each message one `messages` row,
  one `outbound_queue` row (outbound) and a few `message_status_history` rows.
- With `sms.store_raw_pdu = true` (default) the hexadecimal PDU is stored with each part
  and, for the first part, with the message: roughly twice the size of the segment on the
  wire, per copy.
- Each message produces several events (`sms.queued`, `sms.sent`, `sms.delivered` for an
  outbound message), each of which stores the message data again, plus one
  `integration_events` row and at least one `integration_attempts` row per destination.
  With busy integrations the event tables can outgrow the message tables;
  `integration_event_days` bounds them.
- `modem_health` receives one row per modem per health check (every 30 seconds by default)
  and is pruned with `integration_event_days`.
- Recordings and attachments are files on disk, not database content.

To see where rows accumulate, use `GET /api/v1/stats` (`tables`), or
`GET /api/v1/status` for the SQLite file size. The gateway emits a `gateway.error` event when
the data filesystem has less than 200 MiB or less than 2 % free, and `doctor` warns below
500 MiB.

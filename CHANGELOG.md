# Changelog

All notable changes to THUGSred-smsGateway are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **SIM Toolkit attack detection (honeypot).** Inbound binary SMS directed at the SIM
  (S@T Browser / WIB pushes of the Simjacker/WIBattack class) are detected, decoded (TAR,
  SPI security level, suspected-attack flag) and stored as `sim_toolkit` messages with the
  raw PDU preserved. New events `sms.ota_toolkit` and `security.sim_attack`, a dashboard
  filter and badge, and the messages API expose them for remote honeypot collection. Detection
  is read-only; no SIM Toolkit messages are generated. See docs/HONEYPOT.md.

- **Contacts.** Contacts can be saved, changed and deleted from a new Contacts page and
  through `/api/v1/contacts`. They are stored in the database (migration `0002_contacts`),
  apply without a reload and complement `addressbook.conf`.
- **Send page.** Redesigned composer: contact search and quick pick, recipient chip, live
  segment meter, message preview, modem/provider switch, delivery progress.
- **Page addresses.** Dashboard pages use real paths (`/send`, `/contacts`, …) instead of
  `#/` fragments, so reload, back/forward and deep links work; `/send?to=` and `?message=`
  are supported and old fragment addresses are redirected. The API documentation page moved
  to `/docs`.

- **SMS providers.** Optional outbound routes through an SMS provider's API, configured in
  `providers.conf`. First provider type: `gatewayapi`. A request selects one with `provider`
  or `sender_name`, which lets a message show an allowed sender name instead of a SIM
  number. Includes sender allow-listing, per-provider rate limit and daily/monthly segment
  limits, retry handling that never repeats an uncertain submission, signed delivery status
  callbacks (`POST /api/v1/providers/{name}/callback`), `GET /api/v1/providers`, the CLI
  options `--provider` and `--sender-name`, and a route choice on the dashboard's Send page.

## [0.1.0] - 2026-11-24

Initial release.

### Added

- **Daemon.** `thugsred-smsgateway`, a single-binary SMS gateway daemon for serial GSM/LTE
  modems, running as the systemd service `thugsred-smsgateway.service` under its own
  unprivileged account, with readiness notification, watchdog support and a sandboxed unit.
- **Modem handling.** A serialised AT command engine; one supervisor per modem with
  automatic reconnect and exponential backoff; identification, profile selection and
  capability detection; health checks, signal and registration monitoring; detection of an
  unpowered module with a hint to hold PWRKEY.
- **Modem profiles.** Built-in profiles `generic`, `simcom-2g`, `simcom-lte` and `quectel`,
  plus custom profiles from a directory.
- **Multiple modems and SIMs.** `[[modems]]` and `[[sims]]` entries, SIM binding by `sim =`
  or ICCID, message routing by `modem` or `from`.
- **SIM handling.** PIN entry limited to a recorded single attempt, a PIN attempt counter
  with `modem reset-pin-counter`, no PUK entry, optional manual operator selection, roaming
  blocked by default.
- **Receiving.** SMS in PDU mode through notifications and a periodic storage poll;
  persist-before-delete; deduplication by PDU fingerprint; multipart reassembly with
  out-of-order parts, timeouts and reference reuse; classification of text, flash, binary,
  service and WAP Push messages; storage of undecodable PDUs as raw records.
- **Sending.** A durable outbound queue with priorities, scheduling, expiry, cancel and
  retry; automatic GSM 7-bit / UCS-2 selection and segmentation; flash, binary and
  port-addressed messages; validity periods; idempotent submission with `Idempotency-Key`;
  retries with backoff; the `submission_unknown` state for uncertain submissions, which are
  never resent automatically; queue recovery after a restart.
- **Delivery reports.** Decoding and correlation with sent message parts.
- **Spending protection.** Per-SIM daily and monthly quotas, per-modem and per-destination
  rate limits, destination allow and block lists, a queue size limit.
- **PDU codec and tools.** Encoder and decoder for SMS-SUBMIT, SMS-DELIVER and
  SMS-STATUS-REPORT; offline `pdu encode` / `pdu decode` commands and API endpoints.
  National language tables for decoding only: Turkish (single and locking shift), Spanish
  and Portuguese (single shift).
- **MMS.** Decoding of MMS notifications; experimental retrieval from the configured MMSC
  over the host's own network.
- **USSD.** Named commands and optional arbitrary codes.
- **Calls.** Call logging, optional auto-answer, optional recording through an ALSA capture
  device with honest status reporting.
- **GNSS.** Optional position polling for supported profiles; coordinates are never placed
  in events.
- **Storage.** SQLite (default) and MySQL/MariaDB through one code path; embedded
  migrations; retention settings; verified online backup and restore for SQLite.
- **REST API.** Sending, messages, queue, modem and SIM status, calls, recordings,
  attachments, integrations, deliveries, events, history export (JSON and CSV with
  formula-injection protection), USSD, GNSS, configuration, audit trail, diagnostics; an
  OpenAPI document; Prometheus metrics; a live event stream over Server-Sent Events.
- **Access control.** Network allowlist, trusted proxies, Host header check, cross-site
  request rejection, optional bearer tokens, a separate administrative layer, rate limiting,
  request size and time limits, an audit trail.
- **Web dashboard.** Embedded, without external assets: Overview, Send SMS, Inbox, Outbox,
  Message History, Message Queue, PDU Tools, Calls, Integrations, Modem, System and API
  Documentation pages.
- **Integrations.** Webhook (POST, PUT, GET, custom body templates, HMAC-SHA256 signatures),
  Discord, Slack, Microsoft Teams, Telegram, ntfy, Gotify and syslog; a durable outbox with
  retries, backoff, per-integration rate limits, dead letters and replay; filters; routing
  rules; templates with per-destination escaping and mention neutralisation.
- **Privacy.** Per-integration anonymisation and message body redaction, a global
  always-anonymous list, an address book, masked numbers and no message content in logs by
  default.
- **Automation.** Opt-in reply and forward rules with loop prevention, cooldowns and hourly
  caps.
- **Configuration.** TOML files with defaults for every setting, environment and
  command-line overrides, `${ENV}` and `${file:...}` secret references with an
  `smsgateway.env` fallback, validation with `check-config`, redacted output, and atomic
  reload on SIGHUP that keeps the last valid configuration.
- **Command-line tool.** `run`, `version`, `check-config`, `status`, `doctor`,
  `capabilities`, `sms`, `pdu`, `modem`, `db`, `integrations`, `ussd`, `reload`,
  `diagnostics` and `simulator`.
- **Simulator.** A software modem used by the test suite, by `run --simulate-modem` and as
  a pseudo-terminal device for development.
- **Tooling.** A Makefile for building, testing, installing, upgrading and packaging;
  installer and uninstaller scripts that never overwrite configuration or data; an optional
  Debian package; release archives with SHA-256 checksums and a CycloneDX SBOM; a udev rule
  for ModemManager; an nginx reverse proxy example; secret scanning and a pre-commit hook;
  CI with tests on SQLite, MariaDB and MySQL and builds for x86_64 and ARM64.

### Known limitations

- Verified on real hardware with one module only (SIMCom SIM868 on a Waveshare
  GSM/GPRS/GNSS HAT, attached by USB to an x86_64 Debian host): connection, identification,
  profile and capability detection, SIM ready detection, registration, operator, signal and
  storage queries, and the powered-off hint; and, over a real mobile network, receiving a
  single-part GSM 7-bit SMS including national characters (å ø æ) with persist-then-delete
  from SIM storage, sending a single-part GSM 7-bit SMS from the dashboard, and reception
  and correlation of the delivery report for that message. **UCS-2, multipart, binary and
  flash messages, USSD, calls, recording, GNSS position fixes, MMS, multi-modem operation
  with several physical modules, Quectel and SIMCom LTE modules have not been verified on
  real hardware.**
- Builds for `aarch64`, but has not yet been run on a Raspberry Pi by the maintainers.
- The HTTP interface is plain HTTP; TLS requires a reverse proxy. Authentication is off by
  default: do not expose the gateway to untrusted networks.
- Not implemented: MMS sending; MMS retrieval through the modem's own data stack; MQTT
  integration; SMS text mode; WebSocket streaming; integration batching; national language
  tables for encoding; the Portuguese locking shift table.

[Unreleased]: https://github.com/kawaiipantsu/THUGSred-smsGateway/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/kawaiipantsu/THUGSred-smsGateway/releases/tag/v0.1.0

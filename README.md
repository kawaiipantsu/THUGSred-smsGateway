# THUGSred-smsGateway

**A self-hosted SMS gateway daemon for GSM modems, written in Rust.**

THUGSred-smsGateway talks to one or more serial GSM/LTE modems with AT commands, stores every
message in a local database, and exposes sending, history, modem status and event forwarding
through a REST API, a command-line tool and an embedded web dashboard.

- Project website: <https://thugs.red>
- Repository: <https://github.com/kawaiipantsu/THUGSred-smsGateway>
- Licence: MIT
- Binary: `thugsred-smsgateway` · Service: `thugsred-smsgateway.service`

> **Status: 0.1.0, first release.** The automated test suite runs against a built-in software
> modem. Real-hardware verification so far covers one module and, for SMS traffic, only
> single-part GSM 7-bit messages. Read [Project status](#project-status) before relying on it.

## Table of contents

- [Overview](#overview)
- [Project status](#project-status)
- [Features](#features)
- [Architecture](#architecture)
- [Hardware requirements](#hardware-requirements)
- [Supported modem families](#supported-modem-families)
- [Raspberry Pi 4 and ARM64](#raspberry-pi-4-and-arm64)
- [Quick start](#quick-start)
- [Powering the module on (PWRKEY)](#powering-the-module-on-pwrkey)
- [Configuration overview](#configuration-overview)
- [Multi-modem and multi-SIM setup](#multi-modem-and-multi-sim-setup)
- [Command-line tool](#command-line-tool)
- [Makefile reference](#makefile-reference)
- [REST API examples](#rest-api-examples)
- [Web dashboard](#web-dashboard)
- [Database: SQLite](#database-sqlite)
- [Database: MySQL or MariaDB](#database-mysql-or-mariadb)
- [Integrations](#integrations)
- [PDU tools](#pdu-tools)
- [Voice calls and call recording](#voice-calls-and-call-recording)
- [Running behind a reverse proxy](#running-behind-a-reverse-proxy)
- [Security considerations](#security-considerations)
- [Troubleshooting](#troubleshooting)
- [Backup and restore](#backup-and-restore)
- [Upgrading](#upgrading)
- [Documentation index](#documentation-index)
- [Contributing](#contributing)
- [Licence](#licence)

## Overview

The gateway is a single binary that runs as a systemd service under its own unprivileged
account. It:

- opens each configured serial device, identifies the modem, selects a modem profile and
  detects its capabilities;
- receives SMS in PDU mode, reassembles multipart messages, removes duplicates and stores
  everything in SQLite or MySQL/MariaDB;
- sends SMS from a durable outbound queue with priorities, scheduling, retries, rate limits
  and per-SIM quotas;
- correlates delivery reports with sent messages;
- turns everything that happens into events and forwards them to webhooks and chat services;
- serves a REST API, an OpenAPI document, Prometheus metrics, a live event stream and a web
  dashboard on one plain-HTTP port.

Nothing is sent to any outside service unless you enable an integration.

## Project status

| Area | What has been done |
| --- | --- |
| Development platform | Debian 13 on x86_64 |
| Automated tests | Run entirely against the built-in software modem simulator and SQLite. The same suite was also run against MariaDB 10.11.11, MariaDB 11.4 and MySQL 8.4. |
| Real hardware | One module: a Waveshare GSM/GPRS/GNSS HAT (SIMCom SIM868, firmware `1418B05SIM868M32_BT`) attached through its USB port (CP2102 USB-UART bridge) to an x86_64 Debian host |
| Verified on that module | Serial connection and AT handshake, identification, profile auto-detection (`simcom-2g`), capability detection, SIM ready detection, network registration, operator and signal reading, SMS storage query, and the "module powered off" hint. The gateway also ran against it as the installed systemd service with a MariaDB 10.11 database behind an nginx reverse proxy. With GNSS enabled the receiver power-on command was accepted and the capability reported as `supported`, but no position fix was obtained. |
| Verified on that module over a real mobile network | Receiving a single-part GSM 7-bit SMS, including national characters (å ø æ), with persist-then-delete from SIM storage; sending a single-part GSM 7-bit SMS from the dashboard; reception and correlation of the delivery report for that message |
| Dashboard | Exercised in headless Chromium against the simulator (not Firefox; not with calls or recordings), and used to send the one real message mentioned above |
| Not verified on real hardware | UCS-2, multipart, binary and flash messages, USSD, calls and auto-answer, call recording, GNSS position fixes, MMS, more than one physical modem, any Quectel or SIMCom LTE module, running on a Raspberry Pi or any ARM64 machine, Raspberry Pi GPIO UART wiring |

Treat everything in the last row of that table as simulator-tested only until you have
tried it on your own hardware. Compatibility reports are welcome; see
[CONTRIBUTING.md](CONTRIBUTING.md).

## Features

Legend: ✅ implemented and covered by automated tests · 🧪 experimental ·
🔌 hardware-dependent, not verified on real hardware · ❌ not supported

### Messaging

| Feature | Status | Notes |
| --- | --- | --- |
| Receive SMS (PDU mode) | ✅ | Single-part GSM 7-bit reception verified on a SIM868 over a real network |
| Send SMS (PDU mode) | ✅ | Single-part GSM 7-bit sending verified on a SIM868 over a real network |
| GSM 7-bit and UCS-2 with automatic selection | ✅ | UCS-2 not yet verified on real hardware |
| Multipart send and receive | ✅ | Out-of-order parts, duplicates and reference reuse are handled. Not yet verified on real hardware. |
| Flash (class 0) and 8-bit binary SMS with application ports | ✅ | Not yet verified on real hardware |
| Delivery reports correlated with sent messages | ✅ | Verified for one single-part message on a SIM868 over a real network |
| Scheduled sending, priorities, expiry, cancel and retry | ✅ | |
| Sending through an SMS provider (GatewayAPI) with a sender name | 🧪 | Optional route; tested against a stand-in for the provider API, not against the real service. See [docs/PROVIDERS.md](docs/PROVIDERS.md). |
| Idempotent submission (`Idempotency-Key`) | ✅ | |
| Per-SIM daily/monthly quotas, rate limits, allow and block lists | ✅ | |
| Deduplication of received PDUs | ✅ | |
| National language tables (Turkish, Spanish, Portuguese) | ✅ | Decode only, limited set |
| National language tables for encoding | ❌ | |
| SMS text mode | ❌ | PDU mode only |
| MMS notification decoding (WAP Push) | ✅ | |
| MMS retrieval from the carrier MMSC | 🧪 | Only works when the host itself has an IP route to the MMSC |
| MMS retrieval through the modem's own data stack | ❌ | |
| MMS sending | ❌ | |
| USSD (named commands, optional arbitrary codes) | 🔌 | |

### Modems

| Feature | Status | Notes |
| --- | --- | --- |
| Modem profiles with auto-detection | ✅ | `simcom-2g` detection verified on a SIM868 |
| Capability detection | ✅ | Verified on a SIM868 |
| Automatic reconnect with backoff | ✅ | |
| "Module is powered off" detection and hint | ✅ | Verified on a SIM868 |
| SIM PIN entry with a strict one-attempt policy | ✅ | Simulator-tested |
| Several modems and SIMs in one gateway | 🔌 | Simulator-tested with several simulated modems |
| Software modem simulator | ✅ | |
| Call logging, auto-answer | 🔌 | |
| Call recording through an ALSA capture device | 🔌 | See [limitations](#voice-calls-and-call-recording) |
| GNSS position polling | 🔌 | Receiver power-on accepted on a SIM868; no fix obtained, positions unverified |

### Interfaces and operations

| Feature | Status | Notes |
| --- | --- | --- |
| REST API with OpenAPI document | ✅ | |
| Live event stream (Server-Sent Events) | ✅ | |
| WebSocket streaming | ❌ | SSE only |
| Web dashboard (embedded, no external assets) | ✅ | |
| Prometheus metrics | ✅ | |
| Integrations: webhook, Discord, Slack, Microsoft Teams, Telegram, ntfy, Gotify, syslog | ✅ | Payload construction and delivery logic are tested; third-party services were not contacted by the test suite |
| MQTT integration | ❌ | |
| Integration batching | ❌ | One request per event |
| Routing rules, filters, anonymisation, address book | ✅ | |
| Automation rules (reply, forward) with loop protection | ✅ | |
| SQLite storage with online backup and restore | ✅ | |
| MySQL / MariaDB storage | ✅ | Built-in backup is SQLite-only |
| Configuration reload without restart | ✅ | |
| x86_64 Linux | ✅ | Development platform |
| ARM64 Linux (Raspberry Pi 4) | 🔌 | Builds for `aarch64`; not yet run on a Raspberry Pi by the maintainers |

## Architecture

```mermaid
flowchart LR
    subgraph hw["Hardware"]
        M1["GSM modem 1"]
        M2["GSM modem n"]
    end
    subgraph gw["thugsred-smsgateway"]
        SUP["Modem supervisor (one per modem)"]
        SND["Sender worker (one per modem)"]
        SMS["SMS service: PDU codec, inbound, queue"]
        EV["Event bus and outbox"]
        DISP["Integration dispatcher"]
        CALLS["Call manager"]
        API["HTTP server: REST, SSE, metrics, dashboard"]
        HK["Housekeeping"]
    end
    DB[("SQLite or MySQL/MariaDB")]
    EXT["Webhooks and chat services"]
    USERS["API clients, CLI, browser"]

    M1 <-->|"serial, AT commands"| SUP
    M2 <-->|"serial, AT commands"| SUP
    SUP -->|"received PDUs"| SMS
    SND -->|"SMS-SUBMIT PDUs"| SUP
    SMS --> SND
    SUP -->|"call signals"| CALLS
    SMS --> EV
    CALLS --> EV
    SUP --> EV
    EV --> DISP
    DISP -->|"HTTP or UDP"| EXT
    USERS -->|"HTTP"| API
    API --> SMS
    API --> SUP
    SMS <--> DB
    EV <--> DB
    DISP <--> DB
    HK <--> DB
```

Details, task model and data-flow diagrams are in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Hardware requirements

- A Linux host with systemd. Development and testing happened on Debian 13 (x86_64).
- A GSM or LTE modem that offers an AT command serial port and supports SMS **PDU mode**
  (`AT+CMGF=0`). The modem is reached through a serial device such as `/dev/ttyUSB0`,
  `/dev/ttyS0` or a `/dev/serial/by-path/...` name.
- An active SIM card with SMS service. A 2G-only module (such as the SIM868) needs an
  operator that still runs a GSM network where you are.
- A power supply and antenna that meet the module's requirements. GSM modules draw short
  current peaks during transmission; an undersized supply typically shows up as resets.
- Optional, for call recording only: an ALSA capture device that is physically wired to the
  modem's audio output, and `arecord` from `alsa-utils`.

## Supported modem families

Four profiles are built in. A profile is chosen automatically from the modem's model
identification, or set with `modem.profile`.

| Profile | Intended for | Real-hardware status |
| --- | --- | --- |
| `simcom-2g` | SIMCom SIM800 series, SIM868, SIM808, SIM900 | 🔌 One SIM868 checked: connection, identification, profile detection, capabilities, SIM ready, registration, operator, signal, storage query, and single-part GSM 7-bit receive, send and delivery report over a real network. Everything else unverified. |
| `simcom-lte` | SIMCom SIM7600 / SIM7500, A7600 / A767x, SIM7000 / SIM7070 / SIM7080 | 🔌 Unverified: no module of this family has been tested |
| `quectel` | Quectel EC2x, EG2x, EM0x, BG9x, EG9x, UC20, M95, M66, MC60 | 🔌 Unverified: no module of this family has been tested |
| `generic` | Any modem following 3GPP TS 27.005 / 27.007 | 🔌 Unverified on real hardware; used as the fallback |

The full matrix, every profile field and instructions for custom profiles are in
[docs/MODEMS.md](docs/MODEMS.md).

## Raspberry Pi 4 and ARM64

The gateway **builds for `aarch64`; it has not yet been run on a Raspberry Pi by the
maintainers.** `make build-arm64` cross-compiles on an x86_64 host (or builds natively on an
ARM64 host).

Summary for Raspberry Pi OS 64-bit, to be treated as untested guidance:

1. Install the Rust toolchain and `build-essential`, then build with `make release` on the
   Pi, or copy a cross-compiled build to it.
2. Decide how the modem is attached: through USB (a `/dev/ttyUSB*` device) or through the
   GPIO UART (`/dev/ttyS0`, also reachable as `/dev/serial0`).
3. For the GPIO UART, enable the serial port and disable the serial login console with
   `raspi-config` yourself. The installer never changes boot settings.
4. Set the HAT's UART jumpers to match (USB or GPIO).
5. `sudo make install`, edit the configuration, start the service, press PWRKEY.

Step-by-step instructions are in [docs/RASPBERRY-PI.md](docs/RASPBERRY-PI.md).

## Quick start

Build as a normal user; only the installation step needs root.

```sh
# 1. Build (as your own user)
git clone https://github.com/kawaiipantsu/THUGSred-smsGateway.git
cd THUGSred-smsGateway
make deps
make release

# 2. Install program files, service account, unit file and default configuration
sudo make install

# 3. Edit the configuration (at least the serial device)
sudoedit /etc/thugsred/smsgateway.conf
sudoedit /etc/thugsred/sim.conf

# Optional: a short name for the installed tool
alias smsgw='sudo /opt/THUGSred-smsGateway/bin/thugsred-smsgateway'
smsgw check-config

# 4. Start the service
sudo systemctl enable --now thugsred-smsgateway

# 5. Power the module on if it needs it (see the next section), then verify
smsgw doctor
```

Notes:

- `make install` neither enables nor starts the service, and never overwrites existing
  configuration or data.
- The binary is installed as `/opt/THUGSred-smsGateway/bin/thugsred-smsgateway`, which is not
  on the default `PATH`. The rest of this documentation writes `thugsred-smsgateway` for
  brevity. The configuration directory is readable by root and the service group only, and
  the secrets file `smsgateway.env` by root only, so run the tool with `sudo` as in the alias
  above. Run `doctor` and the `db` commands only after the service has started once (they
  open the database directly, and a database file created by root would not be writable by
  the service); see
  [docs/INSTALLATION.md](docs/INSTALLATION.md#running-the-command-line-tool).
- The default API address is `http://<host>:8080/`, reachable from loopback and private
  address ranges only, **without authentication**. See
  [Security considerations](#security-considerations) before exposing it further.
- The default serial device is `/dev/ttyS0`. For a USB-attached module set `modem.device` to
  `/dev/ttyUSB0` or, better, a stable `/dev/serial/by-path/...` name.

Full installation details: [docs/INSTALLATION.md](docs/INSTALLATION.md).

## Powering the module on (PWRKEY)

> **Important.** GSM modules such as the SIM868 do **not** power on by themselves when they
> are attached or when the host boots. After attaching the module, hold its **PWRKEY** button
> for about two seconds until the NET LED starts blinking.

The gateway cannot press the button for you, but it tells you when this is the problem. When
the serial port opens and the modem does not answer `AT`:

- the modem state becomes `DISCONNECTED` with the error
  `no response to AT commands (module powered off?)`;
- `status`, `modem info`, `doctor`, `GET /api/v1/modem` and the dashboard show a hint that
  explains the PWRKEY step, the baud rate and the UART jumpers;
- a `modem.error` event is emitted once;
- the gateway keeps probing (with the default settings the pause between attempts grows from
  5 to at most 20 seconds in this situation) and connects on its own as soon as the module
  answers. No restart is needed.

The same hint appears when a running module is switched off or stops answering.

## Configuration overview

All files are TOML and live in `/etc/thugsred/`. Every setting is optional; the shipped
examples show the defaults.

| File | Purpose | Mode |
| --- | --- | --- |
| `smsgateway.conf` | Main configuration: modems, SMS policy, database, API, logging, calls, security | `0640` |
| `sim.conf` | SIM profiles: number, PIN, SMSC, network selection, roaming, quotas | `0640` |
| `integrations.conf` | Integrations, routing rules and automation rules | `0640` |
| `addressbook.conf` | Names for known numbers | `0640` |
| `anonymous.conf` | Numbers that are never disclosed to integrations or logs | `0640` |
| `providers.conf` | Optional SMS providers (API token, sender names, limits) | `0640` |
| `smsgateway.env` | Secrets referenced as `"${NAME}"`; loaded by systemd for the service and read directly by the command-line tool | `0600` |
| `modem-profiles/*.toml` | Optional custom or overriding modem profiles | |

Useful facts:

- **Overrides:** environment variables `THUGSRED_SMSGW__SECTION__FIELD=value` and the
  command-line option `--set section.field=value`.
- **Secret references:** `"${ENV_VAR}"` and `"${file:/path/to/secret}"` inside any string.
- **Validation:** `thugsred-smsgateway check-config` (add `--show` to print the effective
  configuration with secrets redacted). Unknown keys produce warnings, invalid values errors.
- **Reload:** `sudo systemctl reload thugsred-smsgateway` (SIGHUP), `thugsred-smsgateway
  reload` or `POST /api/v1/config/reload`. A reload is atomic: if the new configuration is
  invalid it is rejected as a whole and the running configuration stays active. Some settings
  need a restart; the reload reports which.

Complete reference: [docs/CONFIGURATION.md](docs/CONFIGURATION.md).

## Multi-modem and multi-SIM setup

Add one `[[modems]]` entry per module. Each entry inherits the `[modem]` table and may
override any of its settings. USB serial bridges often share the same serial number, so use
the port-based names under `/dev/serial/by-path/` to tell devices apart.

`/etc/thugsred/smsgateway.conf`:

```toml
[modem]
baud_rate = 115200
profile = "auto"

[[modems]]
id = "modem-1"
device = "/dev/serial/by-path/pci-0000:00:14.0-usb-0:1:1.0-port0"
sim = "sim-1"

[[modems]]
id = "modem-2"
device = "/dev/serial/by-path/pci-0000:00:14.0-usb-0:2:1.0-port0"
sim = "sim-2"

[sms]
default_modem = ""        # empty: any ready modem takes messages that name none
```

`/etc/thugsred/sim.conf`:

```toml
[[sims]]
id = "sim-1"
name = "Gateway line 1"
mobile_number = "+4500000001"
[sims.limits]
daily_sms_limit = 200

[[sims]]
id = "sim-2"
name = "Gateway line 2"
mobile_number = "+4500000002"
[sims.network]
roaming_enabled = false
```

A request selects a line with `"modem": "modem-2"` or with `"from": "+4500000002"`. `from`
picks one of the gateway's own SIMs; it cannot change the number recipients see, which is
always decided by the mobile network.

Multi-modem operation was tested with several simulated modems, not with more than one
physical module. More in [docs/MODEMS.md](docs/MODEMS.md).

## Command-line tool

Commands that need the modem talk to the running daemon over its HTTP API; they never open
the serial device themselves.

| Command | Purpose |
| --- | --- |
| `run [--simulate-modem]` | Run the daemon (what the service does); with `--simulate-modem` every configured modem is an independent simulated one |
| `version` | Version, commit, build date, target |
| `check-config [--show]` | Validate the configuration |
| `status` | Daemon, modem, database, queue and integration status |
| `doctor` | Diagnose the installation and print advice |
| `capabilities` | Gateway and modem capabilities (JSON) |
| `sms send --to N --message TEXT [--flash] [--modem ID] [--from N] [--at TIME] [--no-delivery-report] [--idempotency-key KEY]` | Queue a message |
| `sms list [--limit N] [--direction inbound\|outbound]` | Recent messages |
| `sms search [--sender S] [--recipient R] [--text T] [--limit N]` | Search history |
| `pdu encode ...` / `pdu decode [PDU]` | Offline PDU tools |
| `modem info\|signal\|reconnect\|reset-pin-counter [--modem ID]` | Modem information and control |
| `db migrate\|backup [--dir D]\|verify [FILE]\|restore FILE --yes` | Database maintenance |
| `integrations list` / `integrations test NAME` | Integration status and test delivery |
| `ussd NAME [--modem ID]` | Run a configured USSD command |
| `reload` | Ask the daemon to reload its configuration |
| `diagnostics` | Redacted diagnostic report (JSON) |
| `simulator [--link PATH]` | Software modem on a pseudo-terminal (development) |

Global options: `-c, --config <path>` (default `/etc/thugsred/smsgateway.conf`, or the
`THUGSRED_SMSGW_CONFIG` environment variable), `--set section.field=value` (repeatable),
`--json`, `--api-url <URL>`.

## Makefile reference

Run `make help` for the same list. Variables: `PREFIX` (default `/opt/THUGSred-smsGateway`),
`CONFDIR` (default `/etc/thugsred`), `DESTDIR`, `CONFIG`, `BACKUP`, `TEST_MYSQL_URL`.

| Target | Description |
| --- | --- |
| `help` | Show the target list |
| `deps` | Check the toolchain and fetch locked dependencies |
| `build` | Debug build |
| `release` | Optimised release build for this machine |
| `build-arm64` | Release build for Linux ARM64 (Raspberry Pi 4); cross-compiles on other hosts |
| `build-amd64` | Release build for Linux AMD64; cross-compiles on other hosts |
| `check` | Formatting, lints and all tests |
| `fmt` | Format the source code |
| `fmt-check` | Verify formatting |
| `lint` | Clippy with warnings as errors |
| `test` | Unit and integration tests (simulated modem, SQLite; no hardware needed) |
| `test-integration` | Integration tests only (gateway, API and resilience against the simulator) |
| `test-mysql` | All tests against MySQL/MariaDB: `make test-mysql TEST_MYSQL_URL=mysql://user:password@host:3306` |
| `audit` | Check dependencies for known vulnerabilities (`cargo-audit`) |
| `deny` | Check licences, advisories and sources (`cargo-deny`) |
| `secret-scan` | Scan the working tree for credentials (`gitleaks`, or the built-in fallback) |
| `hooks` | Install the optional pre-commit secret scan |
| `docs` | Build the Rust API documentation (`target/doc`) |
| `install` | Install the release binary, unit file and default configuration (keeps existing configuration and data) |
| `uninstall` | Remove the service and program files; keeps configuration, database, logs and recordings |
| `purge` | Remove everything including configuration and all data (asks for confirmation) |
| `reinstall` | Reinstall program files and restart the service if it was running |
| `upgrade` | Back up the database, install the new build, check the configuration and restart |
| `config` | Create missing configuration files from the examples (never overwrites) |
| `check-config` | Validate the configuration (`CONFIG=path`; the installed one by default, else the examples) |
| `migrate` | Apply database migrations |
| `backup` | Create a verified database backup (SQLite) in the backups directory |
| `restore` | Restore a backup: `make restore BACKUP=/path/to/file` (stops the service) |
| `doctor` | Diagnose the installation (uses the installed binary when present) |
| `start` / `stop` / `restart` | Control the service |
| `reload` | Reload the configuration without restarting (SIGHUP) |
| `status` | Service and gateway status |
| `enable` / `disable` | Start the service at boot, or not |
| `logs` | Follow the service journal |
| `logs-messages` | Follow the message log |
| `logs-system` | Follow the system log |
| `simulator` | Run the software modem on a pseudo-terminal (development) |
| `sbom` | Write a CycloneDX software bill of materials to `dist/` |
| `package` | Release archive, SHA-256 checksums and SBOM in `dist/` |
| `deb` | Optional Debian package in `dist/` (needs `dpkg-deb`) |
| `clean` | Remove build artefacts |
| `distclean` | Remove build artefacts and `dist/` |

## REST API examples

The examples assume the default address and no authentication. With
`api.authentication = "bearer"` add `-H 'Authorization: Bearer <token>'`.

Send a message (the `Idempotency-Key` makes a repeated request return the original message
instead of sending again):

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/sms/send \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: order-1001-confirmation' \
  -d '{"to": "+4500000000", "message": "Hello from THUGSred SMS Gateway"}'
```

Response, HTTP `202 Accepted`:

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

`202` means the message was validated and stored in the queue, not that it reached the
network. Follow its progress with `GET /api/v1/messages/{id}/status`.

List received messages:

```sh
curl -sS 'http://127.0.0.1:8080/api/v1/messages?direction=inbound&limit=20'
```

Gateway status:

```sh
curl -sS http://127.0.0.1:8080/api/v1/status
```

Decode a PDU (offline, nothing is transmitted):

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/pdu/decode \
  -H 'Content-Type: application/json' \
  -d '{"pdu": "06915400000010040A91540000002000006211422100004005C8329BFD06"}'
```

Every endpoint, with request and response shapes: [docs/API.md](docs/API.md). The OpenAPI
document is served at `/api/v1/openapi.json`.

## Web dashboard

The dashboard is compiled into the binary and served from `/` on the API port. It consists
of plain ES modules without a build step, loads no assets from other hosts, and renders all
API data through text nodes. When bearer authentication is enabled it asks for a token and
keeps it in the browser's session storage only. It updates live through the event stream,
with polling as a fallback, and has a dark/light toggle that is remembered per browser.

It has been exercised in headless Chromium against the simulator only.

| Page | Purpose |
| --- | --- |
| Overview | Daemon, modem, database, queue and integration status at a glance |
| Send SMS | Compose and queue a message |
| Inbox | Received messages |
| Outbox | Sent messages and their delivery state |
| Message History | Search and export all messages |
| Message Queue | Pending, scheduled and failed outbound messages; retry and cancel |
| PDU Tools | Offline PDU encoder and decoder |
| Calls | Call log and recordings |
| Integrations | Integration status, deliveries, dead letters, test delivery |
| Modem | Identification, SIM, network, signal, capabilities |
| System | Configuration status, audit trail, diagnostics |
| API Documentation | Browser for the OpenAPI document |

Disable it with `web.enabled = false` (restart required). `web.show_message_content` and
`web.show_modem_information` are display settings of the dashboard only; the REST API still
returns those fields.

## Database: SQLite

SQLite is the default and needs no setup:

```toml
[database]
driver = "sqlite"
sqlite_path = "/var/lib/thugsred-smsgateway/messages.db"
sqlite_wal = true
auto_migrate = true
```

The file and schema are created on first start. Back it up with
`thugsred-smsgateway db backup`. See [docs/DATABASE.md](docs/DATABASE.md).

## Database: MySQL or MariaDB

Create a database and a user (replace the placeholders):

```sql
CREATE DATABASE smsgateway CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE USER 'smsgateway'@'192.0.2.%' IDENTIFIED BY 'change-me';
GRANT ALL PRIVILEGES ON smsgateway.* TO 'smsgateway'@'192.0.2.%';
FLUSH PRIVILEGES;
```

Put the connection URL into `/etc/thugsred/smsgateway.env` (mode `0600`):

```sh
SMSGW_MYSQL_URL=mysql://smsgateway:change-me@db.example.invalid:3306/smsgateway
```

and reference it from `/etc/thugsred/smsgateway.conf`:

```toml
[database]
driver = "mysql"
mysql_url = "${SMSGW_MYSQL_URL}"
max_connections = 5
auto_migrate = true
```

Changing the database settings needs a restart. The built-in `db backup` and `db restore`
commands are SQLite-only; use `mysqldump` for MySQL/MariaDB
([docs/BACKUP-RESTORE.md](docs/BACKUP-RESTORE.md)).

> `${NAME}` references are resolved from the process environment first and then from
> `smsgateway.env` next to the main configuration file. systemd loads that file for the
> service; the command-line tool reads it directly, which needs root because the file has
> mode `0600`. See [docs/CONFIGURATION.md](docs/CONFIGURATION.md#secret-references).

## Integrations

Integrations live in `/etc/thugsred/integrations.conf`. Any number may exist, including
several of the same type; an event goes to every enabled integration that subscribes to it
and whose filter matches.

```toml
# Everything received goes to one Discord channel, with address book names.
[[integration]]
name = "discord-sms"
type = "discord"
enabled = true
events = ["sms.received"]
webhook_url = "${DISCORD_WEBHOOK_URL}"
username = "THUGSred SMS Gateway"
template = "SMS from {{message.sender_display}}: {{message.body}}"

# A second Discord webhook: only one sender, and no telephone numbers at all.
[[integration]]
name = "discord-alerts"
type = "discord"
enabled = true
events = ["sms.received", "call.missed"]
webhook_url = "${DISCORD_ALERTS_WEBHOOK_URL}"
anonymize = true

[integration.filter]
sender_allowlist = ["+4500000000"]
message_regex = "(?i)(alert|warning)"

# A signed webhook that never receives message text.
[[integration]]
name = "primary-webhook"
type = "webhook"
enabled = true
events = ["sms.received", "sms.delivered"]
url = "https://example.invalid/sms/events"
sign_payload = true
signing_secret = "${SMS_WEBHOOK_SECRET}"
include_message_body = false
```

Address book (`/etc/thugsred/addressbook.conf`) — names shown in the dashboard, the API and
in `{{message.sender_display}}`:

```toml
[contacts]
"+4500000000" = "Example Person"
"ExampleService" = "Example service (alphanumeric sender)"
```

Always-anonymous list (`/etc/thugsred/anonymous.conf`) — these numbers are replaced in every
integration and in the log files, whatever the integration's own settings are:

```toml
numbers = ["+4500000001", "+45*"]
label = "Anonymous"
redact_body = false
```

Anonymisation applies to what leaves the gateway through integrations and to log files. The
gateway's own database, API and dashboard keep the real numbers, and message text may itself
contain numbers. Payload formats, templates, filters, routing rules, signature verification
and automation are described in [docs/INTEGRATIONS.md](docs/INTEGRATIONS.md).

## PDU tools

The encoder and decoder work offline and never transmit anything.

```console
$ thugsred-smsgateway pdu encode --to +4500000000 --message "Hello from THUGSred SMS Gateway"
encoding: gsm7   parts: 1   (offline: nothing was transmitted)
part 1 (AT+CMGS=40):
0001000A91540000000000001FC8329BFD0699E5EF36888AAC1EA7F23219346D4E41C730BD7C0FE701
```

`thugsred-smsgateway pdu decode <hex>` prints the decoded fields as JSON. The same tools are
available as `POST /api/v1/pdu/encode` and `POST /api/v1/pdu/decode` and on the dashboard's
PDU Tools page. Field explanations and limits: [docs/PDU.md](docs/PDU.md).

## Voice calls and call recording

The gateway logs incoming calls (`RING`, caller ID, end of call) and can optionally answer
them. Limitations you should know before enabling anything:

- All call features are **unverified on real hardware**.
- The AT serial line carries **no audio**. Recording captures from a host ALSA device that
  you must wire physically to the modem's audio output. Without such a device, or without
  `arecord` from `alsa-utils`, the recording status is `unavailable` or `failed`; the gateway
  never fabricates a recording.
- Recording requires `calls.auto_answer = true`, records a fixed number of seconds (at most
  300) and, by default, hangs up afterwards.
- Outgoing calls are only logged when they are dialled through the raw AT endpoint, which is
  disabled by default.
- Answering and recording calls can be subject to consent and telecommunications law. Check
  what applies to you first.

Details: [docs/CALLS.md](docs/CALLS.md).

## Running behind a reverse proxy

The gateway speaks **plain HTTP only**. Terminate TLS at a reverse proxy and let the gateway
listen on loopback:

```toml
[api]
bind = "127.0.0.1"
port = 8080
trusted_proxies = ["127.0.0.1"]     # so allowed_cidrs sees the real client address
allowed_hosts = ["smsgateway.example.invalid"]
authentication = "bearer"
tokens = ["${SMSGW_API_TOKEN}"]
```

- `X-Forwarded-For` is honoured only when the direct peer is listed in `trusted_proxies`.
- The proxy must pass the original `Host` header through: the gateway compares `Origin` with
  `Host` to reject cross-site requests.
- The event stream at `/api/v1/events/stream` needs response buffering switched off.

A commented nginx example is in
[scripts/nginx-smsgateway.conf.example](scripts/nginx-smsgateway.conf.example). It listens on
port 80 only; add your own TLS configuration.

## Security considerations

- **With the default `api.authentication = "none"`, anyone who can reach the port can send
  SMS at your expense and read every stored message. An unauthenticated gateway must never be
  reachable from the internet.** The default `allowed_cidrs` admits loopback and private
  address ranges only; keep it narrow.
- Enable bearer tokens, a separate administrative token and `admin_allowed_cidrs` for
  anything beyond a single trusted host.
- Protect spending with `[limits]` in `sim.conf`, `sms.outbound_rate_limit_per_minute`,
  `security.destination_allowlist` and `security.max_outbound_queue_size`.
- Raw AT commands and raw PDU submission are disabled by default.
- Secrets belong in `smsgateway.env` or in files referenced with `${file:...}`, not in the
  configuration files themselves.
- Received SMS content is untrusted input. It is escaped for each integration, and mentions
  are neutralised for chat services.

The threat model and every control are described in [docs/SECURITY.md](docs/SECURITY.md). To
report a vulnerability see [SECURITY.md](SECURITY.md).

## Troubleshooting

Start with `sudo thugsred-smsgateway doctor`; it checks the configuration, directories,
database, serial device, conflicting services, modem, SIM and network, and prints advice for
each finding.

| Symptom | Likely cause | What to do |
| --- | --- | --- |
| Modem `DISCONNECTED`, "no response to AT commands (module powered off?)" | Module not powered on | Hold PWRKEY for about two seconds; check baud rate and UART jumpers |
| "cannot open /dev/...: No such file or directory" | Wrong device name or module not attached | Check `modem.device`; list `/dev/serial/by-path/` |
| "Permission denied" opening the device | Service user not in the device's group | `sudo usermod -aG dialout thugsred-smsgateway`, then restart |
| Device busy, garbled answers | ModemManager or a serial login console uses the port | Install the provided udev rule, or disable `serial-getty@<tty>.service` |
| State `SIM_LOCKED` | PIN needed, PIN rejected, or PUK required | Configure the PIN in `sim.conf`; after a rejected PIN correct it and run `modem reset-pin-counter` |
| State stays `REGISTERING` | No coverage, antenna, inactive SIM, or no 2G network | Check antenna and SIM; a 2G module needs a GSM network |
| Messages stay `queued` | Modem not `READY`, roaming blocked, quota or rate limit reached | `status`, then `GET /api/v1/queue/{id}` for the reason |
| Message in `submission_unknown` | The modem went silent after the PDU was handed over | Check whether it arrived, then retry or leave it; it is never resent automatically |
| "cannot listen on 0.0.0.0:8080" | Port already in use | Change `api.port` or stop the other program |
| Reload rejected | New configuration invalid | `check-config`; the previous configuration is still active |

More: [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md).

## Backup and restore

Back up the database, `/etc/thugsred/`, and, if you use them, the recordings and attachment
directories.

```sh
BIN=/opt/THUGSred-smsGateway/bin/thugsred-smsgateway

sudo $BIN db backup            # online, verified, SQLite only
sudo $BIN db verify /opt/THUGSred-smsGateway/backups/messages-<timestamp>.db

sudo systemctl stop thugsred-smsgateway
sudo $BIN db restore /opt/THUGSred-smsGateway/backups/messages-<timestamp>.db --yes
sudo systemctl start thugsred-smsgateway
```

`db restore` keeps the previous database next to the restored one as
`messages.db.before-restore-<timestamp>` and gives the restored file to the owner of its
directory. `make restore BACKUP=...` does the stop, restore and start for you. MySQL/MariaDB procedures and details:
[docs/BACKUP-RESTORE.md](docs/BACKUP-RESTORE.md).

## Upgrading

```sh
git pull
make release            # as a normal user
sudo make upgrade       # backs up (SQLite), installs, checks the configuration, restarts
```

Schema migrations are applied at start when `database.auto_migrate = true` (the default), or
with `thugsred-smsgateway db migrate`. Rollback steps are in
[docs/BACKUP-RESTORE.md](docs/BACKUP-RESTORE.md#upgrade-procedure).

## Documentation index

| Document | Contents |
| --- | --- |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Components, tasks, data flows, modem state machine |
| [docs/INSTALLATION.md](docs/INSTALLATION.md) | Build, install, uninstall, packaging, cross-compilation |
| [docs/RASPBERRY-PI.md](docs/RASPBERRY-PI.md) | Raspberry Pi 4 notes |
| [docs/MODEMS.md](docs/MODEMS.md) | Profiles, capabilities, AT commands, multi-modem, SIM handling, simulator |
| [docs/CONFIGURATION.md](docs/CONFIGURATION.md) | Every configuration field |
| [docs/API.md](docs/API.md) | REST API reference |
| [docs/PDU.md](docs/PDU.md) | PDU codec and tools |
| [docs/PROVIDERS.md](docs/PROVIDERS.md) | Sending through an SMS provider, sender names, status callbacks |
| [docs/INTEGRATIONS.md](docs/INTEGRATIONS.md) | Integrations, events, templates, routing, anonymisation, automation |
| [docs/DATABASE.md](docs/DATABASE.md) | Engines, schema, message states, retention |
| [docs/SECURITY.md](docs/SECURITY.md) | Threat model and controls |
| [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) | Symptom, cause and fix tables |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Repository layout, tests, extending, release process |
| [docs/CALLS.md](docs/CALLS.md) | Call logging, auto-answer, recording |
| [docs/BACKUP-RESTORE.md](docs/BACKUP-RESTORE.md) | Backup, restore, upgrade, rollback |
| [CHANGELOG.md](CHANGELOG.md) | Release notes |
| [CONTRIBUTING.md](CONTRIBUTING.md) | How to contribute and report compatibility |
| [SECURITY.md](SECURITY.md) | Reporting vulnerabilities |

## Contributing

Contributions and modem compatibility reports are welcome. Run `make check` before opening a
pull request, use synthetic test data only, and read [CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

MIT. See [LICENSE](LICENSE).

THUGSred — <https://thugs.red>

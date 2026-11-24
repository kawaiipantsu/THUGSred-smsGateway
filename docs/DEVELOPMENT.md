# Development

- [Repository layout](#repository-layout)
- [Building](#building)
- [Test suites](#test-suites)
- [Using the simulator](#using-the-simulator)
- [Running against MySQL or MariaDB](#running-against-mysql-or-mariadb)
- [Adding a modem profile](#adding-a-modem-profile)
- [Adding an integration type](#adding-an-integration-type)
- [Adding a migration](#adding-a-migration)
- [Adding configuration, endpoints and events](#adding-configuration-endpoints-and-events)
- [Coding conventions](#coding-conventions)
- [CI overview](#ci-overview)
- [Release process](#release-process)

See [ARCHITECTURE.md](ARCHITECTURE.md) for how the parts fit together and
[../CONTRIBUTING.md](../CONTRIBUTING.md) for contribution rules.

## Repository layout

| Path | Contents |
| --- | --- |
| `src/` | The crate: library `thugsred_smsgateway` plus the binary `thugsred-smsgateway`. Module map in [ARCHITECTURE.md](ARCHITECTURE.md#module-map). |
| `tests/` | Integration tests (`gateway.rs`, `api.rs`, `resilience.rs`, `examples.rs`) and the shared harness `common/mod.rs` |
| `migrations/sqlite/`, `migrations/mysql/` | SQL schema, embedded at build time |
| `config/` | Example configuration files and the built-in modem profiles (embedded at build time) |
| `web/` | Dashboard: `index.html`, `favicon.svg`, `assets/` (plain ES modules and CSS, no build step; embedded at build time) |
| `systemd/` | Unit file |
| `scripts/` | Installer, uninstaller, udev rule, nginx example, SBOM and Debian package scripts, secret scan, pre-commit hook |
| `docs/` | Documentation |
| `.github/workflows/ci.yml` | CI |
| `build.rs` | Embeds commit id, build date and target triple |
| `deny.toml` | `cargo-deny` policy |
| `Makefile` | Project commands (`make help`) |
| `Cargo.toml`, `Cargo.lock` | Manifest and locked dependencies |

Because `config/modem-profiles/`, `migrations/` and `web/` are compiled into the binary,
changing them requires a rebuild.

## Building

```sh
make deps        # toolchain check, fetch locked dependencies
make build       # debug build: target/debug/thugsred-smsgateway
make release     # optimised build: target/release/thugsred-smsgateway
make docs        # Rust API documentation in target/doc
```

- Minimum Rust version: 1.82 (`rust-version` in `Cargo.toml`). Edition 2021.
- All builds use `--locked`; update `Cargo.lock` deliberately.
- Build as a normal user. The Makefile warns when a build runs as root.
- Release profile: `opt-level = 3`, thin LTO, stripped symbols.
- `make check` runs the formatter check, Clippy and all tests; run it before every commit.

## Test suites

No hardware is needed for any test. Everything runs against the in-process software modem
and a temporary database.

```sh
make test               # unit tests and all integration tests (SQLite)
make test-integration   # the integration tests only
cargo test --locked --lib                     # unit tests only
cargo test --locked --test gateway            # one integration suite
cargo test --locked --test api cidr           # tests whose name contains "cidr"
```

### Unit tests

Next to the code they test (`#[cfg(test)]` modules):

| Module | Covers |
| --- | --- |
| `sms/pdu/gsm7`, `types`, `encode`, `decode` | Alphabet tables, septet packing, addresses, timestamps, data coding schemes, user data headers, segmentation limits, encode/decode round trips, malformed input |
| `modem/parser` | Line buffering, final results, unsolicited result codes, response parsers |
| `modem/actor` | Command serialisation, echo and fragment handling, interleaved notifications, timeouts, the ambiguous-submission cases |
| `modem/profiles` | Built-in profiles, detection, directory overrides |
| `config/tests` | Defaults, overrides, secret references, validation errors and warnings, redaction, restart detection |
| `routing` | Subscriptions, routing rules, `stop_processing`, every filter field |
| `templates` | Substitution, escaping per destination, mention neutralisation |
| `integrations` | Payload of every integration type, signatures, anonymisation |
| `mms`, `gnss` | Notification decoding, fetch validation, position parsing |
| `calls` | Recording failure paths (a recording is never fabricated) |
| `database`, `logging`, `metrics`, `utils`, `api/openapi` | Message round trips (nulls, blobs, Unicode), generic rows and statistics, verified SQLite backup; log rotation and pruning; metrics rendering; helpers; document shape |

### Integration tests

Each test starts a complete gateway (`tests/common/mod.rs`): configuration files in a
temporary directory, a fresh database, one or more simulated modems, and for API tests a
real HTTP listener on a local port.

| Suite | Covers |
| --- | --- |
| `tests/gateway.rs` | Receiving, storing and deleting from the modem; Unicode, binary, flash and malformed messages; multipart with out-of-order parts, duplicates and reference reuse; multipart sending and delivery-report correlation; failed and uncorrelated reports; rejected submissions with retry and dead letter; the ambiguous submission that is never retried; cancel, schedule, expiry and destination policy |
| `tests/api.rs` | Health, status, metrics and documentation endpoints; send validation, idempotency and lifecycle; history search, export and untrusted content; offline PDU tools; raw AT and USSD when enabled; bearer tokens, administrative restrictions and CSRF; CIDR restrictions, proxies, rate and size limits; ignored forwarded headers; the live event stream and integration test endpoint |
| `tests/resilience.rs` | Disconnect and recovery with queued messages; the unpowered-module hint; the one-attempt SIM PIN rule; queue survival across a restart without resending uncertain parts; several modems with SIM routing and quotas; integration fan-out, filtering, anonymisation, retry and dead-lettering; routing rules and automation with loop protection; reload with valid and invalid configuration; call logging and honest recording status |
| `tests/examples.rs` | The shipped example configuration loads without errors and matches the defaults in the code; the examples contain nothing that looks like a real secret |

What the tests do **not** cover: real serial hardware, real mobile networks, third-party
integration services, audio capture, and the dashboard (which was exercised by hand in
headless Chromium against the simulator).

## Using the simulator

The software modem (`src/modem/simulator.rs`) emulates a SIMCom-style module that identifies
as a SIM868.

**Quickest: the whole gateway with simulated modems.**

```sh
mkdir -p dev && cp config/smsgateway.conf.example dev/smsgateway.conf
cargo run -- --config dev/smsgateway.conf \
    --set paths.data=$PWD/dev/data \
    --set paths.runtime=$PWD/dev/run \
    --set paths.recordings=$PWD/dev/recordings \
    --set paths.attachments=$PWD/dev/attachments \
    --set paths.backups=$PWD/dev/backups \
    --set paths.sim_config=$PWD/config/sim.conf.example \
    --set paths.addressbook=$PWD/config/addressbook.conf.example \
    --set paths.anonymous=$PWD/config/anonymous.conf.example \
    --set integrations.config_file=$PWD/config/integrations.conf.example \
    --set database.sqlite_path=$PWD/dev/data/messages.db \
    --set logging.system_log=$PWD/dev/log/system.log \
    --set logging.messages_log=$PWD/dev/log/messages.log \
    --set logging.integrations_log=$PWD/dev/log/integrations.log \
    --set logging.audit_log=$PWD/dev/log/audit.log \
    --set api.bind=127.0.0.1 \
    run --simulate-modem
```

Every configured modem gets its own independent simulated modem. The dashboard is at
<http://127.0.0.1:8080/>, and sending works end to end (the simulator accepts every
submission and answers requested delivery reports). The `dev/` directory is your own
scratch space; keep it out of commits.

**With injection of inbound traffic: the pseudo-terminal simulator.**

```sh
# terminal 1
make simulator                      # links the pseudo-terminal at /tmp/ttySMSGW
# or: cargo run -- simulator --link <path>

# terminal 2: the same command as above, with these changes
#   --set modem.device=/tmp/ttySMSGW   run          (without --simulate-modem)
```

Type commands into terminal 1:

```text
sms +4500000001 Hello from the simulator
sms ExampleService Your code is 000000
call +4500000002
hangup
reg 2
reg 1
fail 38
ok
silent
power off
power on
drop
sent
quit
```

`silent` makes the simulator accept a PDU and never answer, which produces a
`submission_unknown` message; `power off` reproduces the PWRKEY hint; `fail <code>` rejects
submissions with a `+CMS ERROR`. The full command list is in
[MODEMS.md](MODEMS.md#the-simulator). This path uses the real serial connector, so it also
exercises device opening and reconnect behaviour.

Use synthetic numbers and content only (`+4500000000`–`+4500000002`).

## Running against MySQL or MariaDB

```sh
make test-mysql TEST_MYSQL_URL=mysql://root:ci-only-password@127.0.0.1:3306
```

- `TEST_MYSQL_URL` is a **server** URL without a database name. The user must be allowed to
  create databases.
- Every test creates its own database named `t_<random hex>` and leaves it behind. Use a
  disposable server and drop it afterwards.
- With the variable set, both the unit tests of the database layer and all integration tests
  use MySQL/MariaDB instead of SQLite.

A disposable server, for example with a container runtime:

```sh
docker run --rm -d --name smsgw-test-db -p 127.0.0.1:3306:3306 \
    -e MARIADB_ROOT_PASSWORD=ci-only-password mariadb:11.4
make test-mysql TEST_MYSQL_URL=mysql://root:ci-only-password@127.0.0.1:3306
docker stop smsgw-test-db
```

CI runs the suite against `mariadb:11.4` and `mysql:8.4`.

## Adding a modem profile

For local use, no code is needed: drop a TOML file into the profile directory
([MODEMS.md](MODEMS.md#adding-a-custom-profile)).

To add a built-in profile:

1. Create `config/modem-profiles/<name>.toml`. Fields are described in
   [MODEMS.md](MODEMS.md#profile-fields).
2. Add it to the `BUILTIN` list in `src/modem/profiles.rs`.
3. Extend the `builtin_profiles_parse_and_detect` test with the identification strings the
   modem really returns for `AT+CGMI` and `AT+CGMM`.
4. If the family needs a new GNSS dialect, add the power-on and query commands and a parser
   in `src/gnss.rs`, with a test using a captured response.
5. Document it in [MODEMS.md](MODEMS.md): the profile table and the compatibility matrix,
   with an honest status. A profile written from a manual and not run on hardware is
   "unverified".

Order matters: the first profile whose `models` list matches wins. A manufacturer is only
used for unlisted models, and only when exactly one profile names it; a new profile that
shares a manufacturer with an existing one makes that fallback ambiguous (`generic`).

## Adding an integration type

1. Add the type name to `INTEGRATION_TYPES` in `src/config/mod.rs`.
2. If it needs new options, add fields to `IntegrationDef` in `src/config/model.rs` (with
   defaults), add secret-bearing keys to `SECRET_KEYS`, and add the required-field checks to
   the `match d.kind` block in `validate`.
3. Add a branch to `build_request` in `src/integrations/mod.rs` that returns a `RequestSpec`.
   Keep it pure (no I/O). Use `chat_text` with the right `Escape` mode for the destination
   and a sensible length limit.
4. If the destination has its own markup, add an `Escape` variant in `src/templates.rs`
   with tests, including mention neutralisation.
5. Add unit tests for the payload, including hostile content.
6. Add an entry to `config/integrations.conf.example` (disabled, with `example.invalid`
   URLs); `tests/examples.rs` then checks that it loads.
7. Document it in [INTEGRATIONS.md](INTEGRATIONS.md) and the field table in
   [CONFIGURATION.md](CONFIGURATION.md#integration).

Delivery, retries, rate limits, anonymisation and dead-lettering are shared and need no
changes. A transport other than HTTP or UDP needs a new `RequestSpec` variant and handling
in `Sender::deliver`.

## Adding a migration

1. Add `migrations/sqlite/000N_<name>.sql` **and** `migrations/mysql/000N_<name>.sql` with
   equivalent changes.
2. Register both in `MIGRATIONS_SQLITE` and `MIGRATIONS_MYSQL` in `src/database/mod.rs` with
   the same version number and name.
3. Follow the schema conventions:
   - only text, integer, real and blob columns (MySQL: `VARCHAR(191)` for indexed text,
     `VARCHAR(64)` for ids, `VARCHAR(32)` for timestamps, `MEDIUMTEXT`, `MEDIUMBLOB`,
     `BIGINT`; `ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci`);
   - timestamps as RFC 3339 UTC text, written with `utils::now()` / `utils::ts()`;
   - no foreign keys, no triggers, no engine-specific functions in queries.
4. Statements are split at `;` and lines starting with `--` are dropped. Do not put a
   semicolon inside a string literal or comment within a statement.
5. Never edit a migration that has been released. Add a new one.
6. Migrations are forward-only; note anything that prevents a downgrade in the changelog.
7. Update [DATABASE.md](DATABASE.md), and the table lists in `table_counts` and
   `apply_retention` (`src/database/misc.rs`) when you add tables.
8. Run `make test` and `make test-mysql`.

## Adding configuration, endpoints and events

**Configuration field.** Add it to the struct in `src/config/model.rs` with its default; add
validation in `validate`; add it to `restart_required` if it only applies at startup; add it
to the matching example file with the same default (`tests/examples.rs` compares them); and
document it in [CONFIGURATION.md](CONFIGURATION.md).

**API endpoint.** Add the handler in `src/api/handlers.rs`, the route in `router`
(`src/api/mod.rs`) and an entry in `ops()` (`src/api/openapi.rs`). Call `require_admin` first
in administrative handlers and `audit` for anything that changes state. Return errors as
`ApiError`. Document it in [API.md](API.md).

**Event.** Add the name to `KNOWN_EVENTS`, emit it with `Events::emit`, add a built-in text
in `default_template`, and document the payload in [INTEGRATIONS.md](INTEGRATIONS.md#events).
Never put location data or secrets into event data: events are forwarded.

## Coding conventions

- `cargo fmt` formatting; Clippy clean with `-D warnings` (`make lint`).
- No `unsafe`, with one documented exception in the pseudo-terminal simulator.
- Errors: `thiserror` enums in library code, `anyhow` at the command-line boundary. No
  panics on external input: PDUs, modem output, HTTP requests and configuration are all
  untrusted. `expect` is reserved for invariants (static regular expressions, client
  construction).
- Everything that touches a modem goes through its supervisor; never write to the serial
  stream from elsewhere.
- Persist before acting when the action cannot be undone: the part state `sending` before a
  submission, the PIN attempt before the PIN.
- Database access only through `src/database/`, with bound parameters, written so that the
  same SQL runs on SQLite and MySQL/MariaDB.
- Read the configuration through the shared handle at the point of use (`cfg.load()`), so
  that reloads take effect; do not cache values across awaits longer than one operation.
- Logging through `tracing` with structured fields. Targets `messages`, `integrations` and
  `audit` go to their own files. Never log secrets, PINs, or message content outside the
  `log_message_content` path; mask numbers with `utils::mask_number` where the redaction
  setting requires it.
- Metric labels must be low-cardinality: never numbers or content.
- Tests use synthetic data only: numbers `+4500000000` and neighbours, `example.invalid`
  hosts, addresses from `192.0.2.0/24`.
- Commit messages follow Conventional Commits ([CONTRIBUTING.md](../CONTRIBUTING.md)).

## CI overview

`.github/workflows/ci.yml` runs on pushes to `main` and on pull requests:

| Job | What it does |
| --- | --- |
| Format and Clippy | `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets -- -D warnings` |
| Tests (SQLite, simulated modem) | Unit tests, then the four integration suites |
| Tests (MariaDB 11.4, MySQL 8.4) | The whole suite with `TEST_MYSQL_URL` against a service container |
| Release build | `make build-amd64` and `make build-arm64` (cross-compiled), with the binaries uploaded as artifacts |
| Dependency advisories and licences | `cargo-deny` |
| Secret scan | `gitleaks` over the full history |

The workflow has read-only repository permissions.

## Release process

1. Make sure `make check` passes, and `make test-mysql` against MariaDB and MySQL.
2. Run `make audit`, `make deny` and `make secret-scan`.
3. Update the version in `Cargo.toml` (the Makefile and the binary read it from there) and
   refresh `Cargo.lock` with a build.
4. Move the "Unreleased" entries in [CHANGELOG.md](../CHANGELOG.md) under the new version
   and date. Call out migrations, configuration changes and anything that affects rollback.
5. Update status claims in the documentation only for what was actually verified.
6. Commit, tag (`v<version>`), push.
7. Build the artefacts on each target architecture (or cross-compile the binary):

   ```sh
   make package      # dist/thugsred-smsgateway-<version>-linux-<arch>.tar.gz, SBOM, SHA256SUMS
   make deb          # dist/thugsred-smsgateway_<version>_<arch>.deb (optional)
   make package      # again, so that SHA256SUMS includes the .deb
   ```

   `make package` builds the release binary, writes the CycloneDX SBOM
   (`make sbom`, needs `python3`), creates the archive with root-owned entries, and writes
   `SHA256SUMS` over the archives, the SBOM and any `.deb` present. `make package` names the
   archive after the **host** architecture and packs `target/release/`; for a cross-compiled
   binary, build the archive on the target architecture or assemble it by hand.
8. For reproducible build dates set `SOURCE_DATE_EPOCH`.
9. Publish the archive, the `.deb`, the SBOM and `SHA256SUMS` with the release notes from
   the changelog. Verify downloads with `sha256sum -c SHA256SUMS`.

`make distclean` removes `dist/` and the build output.

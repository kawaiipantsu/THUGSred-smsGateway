# Contributing to THUGSred-smsGateway

Thank you for helping. Bug reports, fixes, documentation improvements and, above all,
**modem compatibility reports** are welcome.

- Repository: <https://github.com/kawaiipantsu/THUGSred-smsGateway>
- Security issues: do not open a public issue; see [SECURITY.md](SECURITY.md).
- Developer documentation: [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)

## Build, test, lint

```sh
make deps       # check the toolchain, fetch locked dependencies
make build      # debug build
make check      # formatting check, Clippy with warnings as errors, all tests
```

`make check` must pass before you open a pull request. It needs no hardware: the tests run
against the built-in software modem and SQLite.

| Command | Use |
| --- | --- |
| `make fmt` | Format the code |
| `make lint` | Clippy only |
| `make test` | Tests only |
| `make test-mysql TEST_MYSQL_URL=mysql://root:ci-only-password@127.0.0.1:3306` | The whole suite against a disposable MySQL or MariaDB server; required for changes to the database layer or migrations |
| `make secret-scan` | Scan the working tree for credentials |
| `make hooks` | Install the pre-commit secret scan (recommended) |
| `make audit`, `make deny` | Dependency checks; run them when you change dependencies |

Build as a normal user, not as root.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```text
<type>(<optional scope>): <summary in the imperative, lower case, no full stop>

<optional body: what and why>

<optional footers, e.g. BREAKING CHANGE: ...>
```

Types: `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `build`, `ci`, `chore`.
Scopes follow the module names, for example `modem`, `sms`, `pdu`, `api`, `web`,
`integrations`, `config`, `db`, `cli`.

```text
fix(pdu): keep surrogate pairs together when splitting UCS-2 parts
feat(integrations): add rate limit per integration
docs(modems): add compatibility report for an LTE module
```

Mark incompatible changes (configuration, API, schema) with `!` after the type or a
`BREAKING CHANGE:` footer, and describe the migration path.

## Test data: synthetic only

The repository must never contain real messages or identifiers.

| Kind | Use |
| --- | --- |
| Telephone numbers | `+4500000000`, `+4500000001`, `+4500000002`; prefixes such as `+45*` |
| Alphanumeric senders | `ExampleService` and similar invented names |
| Host names | `example.invalid` and subdomains |
| IP addresses | `192.0.2.0/24` (and loopback) |
| Message text | Invented text. Never a message you actually received, however harmless. |
| PDUs | Built with `pdu encode` or the test helpers from synthetic data. Never a PDU captured from a real network: it contains the sender, the service centre number and the content. |
| SIM and device identifiers | Never real ICCIDs, IMSIs, IMEIs or MSISDNs. Use obviously synthetic values (zeros, or the test network prefix `00101`). |
| Operator names and codes | `Example Operator`, `00101` |
| Credentials in examples | `change-me`, `user:password@`, `${NAME}` references, `<placeholders>` |

If you need a real-world PDU to reproduce a decoding bug, rebuild an equivalent one with
synthetic addresses and text that triggers the same code path, and describe how you derived
it.

## No secrets, no deployment configuration

- Never commit `smsgateway.conf`, `sim.conf`, `integrations.conf`, `addressbook.conf`,
  `anonymous.conf`, `smsgateway.env`, databases, logs, recordings, keys or tokens. They are
  listed in `.gitignore`, and the pre-commit hook refuses them by name.
- Only the `*.example` files in `config/` are tracked. Keep their values in step with the
  defaults in `src/config/model.rs`; `tests/examples.rs` checks this.
- No webhook URLs, bot tokens, API tokens, database passwords or SIM PINs anywhere,
  including tests, documentation, commit messages and issue comments.
- No local paths, user names or host names of your own machines in code, tests or
  documentation.
- Run `make hooks` once, and `make secret-scan` before pushing. CI runs `gitleaks` on every
  pull request.

If a secret was committed by mistake, revoke it first; removing it from history does not
make it safe again.

## Reporting modem compatibility

Real-hardware results are the most valuable contribution right now: so far a single module
has been checked. Open an issue titled "Compatibility: <manufacturer> <model>" (or a pull
request that updates [docs/MODEMS.md](docs/MODEMS.md)).

### What to include

1. **Hardware.** Board or adapter (for example "Waveshare GSM/GPRS/GNSS HAT"), module,
   how it is connected (USB port, USB-UART adapter, GPIO UART), and the USB vendor and
   product id of the serial bridge if there is one (`lsusb`).
2. **Host.** Distribution and version, architecture (`uname -m`), gateway version
   (`thugsred-smsgateway version`).
3. **Identification and capabilities**, redacted as described below:

   ```sh
   thugsred-smsgateway modem info
   thugsred-smsgateway capabilities
   thugsred-smsgateway doctor
   ```

4. **What you tested and what happened**, one line each. Please be explicit about what you
   did *not* test.

   | Function | Result |
   | --- | --- |
   | Connection and identification | works / fails / not tested |
   | Profile auto-detection (which profile?) | |
   | SIM ready, SIM PIN entry | |
   | Network registration, operator, signal | |
   | Receive SMS (single, multipart, Unicode) | |
   | Send SMS (single, multipart, Unicode) | |
   | Delivery reports | |
   | USSD | |
   | Incoming call detection, caller ID | |
   | Auto-answer, recording (with which audio path?) | |
   | GNSS fix | |
   | MMS notification | |
   | Power-off detection and recovery | |

5. **Configuration that was needed**: profile name, baud rate, `init_commands`,
   `sms_storage`, `preferred_network`, or a custom profile file.
6. **For failures**: the relevant lines of `system.log` at `log_level = "debug"`, and the
   message state and error from `GET /api/v1/messages/{id}/status`.
7. Optionally the report from `thugsred-smsgateway diagnostics`. It masks secrets and
   reduces IMEI, ICCID, IMSI and MSISDN to their last four characters, but it still contains
   your configuration (host names, integration names, device paths) and operator name:
   read it before posting and remove what you do not want public.

### Redact before posting

| Remove or replace | Where it appears |
| --- | --- |
| IMEI | `modem info --json`, `GET /api/v1/modem`, trace logs |
| ICCID, IMSI | `modem info --json`, `GET /api/v1/sim`, trace logs |
| Your own number (MSISDN, `mobile_number`) and every other telephone number | `status`, `modem info`, logs, PDUs |
| Service centre (SMSC) number | `GET /api/v1/sim`, PDUs |
| Operator name and MCC/MNC, if you prefer not to reveal your network | `status`, `modem info` |
| Message text and PDUs | Logs at `trace` level contain PDUs; a PDU contains numbers and content |
| Host names, IP addresses, user names, paths in your home directory | Logs, `doctor` |
| Tokens, webhook URLs | Should never appear, but check |

Replace values with placeholders of the same shape (`<IMEI>`, `+4500000000`) rather than
deleting lines, so that the output stays readable. The firmware revision and model strings
are useful and not personal: please keep them exactly as reported.

Do not attach `trace` logs unless asked, and never unredacted.

## Pull request checklist

- [ ] `make check` passes.
- [ ] `make test-mysql` passes, if the change touches `src/database/` or `migrations/`.
- [ ] New behaviour has tests; bug fixes have a test that fails without the fix.
- [ ] Tests and examples use synthetic data only (numbers, hosts, text, identifiers).
- [ ] No secrets, deployment configuration, databases, logs or recordings in the diff;
      `make secret-scan` reports no findings.
- [ ] New or changed configuration fields have a default, validation, an entry in the
      matching `config/*.example` file and in [docs/CONFIGURATION.md](docs/CONFIGURATION.md).
- [ ] New or changed endpoints are in the OpenAPI table (`src/api/openapi.rs`) and in
      [docs/API.md](docs/API.md); administrative ones call `require_admin` and are audited.
- [ ] Schema changes come as a new migration for **both** engines; released migrations are
      not edited; [docs/DATABASE.md](docs/DATABASE.md) is updated.
- [ ] Documentation does not claim more than was verified. Anything not run on real hardware
      is described as unverified or simulator-tested.
- [ ] User-visible changes are listed under "Unreleased" in [CHANGELOG.md](CHANGELOG.md).
- [ ] Commits follow Conventional Commits; breaking changes are marked.
- [ ] No new `unsafe` code, no panics on external input, no secrets or message content in
      logs or metric labels.

## Licence

By contributing you agree that your contribution is licensed under the MIT licence of the
project ([LICENSE](LICENSE)).

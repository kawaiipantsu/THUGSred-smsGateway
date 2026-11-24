# Troubleshooting

Start here:

```sh
sudo /opt/THUGSred-smsGateway/bin/thugsred-smsgateway doctor
sudo /opt/THUGSred-smsGateway/bin/thugsred-smsgateway status
sudo journalctl -u thugsred-smsgateway -n 100
```

(The examples below write `thugsred-smsgateway` for the full path. Run it with `sudo`; see
[INSTALLATION.md](INSTALLATION.md#running-the-command-line-tool).)

- [Reading `doctor` output](#reading-doctor-output)
- [Where the logs are](#where-the-logs-are)
- [The modem does not answer](#the-modem-does-not-answer)
- [Device missing, permission denied, or busy](#device-missing-permission-denied-or-busy)
- [SIM locked or PUK required](#sim-locked-or-puk-required)
- [Not registered, or roaming blocks sending](#not-registered-or-roaming-blocks-sending)
- [Messages stuck in the queue](#messages-stuck-in-the-queue)
- [Messages are not received](#messages-are-not-received)
- [Integration failures and dead letters](#integration-failures-and-dead-letters)
- [The service does not start](#the-service-does-not-start)
- [Configuration reload rejected](#configuration-reload-rejected)
- [API and dashboard access](#api-and-dashboard-access)
- [Command-line tool problems](#command-line-tool-problems)
- [Database problems](#database-problems)
- [Calls and recording](#calls-and-recording)

## Reading `doctor` output

`doctor` runs a series of checks and prints one line per finding, with advice on the line
below:

```text
THUGSred SMS Gateway doctor

[ OK ] operating system: <distribution name>
[ OK ] architecture: x86_64
[ OK ] configuration: /etc/thugsred/smsgateway.conf is valid
[ OK ] directory: data: /var/lib/thugsred-smsgateway
[ OK ] disk space: <free> MiB free of <total> MiB
[ OK ] database: sqlite reachable, schema version 1
[ OK ] API: daemon answers at http://127.0.0.1:8080
[ OK ] serial device: modem-1: /dev/ttyUSB0 (group dialout, mode 660)
[FAIL] modem: modem-1: DISCONNECTED: ?
       -> The serial port opened but the modem does not answer AT commands. ...
[ OK ] systemd service: active, enabled

10 check(s), 0 warning(s), 1 failure(s)
```

| Tag | Meaning |
| --- | --- |
| `[ OK ]` | Fine |
| `[WARN]` | Worth a look; does not make `doctor` fail |
| `[FAIL]` | Needs fixing. `doctor` exits with a non-zero status when there is at least one. |

| Check | What it looks at | Typical findings |
| --- | --- | --- |
| operating system, architecture | `/etc/os-release`; `x86_64` and `aarch64` are accepted | Warning on other architectures |
| configuration | Loads and validates all files | Every configuration warning is listed; an invalid configuration is a failure and ends the remaining configuration-dependent checks |
| directory: data, logs, recordings, backups, attachments | Existence and write access for the current user | A missing directory; "not writable by `<user>`", which is normal when you run `doctor` as an unprivileged user other than the service account |
| disk space | Free space on the data filesystem | Warning below 500 MiB |
| database | Opens the database, compares the schema version | Warning when migrations are pending; failure when it cannot be opened |
| API | Calls `GET /api/v1/status` on the daemon | Warning when the daemon is not running or not reachable |
| serial device (per enabled modem) | Exists, is a character device, can be opened by the current user or the service user | Failure with the exact `usermod` command when neither can open it |
| serial conflict | `serial-getty@<tty>` active, `console=<tty>` on the kernel command line, ModemManager active | Failures for the first two, a warning for ModemManager |
| modem, SIM, network (daemon running) | Asks the daemon for the modem's state | Failure with the daemon's hint unless the modem is `READY` |
| modem probe, SIM, network, signal (daemon **not** running) | Opens the serial device itself and sends `AT`, `AT+CGMI`, `AT+CGMM`, `AT+CPIN?`, `AT+CREG?`, `AT+CSQ` | "no answer to AT" when the module is off; warning for a weak signal (below 30 %) |
| capabilities | Summary of what each identified modem supports | Informational |
| systemd service | `is-enabled` and `is-active` | Warning when not active or not installed |

`doctor --json` (global option `--json`) prints the same as
`{"checks": [{"level", "check", "detail", "advice"}], "failures": n, "warnings": n}`.

Notes: when the daemon runs, `doctor` never opens the serial device; it asks the daemon.
`doctor` opens the database directly, and SQLite creates a missing file, so run it for the
first time **after** the service has started once.

## Where the logs are

| Log | Default location | Contents |
| --- | --- | --- |
| Journal | `journalctl -u thugsred-smsgateway` (or `make logs`) | Everything, as text, when `logging.journal_enabled` is true |
| System log | `/var/log/thugsred-smsgateway/system.log` (`make logs-system`) | Daemon, modem and API activity |
| Message log | `/var/log/thugsred-smsgateway/messages.log` (`make logs-messages`) | One entry per message lifecycle step (queued, submitted, received, failed, ...) and per call |
| Integration log | `/var/log/thugsred-smsgateway/integrations.log` | One entry per delivery attempt |
| Audit log | `/var/log/thugsred-smsgateway/audit.log` | Administrative actions, reloads, automation |

Files are JSON lines by default (`logging.format`), rotate daily (UTC) to
`<name>.<YYYY-MM-DD>`, and are kept for `logging.retention_days`. Numbers are masked and
message text is absent unless you changed the logging privacy settings.

More detail without a restart: set `general.log_level = "debug"` (or `"trace"` to see every
AT command and response line) and reload. `trace` output contains PDUs, and therefore message
content and numbers; switch it back afterwards. The SIM PIN is never logged.

Other sources: `thugsred-smsgateway modem info` (state, last error, hint),
`GET /api/v1/modem/health` (signal and registration history),
`GET /api/v1/messages/{id}/status` (per-message history), `thugsred-smsgateway diagnostics`
(redacted report for bug reports).

## The modem does not answer

| Symptom | Cause | Fix |
| --- | --- | --- |
| State `DISCONNECTED`, error `no response to AT commands (module powered off?)` | **The module is not powered on.** GSM modules such as the SIM868 do not start by themselves. | Hold **PWRKEY** for about two seconds until the NET LED blinks. The gateway connects on its own within about 20 seconds; no restart needed. |
| Same error, module is definitely on (LED blinking) | UART jumpers select the other interface (USB vs GPIO) | Set the jumpers to the interface in use; check the board documentation |
| Same error | Wrong baud rate | Set `modem.baud_rate` to the module's rate (115200 is common; many modules autobaud after a few `AT`s, which the gateway sends) |
| Same error | Wrong device (another serial adapter) | Check `modem.device` against `ls -l /dev/serial/by-path/` |
| Same error, on a Raspberry Pi GPIO UART | Serial port not enabled, or TX/RX not connected as the HAT expects | See [RASPBERRY-PI.md](RASPBERRY-PI.md#enabling-the-gpio-uart) |
| `modem stopped responding to AT commands` after working for a while | Module lost power or reset; three health checks in a row failed | Check the power supply; press PWRKEY if the module is off |
| `modem restarted` / `modem powered down (...)` in the log | The module announced a restart or power-down | Usually power supply or a deliberate power-off; the gateway reconnects when the module answers again |
| `modem rejected PDU mode (AT+CMGF=0)` | The modem does not support PDU mode | Not usable with this gateway |
| Modem connects but identification shows `?` or a wrong profile | Model not in any profile's list | Set `modem.profile`, or add a custom profile ([MODEMS.md](MODEMS.md#adding-a-custom-profile)) |
| Stays `DISCONNECTED` and never retries | `modem.auto_reconnect = false` | `thugsred-smsgateway modem reconnect` |

To watch the conversation: `general.log_level = "trace"`, reload, and follow `system.log`.

## Device missing, permission denied, or busy

| Symptom | Cause | Fix |
| --- | --- | --- |
| `cannot open /dev/...: No such file or directory`; hint "does not exist" | Module not attached, cable, or wrong name | `ls -l /dev/serial/by-path/ /dev/ttyUSB* /dev/ttyS0`; fix `modem.device` |
| Device name changes between boots (`ttyUSB0` ↔ `ttyUSB1`) | Kernel enumeration order | Use the `/dev/serial/by-path/...` name |
| `Permission denied`; hint "No permission to open" | Service user is not in the device's group | `ls -l <device>`; `sudo usermod -aG dialout thugsred-smsgateway`; restart the service |
| Hint "in use by another process" | Another program holds the port | See the rows below |
| Garbled or missing answers, random `ERROR`s, modem state flapping | **ModemManager** probes the port | Install the udev rule: `sudo cp /opt/THUGSred-smsGateway/scripts/99-thugsred-smsgateway.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger`, then replug or reboot; or `sudo systemctl disable --now ModemManager` |
| Same, on a GPIO UART; `doctor` reports "a login console (serial-getty) is running on this port" | **serial-getty** login console on the port | `sudo systemctl disable --now serial-getty@ttyS0.service` |
| `doctor` reports "the kernel console is attached to this port" | `console=ttyS0` (or `serial0`) on the kernel command line | Remove it; on a Raspberry Pi: `raspi-config` → Interface Options → Serial Port → no login shell, hardware enabled |
| A terminal program (`minicom`, `screen`) is open on the port | Two programs on one port | Close it. The gateway asks for exclusive access, so a second opener normally fails. |
| `... is used by more than one modem` at configuration check | Two enabled `[[modems]]` entries share a device | Give each its own device |

## SIM locked or PUK required

`thugsred-smsgateway modem info` shows the SIM state and a hint.

| State / message | Cause | Fix |
| --- | --- | --- |
| `SIM_LOCKED`, "SIM requires a PIN and none is configured" | The SIM asks for a PIN | Set `pin` and `pin_enabled = true` in `sim.conf` and reload; the modem reconnects on its own |
| `SIM_LOCKED`, "SIM PIN rejected" | The configured PIN is wrong | **Do not just retry.** Correct the PIN in `sim.conf`, run `modem reset-pin-counter`, then reload (the changed SIM profile makes the modem reconnect). Each attempt uses one of the SIM's remaining tries. |
| `SIM_LOCKED`, "SIM PIN was rejected earlier; not trying again" | The attempt counter has reached `pin_max_attempts`; the gateway protects the SIM | As above |
| `SIM_LOCKED`, "SIM is blocked and requires the PUK" | Too many wrong PINs | Put the SIM into a phone and unblock it with the PUK from your operator. The gateway never enters a PUK. |
| `DEGRADED`, "SIM not usable" | No SIM, badly seated, or not detected | Power the module off, reseat the SIM, power on |
| `DEGRADED`, "SIM did not become ready after PIN entry" | The SIM is slow after unlock | Nothing; it is re-checked with every health check |
| "cannot record PIN attempt; not entering PIN" | Database unavailable | Fix the database; the PIN is entered at a later check |
| PIN changed in `sim.conf` but nothing happens | No reload yet, the reload was rejected, or the PIN attempt counter is still at its limit | Reload and check `GET /api/v1/config/status`; run `modem reset-pin-counter` |

Simplest long-term fix when you do not need the PIN: disable the PIN request on the SIM
using a phone.

## Not registered, or roaming blocks sending

| Symptom | Cause | Fix |
| --- | --- | --- |
| State stays `REGISTERING`, registration `searching` or `not_registered` | No coverage, antenna missing or loose, wrong band | Attach and reposition the antenna; check `modem signal` |
| Same, with a 2G-only module | **No 2G network** from your operator at this location | A GSM/GPRS-only module cannot be used there; use an LTE module |
| Registration `denied` | SIM not activated, barred, or not allowed on this network | Check the subscription with the operator; test the SIM in a phone |
| Registration `unknown`, signal `null` | Modem does not report it | Check the antenna; look at `modem health` over time |
| Registered, weak signal (`doctor` warns below 30 %) | Antenna position | Move the antenna; avoid metal enclosures |
| State `READY`, registration `roaming`, messages stay `queued` with history "deferred: modem not ready: SIM is roaming and network.roaming_enabled is false in sim.conf" | Roaming is blocked by default | If roaming charges are acceptable: `roaming_enabled = true` in the SIM's network section and reload (the modem reconnects on its own) |
| Manual operator selection has no effect | `selection = "manual"` needs `operator_code` (numeric MCC+MNC) | Set both and reload (the modem reconnects on its own) |
| `preferred_network` ignored, warning "profile cannot select the preferred network type" | The profile has no `[network_modes]` | Use a profile that has, or add the command through a custom profile |

## Messages stuck in the queue

Look at the entry and its history:

```sh
curl -sS http://127.0.0.1:8080/api/v1/queue/<message id>
curl -sS http://127.0.0.1:8080/api/v1/messages/<message id>/status
```

| State | What it means | What to do |
| --- | --- | --- |
| `queued` | Waiting. Either no modem is `READY`, or the message was deferred; the history says why ("deferred: ..."). | See the causes below |
| `scheduled` | Waiting for `not_before` | Nothing; cancel if unwanted |
| `processing` | Being transmitted right now | Wait; a multipart message takes a while |
| `retrying` | The network or modem rejected it; next try at `next_attempt_at`; `last_error` holds the modem's error | Wait, or fix the cause (wrong SMSC, barred destination, no credit) |
| `failed` | Rejected `sms.max_send_attempts` times | Fix the cause, then `POST /api/v1/queue/{id}/retry` |
| `expired` | Not sent within `sms.queue_expiry_seconds` | Retry if it is still wanted |
| `submission_unknown` | **The PDU was handed to the modem, but no result came back.** It may or may not have been sent. The gateway does not resend it automatically, because that could deliver and bill it twice. | Find out whether it arrived (ask the recipient, check a later delivery report or the operator's records). If it did not, retry it; if it did, leave it. |
| `submitted` | The network accepted it; no delivery report (yet) | Normal when reports are not requested or not supported by the network |
| `delivery_failed` | A delivery report reported failure; `error` has the reason | Check the number; retry resends all parts |

Causes of `queued` that does not move:

| Cause | How to recognise | Fix |
| --- | --- | --- |
| Modem not `READY` | `status` | Fix the modem (sections above) |
| Message pinned to a modem that is down or disabled | `modem_id` set in the queue entry | Bring that modem up, or cancel and resend without `modem`/`from` |
| `sms.default_modem` set and that modem is down | Unpinned messages wait for it | Clear `default_modem` or fix the modem |
| Roaming blocked | History: "SIM is roaming ..." | See the previous section |
| SIM quota reached | History: "deferred: daily SMS limit ..." / "monthly SMS limit ..." | Wait for the next period, raise the limit, or cancel |
| Rate limit | Many messages, `sms.outbound_rate_limit_per_minute` segments per minute | Wait, or raise the limit |
| Per-destination interval | `next_attempt_at` set, `sms.per_destination_interval_seconds` > 0 | Wait |
| Sending disabled | `sms.enabled` or `sms.send_enabled` is false | Enable and reload |

Other outbound symptoms:

| Symptom | Cause | Fix |
| --- | --- | --- |
| `429 limit_reached` on submission | Queue full (`security.max_outbound_queue_size`) or quota exhausted for a pinned message | Clear the backlog; check why it is not draining |
| `403 forbidden` "destination is not on the allowlist" / "is blocked" | `security.destination_allowlist` / `destination_blocklist` | Adjust the lists |
| `400` "message needs N parts, limit is M" | Longer than `sms.max_message_parts` | Shorten, or raise the limit |
| `400` "no configured SIM has the gateway number" | `from` does not match the `mobile_number` of the SIM profile bound to any enabled modem | Fix `mobile_number` or use `modem` |
| Every submission fails with `+CMS ERROR: ...` | Network-side rejection (for example no service centre number, barred, no credit) | Check `GET /api/v1/sim` for the detected SMSC; set `smsc` in `sim.conf` if it is empty; test the SIM in a phone |
| After a restart, a message is `submission_unknown` | The daemon stopped while a part was being transmitted | Same decision as above |
| `504 timeout` from the raw PDU endpoint | The modem took longer than `api.request_timeout_seconds` to answer. The submission continues in the background and its outcome is always recorded. | Do not resend blindly: read the outcome from `GET /api/v1/messages?type=raw_pdu` or the message's status |

## Messages are not received

| Symptom | Cause | Fix |
| --- | --- | --- |
| Nothing arrives | Modem not registered; receiving disabled (`sms.enabled`, `sms.receive_enabled`) | `status`; enable and reload |
| Messages arrive only after up to 15 seconds | The modem accepted no `+CNMI` setting, so only the periodic poll finds them (log: "no +CNMI setting accepted") | Normal fallback; lower `modem.poll_interval_seconds` or add a suitable `cnmi` value in a custom profile |
| `modem.error` "modem SMS storage is nearly full" | Messages accumulate on the SIM | Keep `sms.delete_from_modem_after_persist = true`; check for a database problem that prevents deletion |
| `gateway.error` "inbound message could not be stored" | Database write failed; the PDU stays on the modem and is retried at the next poll | Fix the database |
| Message stored with status `malformed` | The PDU could not be decoded | The raw PDU is kept; decode it with `pdu decode` and please report it (redacted) |
| Message stored as `incomplete` with `[...]` | Parts of a multipart message never arrived within `sms.multipart_timeout_seconds` | Network-side loss; raise the timeout if parts arrive late |
| Text shows wrong characters | National language shift table that is not supported | See [PDU.md](PDU.md#limitations) |
| Message present in the API but not forwarded | Integration filter, routing rule, or `sms.allow_service_messages = false` for service messages | See the next section |

## Integration failures and dead letters

```sh
thugsred-smsgateway integrations list
thugsred-smsgateway integrations test <name>
curl -sS 'http://127.0.0.1:8080/api/v1/deliveries?integration=<name>&status=dead'
curl -sS http://127.0.0.1:8080/api/v1/dead-letters
sudo tail /var/log/thugsred-smsgateway/integrations.log
```

| Symptom | Cause | Fix |
| --- | --- | --- |
| Nothing is delivered, no deliveries listed | Integration not `enabled`; `integrations.enabled = false`; event not in `events`; filter does not pass; a routing rule matched and excluded it | Check each in turn. Remember: an allowlist never passes events without that value, and a matching routing rule restricts delivery to its destinations ([INTEGRATIONS.md](INTEGRATIONS.md#routing-rules)). |
| Deliveries `dead` with `HTTP status 404` / `401` / `403` | Wrong or revoked webhook URL or token | Fix the secret, restart or reload, then replay |
| Deliveries `dead` with `HTTP status 400` | The receiver rejects the payload | Check the template or body template; run `integrations test` |
| Deliveries `dead` with `HTTP status 301` / `302` / `307` | The URL redirects; redirects are not followed | Use the final URL |
| Deliveries `retrying` with `HTTP status 429` | Rate limited by the service | Set `rate_limit_per_minute` on the integration |
| `connection failed` / `request timed out` | Network, DNS, firewall, or the receiver is down | Test from the host with `curl`; raise `timeout_seconds` if the receiver is slow |
| TLS errors with an internal certificate authority | Only publicly trusted roots are accepted | Use a publicly trusted certificate, or `verify_tls = false` on a trusted network |
| `integration is disabled or no longer configured` | It was disabled or removed while deliveries were waiting | Expected; re-enable and replay if wanted |
| `discarded at startup (persistent_queue disabled)` | `integrations.persistent_queue = false` | Expected with that setting |
| Receiver rejects the signature | Verifying against re-serialised JSON, wrong secret, or a timestamp tolerance shorter than the retry schedule | Verify the raw body; see [INTEGRATIONS.md](INTEGRATIONS.md#webhook-signatures) |
| Receiver sees duplicates | At-least-once delivery | Deduplicate by event id |
| Configuration error "environment variable X is not set" for an integration | The secret is missing from `smsgateway.env`, or you are not root | Add it; restart the service (systemd reads the file at start) |
| Changed a secret in `smsgateway.env`, reload did not help | The service's environment is fixed at start | `sudo systemctl restart thugsred-smsgateway` |

Replay after fixing the cause:

```sh
curl -sS -X POST http://127.0.0.1:8080/api/v1/deliveries/<delivery id>/replay
```

Dead letters of kind `sms` are outbound messages that failed for good; retry those with
`POST /api/v1/queue/{id}/retry`.

## The service does not start

`sudo journalctl -u thugsred-smsgateway -n 50` shows the reason.

| Message | Cause | Fix |
| --- | --- | --- |
| `cannot listen on 0.0.0.0:8080: Address already in use` | **The port is taken** by another program (or another instance) | `sudo ss -ltnp 'sport = :8080'` to find it; change `api.port` or stop the other program |
| `cannot listen on ...: Cannot assign requested address` | `api.bind` is not an address of this host | Fix `api.bind` |
| `cannot listen on ...: Permission denied` | A port below 1024; the service has no capabilities | Use a port above 1023 and a reverse proxy |
| `configuration is invalid:` followed by a list | Configuration errors | Fix every listed field; `check-config` |
| `...: cannot read file: Permission denied` | Configuration not readable by the service user | `sudo chown root:thugsred-smsgateway /etc/thugsred/*.conf; sudo chmod 640 /etc/thugsred/*.conf` |
| `environment variable X is not set ...` | A `${X}` reference without a value in `smsgateway.env` | Add it to `/etc/thugsred/smsgateway.env` |
| `database schema is at version N but version M is required` | `database.auto_migrate = false` and migrations are pending | `thugsred-smsgateway db migrate` |
| Database errors at start | See [Database problems](#database-problems) | |
| `warning: cannot open log file ...; this log is disabled` | Log directory not writable | Fix ownership of `/var/log/thugsred-smsgateway`; the daemon runs without that file |
| Service restarts every two minutes with a watchdog message from systemd | The database stopped answering, so liveness is not reported | Fix the database |
| `start-limit-hit` | Five failed starts in five minutes | Fix the cause, then `sudo systemctl reset-failed thugsred-smsgateway` |
| Read-only file system errors for a path you moved | The sandbox only allows the default data paths | Add `ReadWritePaths=` in a drop-in ([INSTALLATION.md](INSTALLATION.md#the-systemd-unit)) |

A missing or unpowered modem never prevents the service from starting: the API comes up and
the modem is retried in the background.

## Configuration reload rejected

| Symptom | Cause | Fix |
| --- | --- | --- |
| `thugsred-smsgateway reload` prints the list of errors (HTTP `422`, `invalid_configuration`) | The new configuration is invalid | Fix the listed fields. **The previous configuration is still active**; nothing was half-applied. |
| `systemctl reload` succeeds but nothing changed | SIGHUP always "succeeds" for systemd; the rejection is in the log | `journalctl -u thugsred-smsgateway -n 20`, or `GET /api/v1/config/status` (`last_reload_error`) |
| Reload accepted, a setting has no effect, `restart required for: ...` is printed | The setting only applies after a restart | `sudo systemctl restart thugsred-smsgateway` |
| Reload accepted, a changed modem profile file has no effect | Profile files are read at startup only and are not part of the restart report | Restart |
| SIM PIN, SMSC, roaming or operator change has no effect | Applied when the modem connects; a reload that changes the bound SIM profile reconnects the modem within a few seconds | Check that the reload was accepted and that the modem is bound to the profile you edited (`modem info`) |
| New value in `smsgateway.env` not picked up | systemd reads environment files at start only | Restart |
| A warning about an unknown key | Typo in a key or section name; the setting was ignored | Correct it |

Validate before reloading: `thugsred-smsgateway check-config`.

## API and dashboard access

| Symptom | Cause | Fix |
| --- | --- | --- |
| `403` "access from this address is not allowed (api.allowed_cidrs)" | Client address outside `allowed_cidrs`. Behind a proxy without `trusted_proxies`, the client is the proxy. | Add the network, or set `trusted_proxies` |
| `421 invalid_host` | `Host` header not in `allowed_hosts` | Add the name or address clients use (with or without port) |
| `403` "cross-origin request rejected" from the dashboard behind a proxy | The proxy rewrites `Host`, so `Origin` no longer matches | Pass the original header: `proxy_set_header Host $http_host;` |
| `403` "cross-origin request rejected" from another web application | CORS is off | `cors_enabled = true` and list the origin |
| `401 unauthorized` | Missing or wrong token; or an administrative endpoint needs an admin token | `Authorization: Bearer <token>` |
| `403` "administrative endpoints are disabled / not available from this address" | `admin_enabled = false` or `admin_allowed_cidrs` | Adjust |
| `429 rate_limited` | More than `rate_limit_per_minute` requests from one address (all clients share one address behind a proxy without `trusted_proxies`) | Raise the limit; set `trusted_proxies` |
| `415 unsupported_media_type` | Body not sent as JSON | `-H 'Content-Type: application/json'` |
| `400` "unknown field" on send | Typo in a field name; send requests reject unknown fields | Fix the name |
| `504 timeout` on USSD or raw PDU | The modem took longer than `api.request_timeout_seconds` | Raise it (restart needed) |
| Dashboard does not update live behind a proxy | Proxy buffers the event stream | Disable buffering for `/api/v1/events/stream`; the dashboard falls back to polling meanwhile |
| Dashboard not found (`404` on `/`) | `web.enabled = false` | Enable and restart |
| `/metrics` returns `401` | Bearer authentication applies to metrics | Configure the scraper with a token |

## Command-line tool problems

| Symptom | Cause | Fix |
| --- | --- | --- |
| `command not found` | The binary is not on `PATH` | `/opt/THUGSred-smsGateway/bin/thugsred-smsgateway`, or create a symbolic link |
| `/etc/thugsred/smsgateway.conf: cannot read file: Permission denied` | Configuration is readable by root and the service group only | Use `sudo` |
| `environment variable X is not set (and not found in smsgateway.env, which may need root to read)` | `${X}` is resolved from the environment and from `smsgateway.env`, which only root can read | Use `sudo`; check that the variable is in the file |
| `cannot reach the gateway daemon at http://127.0.0.1:8080 ...` | Service not running, `api.enabled = false`, or the API listens elsewhere | `systemctl status thugsred-smsgateway`; `--api-url` for a non-standard address |
| `... (403 Forbidden)` from the tool itself | `127.0.0.1` is not in `allowed_cidrs`, or `allowed_hosts` does not include `127.0.0.1` / `127.0.0.1:<port>` | Add them, or use `--api-url` with an accepted name |
| `... (401 Unauthorized)` | The configuration the tool loaded has no (valid) token | The tool uses the first admin token, else the first API token, from the configuration |
| Database owned by root after running `doctor` or `db` commands before the first service start | SQLite created the file as root | `sudo chown thugsred-smsgateway: /var/lib/thugsred-smsgateway/messages.db*` |

## Database problems

| Symptom | Cause | Fix |
| --- | --- | --- |
| `unable to open database file` (SQLite) | Directory missing or not writable by the service user | Check `/var/lib/thugsred-smsgateway` ownership (`thugsred-smsgateway`, mode `0750`) |
| `attempt to write a readonly database` | The database file, or its `-wal`/`-shm` companions, belong to root | `sudo chown thugsred-smsgateway: /var/lib/thugsred-smsgateway/messages.db*` |
| `database is locked` | Another process holds the file for a long time (a copy tool, a manual `sqlite3` session) | Do not open the live database with other tools for writing; use `db backup` for copies |
| `db verify` reports `FAILED` | SQLite integrity check failed | Stop the service, keep the damaged file, restore the latest verified backup ([BACKUP-RESTORE.md](BACKUP-RESTORE.md)) |
| `/ready` returns `503`; status shows the database as unavailable | Database unreachable | Fix it; the API keeps answering, and systemd restarts the daemon when the watchdog expires |
| MySQL/MariaDB: connection refused, access denied, unknown database | URL, credentials, grants, firewall | Test with the `mysql` client from the gateway host using the same values |
| MySQL/MariaDB: `database.mysql_url: must be set to a mysql:// URL` | Empty or unresolved `${SMSGW_MYSQL_URL}` | Set it in `smsgateway.env` |
| `built-in backup supports SQLite only` | `db backup` on MySQL/MariaDB | Use `mysqldump` |
| Schema warning in `doctor` | Pending migrations | `db migrate` or restart with `auto_migrate = true` |
| Disk filling up | Events and health samples; recordings | Check `GET /api/v1/stats` (`tables`); lower the `[retention]` periods |
| `gateway.error` "low disk space" | Less than 200 MiB or 2 % free on the data filesystem | Free space; lower retention |

## Calls and recording

| Symptom | Cause | Fix |
| --- | --- | --- |
| Calls are not logged | `calls.enabled` or `calls.log_incoming` false | Enable and reload |
| Calls logged without a number | The network or caller withholds caller ID, or the modem rejected `AT+CLIP=1` | Nothing to fix on the gateway |
| `recording_status` `unavailable` | No `calls.audio_device`, `audio_backend = "none"`, or `arecord` not installed | Configure a capture device; `sudo apt install alsa-utils` |
| `recording_status` `failed` | `arecord` failed or captured nothing (wrong device, no permission, nothing wired) | Test as the service user: `sudo -u thugsred-smsgateway arecord -D <device> -f S16_LE -r 8000 -c 1 -d 3 /dev/null`; check the `audio` group |
| A recording exists but is silent | The capture device is not wired to the modem's audio output | The AT serial line carries no audio; see [CALLS.md](CALLS.md#recording-requirements) |
| Recording enabled but never starts | `calls.auto_answer` is false | Recording needs auto-answer |

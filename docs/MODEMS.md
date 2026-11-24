# Modems

- [Compatibility matrix](#compatibility-matrix)
- [Modem profiles](#modem-profiles)
- [Profile fields](#profile-fields)
- [Adding a custom profile](#adding-a-custom-profile)
- [Capability detection](#capability-detection)
- [AT commands the gateway uses](#at-commands-the-gateway-uses)
- [Power-on and the PWRKEY hint](#power-on-and-the-pwrkey-hint)
- [ModemManager and serial consoles](#modemmanager-and-serial-consoles)
- [Multi-modem setup](#multi-modem-setup)
- [SIM binding](#sim-binding)
- [SIM PIN and PUK policy](#sim-pin-and-puk-policy)
- [Roaming](#roaming)
- [MMS notes](#mms-notes)
- [GNSS notes](#gnss-notes)
- [The simulator](#the-simulator)

## Compatibility matrix

Only one physical module has been checked. Everything else in this table is either covered by
the automated tests against the software simulator, or not tested at all.

Hardware checked: Waveshare GSM/GPRS/GNSS HAT with a SIMCom SIM868 (firmware
`1418B05SIM868M32_BT`), attached through its USB port (CP2102 USB-UART bridge) to an x86_64
Debian host. On that module the gateway also ran as the installed systemd service, with a
MariaDB 10.11 database and behind an nginx reverse proxy, and exchanged SMS over a real
mobile network as far as the table says.

| Function | SIM868 (real module) | Simulator | SIMCom LTE, Quectel, other modems |
| --- | --- | --- | --- |
| Serial connection and AT handshake | verified | tested | unverified |
| Identification (manufacturer, model, firmware) | verified | tested | unverified |
| Profile auto-detection | verified (`simcom-2g`) | tested | unverified (detection rules are unit-tested with sample identification strings) |
| Capability detection | verified | tested | unverified |
| SIM ready detection | verified | tested | unverified |
| SIM PIN entry | unverified | tested | unverified |
| Network registration, operator, signal | verified | tested | unverified |
| SMS storage query | verified | tested | unverified |
| "Module powered off" hint | verified | tested | unverified |
| Receiving SMS, single-part GSM 7-bit (including national characters å ø æ), with persist-then-delete from SIM storage | verified | tested | unverified |
| Sending SMS, single-part GSM 7-bit (from the dashboard) | verified | tested | unverified |
| Delivery report reception and correlation (for that one message) | verified | tested | unverified |
| UCS-2, multipart, binary and flash messages | unverified | tested | unverified |
| USSD | unverified | tested | unverified |
| Call detection, auto-answer | unverified | tested | unverified |
| Call recording | unverified | not applicable (needs audio hardware) | unverified |
| GNSS | receiver power-on command accepted and capability reported as `supported`; no position fix obtained, positions unverified | parsing tested | unverified |
| MMS notification decoding | unverified | tested | unverified |
| MMS retrieval | unverified | request validation tested | unverified |
| Preferred network selection | not applicable (2G only) | not applicable | unverified |
| More than one physical modem | unverified | tested with several simulated modems | unverified |
| ARM64 / Raspberry Pi at runtime | unverified | unverified | unverified |
| Raspberry Pi GPIO UART (`/dev/ttyS0`) | unverified | not applicable | unverified |

Requirements for any modem: an AT command serial port, PDU mode (`AT+CMGF=0`), and the
standard SMS commands of 3GPP TS 27.005. A modem that rejects `AT+CMGF=0` cannot be used; the
gateway reports "modem rejected PDU mode".

## Modem profiles

A profile is a small TOML document that describes vendor-specific behaviour. Four profiles
are compiled into the binary:

| Name | Model substrings matched | Initial `+CNMI` settings tried | ICCID commands | GNSS | Network mode commands |
| --- | --- | --- | --- | --- | --- |
| `generic` | none (fallback) | `2,1,0,1,0`, `2,1,0,0,0`, `1,1,0,0,0` | `AT+CCID`, `AT+ICCID`, `AT+QCCID` | none | none |
| `simcom-2g` | `SIM800`, `SIM868`, `SIM808`, `SIM900` | `2,1,0,1,0`, `2,1,0,0,0` | `AT+CCID` | `simcom_cgns` | none |
| `simcom-lte` | `SIM7600`, `SIM7500`, `A7600`, `A7670`, `A7672`, `SIM7000`, `SIM7070`, `SIM7080` | `2,1,0,1,0`, `2,1,0,0,0` | `AT+CICCID`, `AT+CCID` | `simcom_cgps` | `AT+CNMP=2` / `13` / `14` / `38` |
| `quectel` | `EC2`, `EG2`, `EM0`, `BG9`, `M95`, `M66`, `MC60`, `UC20`, `EG9` | `2,1,0,1,0`, `2,1,0,0,0` | `AT+QCCID`, `AT+CCID` | `quectel_qgps` | `AT+QCFG="nwscanmode",0` / `1` / `2` / `3` |

**Selection.** With `modem.profile = "auto"` (the default) the gateway reads `AT+CGMI` and
`AT+CGMM` and picks:

1. the first profile one of whose `models` entries is a case-insensitive substring of the
   model identification;
2. otherwise, for an unknown model, the profile whose `manufacturers` entry is a substring of
   the manufacturer identification, **when exactly one profile matches**;
3. otherwise `generic`.

With the built-in profiles this means: an unlisted Quectel model gets the `quectel` profile,
because only one profile names that manufacturer. An unlisted SIMCom model gets `generic`,
because two profiles (`simcom-2g` and `simcom-lte`) name SIMCom and the gateway does not
guess between them; set `modem.profile` explicitly, or add a custom profile, for such
modules. A configured profile name that does not exist falls back to auto-detection.

The selected profile is shown as `profile` in `modem info` and `GET /api/v1/modem`, and the
available names in `GET /api/v1/capabilities` (`gateway.modem_profiles`).

## Profile fields

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | `"generic"` | Profile name. A directory profile with the name of a built-in one replaces it. |
| `description` | string | empty | Free text |
| `manufacturers` | list of strings | `[]` | Case-insensitive substrings of the manufacturer identification; used for models that no profile lists (see the selection rules) |
| `models` | list of strings | `[]` | Case-insensitive substrings of the model identification |
| `init_commands` | list of strings | `[]` | AT commands sent after capability detection, before `modem.init_commands` |
| `cnmi` | list of strings | `["2,1,0,1,0", "2,1,0,0,0", "1,1,0,0,0"]` | `AT+CNMI=` parameter sets tried in order until the modem accepts one. If none is accepted the gateway relies on periodic polling. |
| `iccid_commands` | list of strings | `["AT+CCID"]` | Commands tried in order to read the ICCID |
| `network_technologies` | list of strings | `[]` | Reported as the modem's technologies (`GSM`, `GPRS`, `UMTS`, `LTE`). `LTE` is added automatically when the modem answers `AT+CEREG?`. |
| `voice` | string | `"unknown"` | `supported`, `unsupported`, `unverified` or `unknown`; reported as the `voice` capability |
| `audio_interfaces` | list of strings | `[]` | Descriptive list of the family's audio paths |
| `mms_test_command` | string | empty | Test command for the modem's own MMS command set. If accepted, the `mms` capability is reported as `unverified`. |
| `bluetooth_test_command` | string | empty | Test command for Bluetooth support |
| `sms_storage` | string | empty | Preferred SMS storage (for example `"SM"`); empty keeps the modem default. `modem.sms_storage` takes precedence. |
| `power_on_hint` | string | empty | Descriptive text about powering the module on |
| `[gnss] kind` | string | empty | `simcom_cgns`, `simcom_cgps`, `quectel_qgps`, or empty for none |
| `[gnss] test_command` | string | empty | Test command used for the `gnss` capability |
| `[network_modes]` | table | empty | Maps `auto`, `2g`, `3g`, `4g` (the values of `modem.preferred_network`) to the AT command that selects the mode |

Commands in `init_commands` that the modem rejects are logged as warnings and do not stop
initialisation.

## Adding a custom profile

Put a `*.toml` file into `/etc/thugsred/modem-profiles/` (`paths.modem_profiles`). Files are
read in name order at startup; a profile whose `name` equals a built-in one replaces it,
other names are added. Files that cannot be parsed, or have an empty name, produce a warning
in the system log and are skipped. Profile changes need a restart of the service.

```toml
# /etc/thugsred/modem-profiles/acme-x1.toml
name = "acme-x1"
description = "Example profile for a hypothetical modem"
manufacturers = ["ACME"]
models = ["X1"]
init_commands = []
cnmi = ["2,1,0,1,0", "2,1,0,0,0"]
iccid_commands = ["AT+CCID"]
network_technologies = ["GSM", "LTE"]
voice = "unverified"
audio_interfaces = []
sms_storage = "SM"

[network_modes]
auto = "AT+EXAMPLE=0"
4g = "AT+EXAMPLE=3"
```

Then select it explicitly or rely on detection:

```toml
[modem]
profile = "acme-x1"      # or "auto"
```

The built-in profiles are installed for reference in
`/opt/THUGSred-smsGateway/config/modem-profiles/`. Copy one into
`/etc/thugsred/modem-profiles/` to change it.

GNSS support is limited to the three `kind` values above, because each needs its own response
parser in the gateway.

## Capability detection

With `modem.auto_detect_capabilities = true` (the default) the gateway probes the modem after
identification. Values are `supported`, `unsupported`, `unverified` or `unknown`.

| Capability (API field) | Source |
| --- | --- |
| `sms_modes` | `AT+CMGF=?` (`pdu`, `text`) |
| `character_sets` | `AT+CSCS=?` |
| `baud_rates` | `AT+IPR=?` |
| `sms_storages` | `AT+CPMS=?` |
| `delivery_report_routing` | `AT+CNMI=?` (fourth parameter allows 1 or 2) |
| `caller_id` | `AT+CLIP=?` accepted |
| `ussd` | `AT+CUSD=?` accepted |
| `gnss` | The profile's `[gnss] test_command` accepted; `unverified` when not probed but the profile names a GNSS kind |
| `mms` | The profile's `mms_test_command`: `unverified` when accepted, `unsupported` when rejected. Never `supported`, because retrieval through the modem is not implemented. |
| `bluetooth` | The profile's `bluetooth_test_command` |
| `network_technologies` | Profile, plus `LTE` when `AT+CEREG?` is answered |
| `voice` | Profile; raised from `unknown` to `unverified` when caller ID is supported |
| `audio.modem_interfaces` | Profile |
| `audio.host_capture_devices` | ALSA capture devices found in `/proc/asound/pcm` on the host |
| `audio.call_recording` | `unsupported` when the host has no capture device, otherwise `unverified` |

With detection disabled only the profile-declared values are reported and `detected` is
`false`.

View the result with `thugsred-smsgateway capabilities`, `modem info` or
`GET /api/v1/capabilities`.

## AT commands the gateway uses

Initialisation (per connection):

| Step | Commands |
| --- | --- |
| Handshake | `AT` (up to five attempts of 1.2 s, for autobauding modems) |
| Basic setup | `ATE0`, `AT+CMEE=1` |
| Identification | `AT+CGMI`, `AT+CGMM`, `AT+CGMR`, `AT+CGSN` |
| Capabilities | See the table above |
| Profile and user commands | Profile `init_commands`, then `modem.init_commands` |
| Network mode | The profile's `[network_modes]` command for `modem.preferred_network` |
| SIM identity | The profile's `iccid_commands` |
| SIM state | `AT+CPIN?`, and at most once `AT+CPIN="<pin>"` |
| SMS setup (SIM ready) | `AT+CMGF=0`, `AT+CPMS="<s>","<s>","<s>"` (when a storage is configured), `AT+CSCA="<smsc>",<type>` (when `smsc` is set in the SIM profile), `AT+CNMI=<profile setting>` |
| Notifications | `AT+CLIP=1`, `AT+CREG=1`, `AT+CGREG=1`, `AT+CUSD=1` (when USSD is enabled) |
| Manual operator | `AT+COPS=1,2,"<code>"` (when `network.selection = "manual"`) |
| Subscriber data | `AT+CIMI`, `AT+CNUM`, `AT+CSCA?` |
| GNSS power | `AT+CGNSPWR=1`, `AT+CGPS=1` or `AT+QGPS=1` (only when `[gnss]` is enabled for this modem) |

While connected:

| Purpose | Commands |
| --- | --- |
| Health check | `AT`, `AT+CSQ` |
| Registration | `AT+CREG?`, `AT+CEREG?` (LTE-capable modems), `AT+COPS?` |
| Stored messages | `AT+CPMS?`, `AT+CMGL=4`, `AT+CMGR=<index>`, `AT+CMGD=<index>` |
| Sending | `AT+CMGS=<length>` followed by the PDU and Ctrl-Z |
| USSD | `AT+CUSD=1,"<code>",15`, `AT+CUSD=2` |
| Calls | `ATA`, `ATH` |
| GNSS | `AT+CGNSINF`, `AT+CGPSINFO` or `AT+QGPSLOC=2` |

Unsolicited result codes that are acted on: `+CMTI`, `+CDSI`, `+CMT`, `+CDS`, `+CREG`,
`+CGREG`, `+CEREG`, `RING`, `+CRING`, `+CLIP`, `NO CARRIER`, `BUSY`, `NO ANSWER`,
`NO DIALTONE`, `+CUSD`, `+CPIN`, `RDY` (modem restarted) and power-down notices.

Commands are executed strictly one at a time. A command that gets no final result within
`modem.command_timeout_seconds` fails; submissions use `modem.message_timeout_seconds`.

## Power-on and the PWRKEY hint

GSM modules such as the SIM868 are not powered on automatically. After attaching the module,
hold its PWRKEY button for about two seconds until the NET LED blinks.

When the serial device opens but the modem does not answer `AT`, the gateway sets:

- `state`: `DISCONNECTED`
- `last_error`: `no response to AT commands (module powered off?)`
- `hint`: an explanation that names PWRKEY, the baud rate and, for Waveshare HATs, the UART
  jumpers, and states that the gateway keeps retrying

and emits one `modem.error` event carrying the same hint. The hint is shown by `status`,
`modem info`, `doctor`, the API and the dashboard. With the default settings the gateway
probes again after 5, 10 and then every 20 seconds, and connects without a restart as soon as
the module answers. The same happens when a connected module stops answering three health
checks in a row or announces a power-down.

`doctor`, when the daemon is not running, opens the serial device itself and reports "no
answer to AT" with the same advice.

## ModemManager and serial consoles

Only one program can sensibly talk to a modem's AT port. The gateway requests exclusive
access to the device, and `doctor` checks for the usual competitors:

| Competitor | Detection | Fix |
| --- | --- | --- |
| ModemManager | `ModemManager.service` is active (warning) | Install the udev rule below, or stop ModemManager if nothing else needs it |
| Serial login console | `serial-getty@<tty>.service` is active on the port (failure) | `sudo systemctl disable --now serial-getty@<tty>.service` |
| Kernel console | `console=<tty>` on the kernel command line (failure) | Remove the entry (on a Raspberry Pi with `raspi-config`) |

The udev rule [scripts/99-thugsred-smsgateway.rules](../scripts/99-thugsred-smsgateway.rules)
sets `ID_MM_DEVICE_IGNORE` for three common USB-UART bridges: Silicon Labs CP210x
(`10c4:ea60`), QinHeng CH340/CH341 (`1a86:7523`) and FTDI FT232 (`0403:6001`). It is optional
and not installed automatically:

```sh
sudo cp /opt/THUGSred-smsGateway/scripts/99-thugsred-smsgateway.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger
```

Remove the lines for adapters that ModemManager should keep managing on the machine.

## Multi-modem setup

Each `[[modems]]` entry in `smsgateway.conf` describes one module. It inherits every setting
of the `[modem]` table except `id` and `sim`, and may override any of them. Without an `id`
the entries are named `modem-1`, `modem-2`, ... in file order.

```toml
[modem]
baud_rate = 115200

[[modems]]
id = "modem-1"
device = "/dev/serial/by-path/pci-0000:00:14.0-usb-0:1:1.0-port0"
sim = "sim-1"

[[modems]]
id = "modem-2"
device = "/dev/serial/by-path/pci-0000:00:14.0-usb-0:2:1.0-port0"
sim = "sim-2"
```

**Use by-path device names.** `/dev/ttyUSB0` and `/dev/ttyUSB1` are assigned in detection
order and can swap after a reboot. The names under `/dev/serial/by-id/` often do not help
either, because inexpensive USB-UART bridges frequently share the same serial number. The
names under `/dev/serial/by-path/` identify the physical USB port:

```sh
ls -l /dev/serial/by-path/
```

Rules enforced by validation:

- modem ids are unique and contain only letters, digits, `-` and `_`;
- two enabled modems may not use the same device;
- `sim` must name a profile defined in `sim.conf`;
- `sms.default_modem` and `gnss.modem` must name configured modems.

Behaviour:

- Every modem has its own supervisor and sender worker; one failing modem does not stop the
  others.
- A message that names a modem (`modem`) or a gateway number (`from`) is sent only through
  that modem. Other messages are taken by any modem that is `READY`, or only by
  `sms.default_modem` when that is set.
- API calls and commands that take a modem default to the first configured modem.
- Adding or removing `[[modems]]` entries needs a restart. Changing the settings of an
  existing modem is applied on reload by reconnecting that modem.

Multi-modem operation has been tested with simulated modems only.

## SIM binding

A SIM profile from `sim.conf` is bound to a modem in this order:

1. **By ICCID.** If the ICCID read from the SIM equals the `iccid` of a SIM profile, that
   profile is used, whichever modem the SIM sits in.
2. **By `sim =`** on the modem entry.
3. **Implicitly**, when exactly one modem and exactly one SIM profile are configured.

The bound profile supplies the PIN, SMSC, network selection and roaming policy for the
connection and is shown as `sim.profile` in `modem info`. The binding made at
initialisation is recorded, and quotas (`[limits]`), the `from` selector and the sender
stored with outbound messages use that recorded binding, whether it was made by `sim =` or
by ICCID. Before a modem has completed its first initialisation, the configured binding
(rules 2 and 3) is used.

Recommendation for several modems: set `sim =` on each `[[modems]]` entry, and add `iccid`
if you want the gateway to follow a SIM that is moved.

The classic single-SIM layout (`[sim]`, `[network]`, `[identity]`, `[limits]`) defines a
profile with the id `default`. `[[sims]]` entries without an `id` are named `sim-1`, `sim-2`,
... in file order.

`mobile_number` is descriptive metadata: it is shown in the dashboard, stored as the sender of
outbound and the recipient of inbound messages, and used by `from`. It does not change the
number recipients see.

PIN, SMSC, network selection and roaming settings are applied when a modem connects. A
reload that changes the SIM profile bound to a connected modem is detected within a few
seconds and makes that modem reconnect on its own.

## SIM PIN and PUK policy

Entering a wrong PIN three times blocks a SIM. The gateway is deliberately conservative:

| Situation | Behaviour |
| --- | --- |
| SIM reports `READY` | Nothing to do |
| SIM asks for a PIN, none configured (`pin_enabled = false` or empty `pin`) | State `SIM_LOCKED`, SIM state `pin_required`; no attempt is made |
| SIM asks for a PIN, PIN configured, attempt counter below `pin_max_attempts` | The counter is incremented and stored in the database **before** the PIN is sent, then the PIN is entered once. On success the counter is reset to 0. |
| The PIN is rejected | State `SIM_LOCKED`. No further attempt is made. |
| Attempt counter has reached `pin_max_attempts` | No attempt; the hint says to correct the PIN and clear the counter |
| The attempt cannot be recorded (database unavailable) | The PIN is not entered |
| SIM asks for a PUK (`SIM PUK`, `SIM PUK2`) | State `SIM_LOCKED`, SIM state `puk_required`. **The gateway never enters a PUK.** Unblock the SIM in a phone. |
| No SIM, or an unknown SIM state | State `DEGRADED`, SIM state `missing` or `error`; re-checked with every health check |

Details:

- `pin_max_attempts` is 1 by default and may be 2 at most.
- The counter is kept per modem id and ICCID, survives restarts, and is not reset by a
  reload.
- The PIN is never written to logs; the command is logged as redacted.
- A `sim.locked` event is emitted when the SIM state changes to a locked or unusable one.

After correcting the PIN in `sim.conf`:

```sh
thugsred-smsgateway modem reset-pin-counter        # optionally --modem <id>
sudo systemctl reload thugsred-smsgateway          # the changed SIM profile reconnects the modem
```

`reset-pin-counter` (an administrative API call, `POST /api/v1/modem/reset-pin-counter`)
permits exactly one new attempt. Make sure the PIN is right first: you are spending one of
the SIM's remaining tries.

This policy has been tested against the simulator, not with a PIN-locked SIM in real
hardware.

## Roaming

`roaming_enabled` in the SIM profile's network section defaults to `false`. While the modem
is registered as `roaming` and the bound SIM profile does not allow roaming, nothing is
sent: the modem reports "SIM is roaming and network.roaming_enabled is false in sim.conf",
and the message returns to the queue and is tried again every 10 seconds without using up
send attempts, until it is sent or expires. Receiving is not affected.

## MMS notes

- **Notifications** arrive as WAP Push SMS (destination port 2948 or 2949). The gateway
  decodes M-Notification.ind and stores the message with type `mms_notification`; the sender,
  subject, size, expiry and content location are kept in the message metadata. Pushes that
  are not MMS notifications are stored as `wap_push`.
- **Retrieval is experimental and off by default.** With `mms.enabled = true` and
  `mms.download_attachments = true`, the gateway fetches the announced content with an HTTP
  GET **from the host's own network stack** and stores the raw MMS PDU as an attachment
  (`application/vnd.wap.mms-message`) without interpreting it. Carrier MMSCs are normally
  reachable only through the carrier's mobile data connection, so this works only if the
  host itself has an IP route to the MMSC.
- Retrieval **through the modem's own data stack is not implemented.** That is why the `mms`
  capability is never reported as `supported`. `mms.apn` and `mms.provider_profile` exist in
  the configuration but are not used by this release.
- Safety limits: only the host named in `mms.mmsc_url` is ever contacted, URLs with
  credentials are refused, redirects are not followed, and downloads larger than
  `mms.max_attachment_size_mb` are rejected.
- **Sending MMS is not supported.**

## GNSS notes

GNSS is off by default. With `[gnss] enabled = true` the gateway powers the receiver on when
the selected modem connects (`gnss.modem`, or the first modem) and polls the position every
`gnss.poll_interval_seconds`. It requires a profile with a GNSS `kind`.

- The current position is available from `GET /api/v1/gnss`; a history is kept only with
  `store_location_history = true`.
- The `gnss.fix` event is emitted once, at the first fix after the daemon started, and
  carries only the satellite count. Coordinates are never part of an event and are
  therefore never forwarded to integrations.
- The receiver is powered on as part of the SMS setup, which runs once the SIM is ready.
  Enabling GNSS takes effect at the next connection of the modem (`modem reconnect`).
- On the one real module checked, the power-on command was accepted and the capability was
  reported as `supported`, but no position fix was obtained. Position reporting is therefore
  unverified on real hardware.

## The simulator

The software modem emulates a SIMCom-style GSM module (it identifies as a SIM868). It answers
the AT commands the gateway uses, stores and lists messages, accepts PDU submissions and can
inject received SMS, delivery reports, calls, disconnections and failures. The automated
tests use it in-process.

**Whole gateway with simulated modems:**

```sh
thugsred-smsgateway --config <your.conf> run --simulate-modem
```

Every configured modem gets its own independent simulated modem; no serial device is opened
and no real SMS is sent or received. The log says so at startup.

**Simulator on a pseudo-terminal**, for running the unmodified serial code path:

```sh
thugsred-smsgateway simulator --link /path/to/ttySMSGW
# in another terminal
thugsred-smsgateway --config <your.conf> run --set modem.device=/path/to/ttySMSGW
```

`make simulator` runs the same command from a source checkout with the link
`/tmp/ttySMSGW`. Commands typed into the simulator:

| Command | Effect |
| --- | --- |
| `sms <sender> <text>` | Deliver an SMS; long texts become multipart |
| `call <number>` | Incoming call |
| `hangup` | End the call |
| `reg <0-5>` | Set the network registration status |
| `fail <cms code>` | Reject the next submissions with that `+CMS ERROR` |
| `silent` | Accept the PDU of the next submissions and then stay silent (produces `submission_unknown`) |
| `ok` | Back to normal submissions |
| `power off` / `power on` | Simulate an unpowered module |
| `drop` | Drop the serial connection |
| `sent` | List the submitted PDUs |
| `quit` | Exit |

See [DEVELOPMENT.md](DEVELOPMENT.md#using-the-simulator) for a complete development setup.

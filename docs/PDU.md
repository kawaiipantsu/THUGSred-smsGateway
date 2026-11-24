# PDU codec and tools

The gateway works in SMS **PDU mode** only. Its codec implements the parts of 3GPP TS 23.040
(SMS transfer protocol) and 3GPP TS 23.038 (alphabets) that are needed to send and receive
ordinary messages. The codec is pure: it never talks to a modem, treats every input as
untrusted and checks every length before reading.

- [What is supported](#what-is-supported)
- [PDU layout](#pdu-layout)
- [Encoding selection](#encoding-selection)
- [Segmentation limits](#segmentation-limits)
- [Encoding with the command-line tool](#encoding-with-the-command-line-tool)
- [Decoding](#decoding)
- [API tools](#api-tools)
- [Field reference](#field-reference)
- [Limitations](#limitations)

## What is supported

| Area | Decode | Encode |
| --- | --- | --- |
| SMS-DELIVER (received messages) | yes | yes, for the simulator and tests only |
| SMS-SUBMIT (sent messages) | yes | yes |
| SMS-STATUS-REPORT (delivery reports) | yes | yes, for the simulator and tests only |
| Service centre address prefix (modem format) | yes | yes; empty by default so that the modem uses the SMSC stored on the SIM |
| Addresses: international, national, alphanumeric senders | yes | numeric destinations |
| GSM 7-bit default alphabet and its extension table | yes | yes |
| UCS-2 (decoded as UTF-16, including surrogate pairs) | yes | yes |
| 8-bit data | yes | yes |
| Data coding scheme: general group, message classes, message-waiting groups, data coding/message class group | yes | general group with optional class |
| Concatenation, 8-bit reference (IEI `00`) | yes | yes |
| Concatenation, 16-bit reference (IEI `08`) | yes | no |
| Application ports, 16-bit (IEI `05`) | yes | yes |
| Application ports, 8-bit (IEI `04`) | yes | no |
| National language single shift tables: Turkish (1), Spanish (2), Portuguese (3) | yes | no |
| National language locking shift table: Turkish (1) | yes | no |
| Portuguese locking shift table, all other national tables | no (default alphabet is used, with a warning) | no |
| Validity period: relative | yes | yes |
| Validity period: absolute | yes | no |
| Validity period: enhanced | skipped (recognised, not interpreted) | no |
| Compressed user data | no (payload kept raw, with a warning) | no |
| Protocol identifier | decoded with a description | set verbatim |
| Service centre timestamps with time zone | yes | yes |

## PDU layout

PDUs are exchanged with the modem as hexadecimal text in "modem format": a service centre
address followed by the TPDU. The argument of `AT+CMGS=` is the TPDU length in octets,
**without** the service centre part.

SMS-SUBMIT as produced by the gateway:

| Field | Octets | Notes |
| --- | --- | --- |
| SMSC length and address | 1 or more | `00` means "use the SIM's service centre" |
| First octet | 1 | Message type `01`, plus flags: validity period present (`10`), status report requested (`20`), user data header present (`40`) |
| TP-MR, message reference | 1 | `00`; the modem assigns the real reference and returns it in `+CMGS` |
| TP-DA, destination address | 2 to 12 | Digit count, type of address (`91` international, `81` otherwise), digits as swapped semi-octets |
| TP-PID, protocol identifier | 1 | `00` unless requested otherwise |
| TP-DCS, data coding scheme | 1 | `00` GSM 7-bit, `04` 8-bit, `08` UCS-2; with a class: `10`–`13`, `14`–`17`, `18`–`1B` |
| TP-VP, validity period | 0 or 1 | Relative format, only when a validity was requested |
| TP-UDL, user data length | 1 | Septets for GSM 7-bit (including the header), octets otherwise |
| TP-UD, user data | up to 140 | Optional user data header, then the payload |

SMS-DELIVER as received: SMSC address, first octet, TP-OA (sender), TP-PID, TP-DCS, TP-SCTS
(7-octet service centre timestamp), TP-UDL, TP-UD.

SMS-STATUS-REPORT as received: SMSC address, first octet, TP-MR (the reference of the
original submission), TP-RA (recipient), TP-SCTS, TP-DT (discharge time), TP-ST (status),
optionally further fields.

Annotated example (`Hello from THUGSred SMS Gateway` to `+4500000000`):

```text
00                  SMSC: none given, the modem uses the SIM's service centre
01                  first octet: SMS-SUBMIT, no validity period, no report, no header
00                  message reference (assigned by the modem)
0A 91 5400000000    destination: 10 digits, international, +4500000000
00                  protocol identifier
00                  data coding scheme: GSM 7-bit
1F                  user data length: 31 septets
C8329BFD0699E5EF36888AAC1EA7F23219346D4E41C730BD7C0FE701    packed text
```

## Encoding selection

| Request | Result |
| --- | --- |
| Binary payload | 8-bit data. Any explicit encoding other than `auto` or `binary` is an error. |
| Text, `encoding: "gsm7"` | GSM 7-bit; an error if a character is outside the default alphabet and its extension table |
| Text, `encoding: "ucs2"` | UCS-2 |
| Text, `encoding: "binary"` | Error |
| Text, `encoding: "auto"` | GSM 7-bit when every character fits, otherwise UCS-2 |

Gateway settings that apply to send requests (not to the offline tools): with
`sms.auto_encoding = false`, `auto` means `sms.default_encoding`; with
`sms.allow_unicode = false`, text that needs UCS-2 is rejected.

Characters of the GSM extension table (`^ { } \ [ ~ ] |` and `€`) cost two septets each. A
single character outside the alphabet switches the whole message to UCS-2, which more than
halves the capacity.

## Segmentation limits

Capacity of one message or one part, in characters for text (extension characters count
twice in GSM 7-bit; characters outside the Basic Multilingual Plane count twice in UCS-2):

| | GSM 7-bit | UCS-2 | 8-bit data |
| --- | --- | --- | --- |
| Single message | 160 | 70 | 140 octets |
| Each part of a multipart message | 153 | 67 | 134 octets |
| Single message with application ports | 152 | 66 | 133 octets |
| Each part of a multipart message with application ports | 146 | 64 | 128 octets |

Why: a part carries at most 140 octets of user data. Concatenation adds a 6-octet user data
header (length octet plus a 5-octet element), application ports a 7-octet header, both
together 12 octets. In GSM 7-bit the header is padded to a septet boundary.

Rules:

- A message that fits a single part is sent without a concatenation header.
- An escape sequence of the GSM extension table and a UTF-16 surrogate pair are never split
  across two parts; the part is ended one unit early instead.
- At most 255 parts are possible. The gateway limits send requests to
  `sms.max_message_parts` (default 10) and to one part when `sms.allow_multipart = false`.
  The offline tools allow 255.
- All parts share an 8-bit reference number. The gateway picks it at random for each
  message.
- `POST /api/v1/sms/estimate` reports the encoding, part count and remaining units for a text
  without queueing anything.

## Encoding with the command-line tool

`thugsred-smsgateway pdu encode` builds SMS-SUBMIT PDUs offline. Nothing is transmitted, and
no configuration or daemon is needed.

| Option | Meaning |
| --- | --- |
| `--to <number>` | Destination (required) |
| `--message <text>` | Text; exactly one of `--message` and `--binary` |
| `--binary <hex>` | Binary payload as hexadecimal |
| `--encoding <auto\|gsm7\|ucs2\|binary>` | Default `auto` |
| `--smsc <number>` | Explicit service centre |
| `--flash` | Class 0 |
| `--delivery-report` | Request a status report |
| `--validity-minutes <n>` | Relative validity period |
| `--destination-port <n>`, `--source-port <n>` | Application ports |
| `--json` (global) | Print the result as JSON |

A short text:

```console
$ thugsred-smsgateway pdu encode --to +4500000000 --message "Hello from THUGSred SMS Gateway"
encoding: gsm7   parts: 1   (offline: nothing was transmitted)
part 1 (AT+CMGS=40):
0001000A91540000000000001FC8329BFD0699E5EF36888AAC1EA7F23219346D4E41C730BD7C0FE701
```

Text that needs UCS-2:

```console
$ thugsred-smsgateway pdu encode --to +4500000000 --message "Blåbærgrød €5 ✓"
encoding: ucs2   parts: 1   (offline: nothing was transmitted)
part 1 (AT+CMGS=42):
0001000A91540000000000081E0042006C00E5006200E600720067007200F80064002020AC003500202713
```

Binary data with application ports (note the first octet `41`: user data header present):

```console
$ thugsred-smsgateway pdu encode --to +4500000000 --binary DEADBEEF --destination-port 5000 --source-port 5000
encoding: 8bit   parts: 1   (offline: nothing was transmitted)
part 1 (AT+CMGS=23):
0041000A91540000000000040B06050413881388DEADBEEF
```

A flash message with a 60-minute validity period (first octet `11`, data coding scheme `10`,
validity `0B`):

```console
$ thugsred-smsgateway pdu encode --to +4500000000 --message "x" --encoding gsm7 --flash --validity-minutes 60
encoding: gsm7   parts: 1   (offline: nothing was transmitted)
part 1 (AT+CMGS=14):
0011000A91540000000000100B0178
```

A text of 170 characters becomes two parts (153 + 17); each carries the header
`05 00 03 <reference> 02 <sequence>`:

```console
$ thugsred-smsgateway pdu encode --to +4500000000 --message "AAAA…(170 × A)"
encoding: gsm7   parts: 2   (offline: nothing was transmitted)
part 1 (AT+CMGS=152):
0041000A9154000000000000A005000300020182C16030180C06…
part 2 (AT+CMGS=33):
0041000A91540000000000001805000300020282C16030180C0683C16030180C0683
```

JSON output:

```console
$ thugsred-smsgateway --json pdu encode --to +4500000000 --message "Hello from THUGSred SMS Gateway" --delivery-report
{
  "data_coding_scheme": 0,
  "encoding": "gsm7",
  "parts": [
    {
      "payload": "48656C6C6F2066726F6D20544855475372656420534D532047617465776179",
      "pdu": "0021000A91540000000000001FC8329BFD0699E5EF36888AAC1EA7F23219346D4E41C730BD7C0FE701",
      "sequence": 1,
      "text": "Hello from THUGSred SMS Gateway",
      "tpdu_length": 40
    }
  ],
  "reference": null
}
```

Errors:

```console
$ thugsred-smsgateway pdu encode --to +4500000000 --message "ж" --encoding gsm7
error: invalid PDU: message contains characters outside the GSM 7-bit alphabet
```

## Decoding

`thugsred-smsgateway pdu decode [PDU]` prints the decoded PDU as JSON. Without an argument
the PDU is read from standard input.

| Option | Meaning |
| --- | --- |
| `--direction <auto\|deliver\|submit>` | `deliver`: received from the network (SMS-DELIVER or SMS-STATUS-REPORT). `submit`: sent to it. Default `auto`. |
| `--no-smsc` | The PDU is a bare TPDU without the service centre prefix (not used with `auto`) |

The message type bits alone do not say whether a PDU is a DELIVER or a SUBMIT; the direction
decides. `auto` tries the plausible interpretations and returns the first that parses
cleanly, preferring modem format over a bare TPDU and received over sent. When you know what
you have, say so.

A received message:

```console
$ thugsred-smsgateway pdu decode 06915400000010040A91540000002000006211422100004005C8329BFD06
{
  "data_coding_scheme": {
    "alphabet": "gsm7",
    "auto_delete": false,
    "compressed": false,
    "message_class": null,
    "message_waiting": null,
    "raw": 0,
    "reserved": false
  },
  "first_octet": 4,
  "more_messages_to_send": false,
  "protocol_description": "normal",
  "protocol_identifier": 0,
  "reply_path": false,
  "sender": {
    "number": "+4500000002",
    "numbering_plan": "isdn",
    "type_of_address": 145,
    "type_of_number": "international"
  },
  "smsc": {
    "number": "+4500000001",
    "numbering_plan": "isdn",
    "type_of_address": 145,
    "type_of_number": "international"
  },
  "status_report_indication": false,
  "timestamp": {
    "local": "2026-11-24T12:00:00+01:00",
    "raw": "26/11/24,12:00:00+04",
    "utc": "2026-11-24T11:00:00Z"
  },
  "type": "deliver",
  "user_data": {
    "encoding": "gsm7",
    "header": null,
    "length": 5,
    "payload": "48656C6C6F",
    "text": "Hello",
    "warnings": []
  },
  "user_data_header_indicator": false
}
```

A PDU built by `pdu encode` decodes as a SUBMIT:

```console
$ thugsred-smsgateway pdu decode 0001000A91540000000000001FC8329BFD0699E5EF36888AAC1EA7F23219346D4E41C730BD7C0FE701
{
  "data_coding_scheme": { "alphabet": "gsm7", "auto_delete": false, "compressed": false, "message_class": null, "message_waiting": null, "raw": 0, "reserved": false },
  "first_octet": 1,
  "message_reference": 0,
  "protocol_description": "normal",
  "protocol_identifier": 0,
  "recipient": { "number": "+4500000000", "numbering_plan": "isdn", "type_of_address": 145, "type_of_number": "international" },
  "reject_duplicates": false,
  "reply_path": false,
  "smsc": null,
  "status_report_request": false,
  "type": "submit",
  "user_data": {
    "encoding": "gsm7",
    "header": null,
    "length": 31,
    "payload": "48656C6C6F2066726F6D20544855475372656420534D532047617465776179",
    "text": "Hello from THUGSred SMS Gateway",
    "warnings": []
  },
  "user_data_header_indicator": false,
  "validity_absolute": null,
  "validity_minutes": null,
  "validity_period_format": "none"
}
```

(The second example is reflowed for brevity; the tool prints one field per line.)

Invalid input:

```console
$ thugsred-smsgateway pdu decode ZZ
error: PDU is not valid hexadecimal
```

## API tools

| Endpoint | Equivalent |
| --- | --- |
| `POST /api/v1/pdu/encode` | `pdu encode`; additionally accepts `message_class`, `protocol_identifier` and `reference`. Response: `{"transmitted": false, "encoded": {...}}` |
| `POST /api/v1/pdu/decode` | `pdu decode`; fields `pdu`, `direction`, `smsc`. Response: `{"decoded": {...}, "mms_notification": ...}` |
| `GET /api/v1/messages/{id}` | Includes `decoded_pdu` for stored messages that kept their raw PDU |

Requests and responses are shown in [API.md](API.md#pdu-tools). The dashboard's PDU Tools
page uses the same endpoints. Neither endpoint is gated by the raw-PDU setting, because
neither transmits anything.

## Field reference

| Field | Meaning |
| --- | --- |
| `type` | `deliver`, `submit` or `status_report` |
| `smsc` | Service centre address, or `null` when the PDU has none |
| `first_octet` | The first TPDU octet as a number; the flags below are derived from it |
| `more_messages_to_send`, `status_report_indication`, `reply_path`, `user_data_header_indicator` | DELIVER flags |
| `reject_duplicates`, `status_report_request` | SUBMIT flags |
| `sender` / `recipient` | Address with `number` (display form, `+` for international numbers, text for alphanumeric senders), `type_of_number`, `numbering_plan`, `type_of_address` (raw octet) |
| `protocol_identifier`, `protocol_description` | TP-PID and its interpretation |
| `data_coding_scheme.alphabet` | `gsm7`, `8bit` or `ucs2` |
| `data_coding_scheme.message_class` | 0 (flash) to 3, or `null` |
| `data_coding_scheme.compressed` | Compressed text; not decoded |
| `data_coding_scheme.auto_delete` | "Marked for automatic deletion" group |
| `data_coding_scheme.message_waiting` | `{active, kind, store}` for message-waiting indications; `kind` is `voicemail`, `fax`, `email` or `other` |
| `data_coding_scheme.reserved` | A reserved coding; decoded as GSM 7-bit with a warning |
| `timestamp` | `raw` digits, `local` (RFC 3339 with the network's offset) and `utc`; the latter two are `null` for an invalid timestamp |
| `validity_period_format`, `validity_minutes`, `validity_absolute` | SUBMIT validity: `none`, `relative`, `enhanced` or `absolute` |
| `message_reference` | SUBMIT: the reference in the PDU. STATUS-REPORT: the reference of the reported submission. |
| `discharge_time`, `status`, `status_category`, `status_description` | STATUS-REPORT outcome. Categories: `delivered`, `pending`, `temporary_failure`, `permanent_failure` |
| `user_data.length` | TP-UDL as transmitted |
| `user_data.encoding` | Alphabet of the payload |
| `user_data.header` | User data header or `null`: `concatenation` (`reference`, `total`, `sequence`, `wide_reference`), `ports` (`destination`, `source`), `national_single_shift`, `national_locking_shift`, and all `elements` (`id`, `data`) |
| `user_data.text` | Decoded text for GSM 7-bit and UCS-2 |
| `user_data.payload` | Payload without the header, uppercase hexadecimal: one unpacked septet per byte for GSM 7-bit, raw octets otherwise |
| `user_data.warnings` | Non-fatal findings: trailing octets, malformed header elements, unsupported national tables, compression, reserved coding |

Relative validity periods are rounded up to the next representable value: 5-minute steps up
to 12 hours, 30-minute steps up to 24 hours, whole days up to 30 days, whole weeks up to 63
weeks.

How the gateway interprets delivery report categories: `delivered` marks the part delivered;
`pending` is recorded in the message history and changes nothing else; both failure
categories mark the part, and with it the message, as `delivery_failed`.

## Limitations

- PDU mode only; SMS text mode is not implemented.
- National language tables are decode-only and limited to Turkish (single and locking
  shift), Spanish (single shift) and Portuguese (single shift). There is no Portuguese
  locking shift table. Outgoing text that does not fit the default alphabet is sent as UCS-2.
- Outgoing multipart messages always use the 8-bit concatenation reference.
- Only relative validity periods are produced.
- Compressed user data is not decompressed.
- A PDU is limited to 1024 hexadecimal characters and 140 octets of user data; anything else
  is rejected.
- Messages that cannot be decoded are not discarded: the gateway stores them as raw records
  with status `malformed` and the decoder's error text.
- The codec has been tested with constructed PDUs and round trips. It has not yet been
  exercised with PDUs captured from a live network through the gateway.

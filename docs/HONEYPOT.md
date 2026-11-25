# Honeypot and SIM Toolkit attack detection

The gateway recognises inbound **SIM Toolkit over-the-air (OTA)** messages — the
binary SMS used to reach the S@T Browser (SIMalliance Toolbox Browser) and WIB
(Wireless Internet Browser) on a SIM card. These are the messages behind the
**Simjacker** and **WIBattack** techniques, which can make a vulnerable SIM
silently return its location and IMEI, send messages or place calls, with
nothing shown on the handset.

On a number used as a honeypot this turns a silent attack packet into a stored,
decoded, scored, alertable record. Detection is **read-only**: the gateway inspects
what arrived and never builds or answers such a message.

Detected attack categories (SMS-layer, i.e. what a receiving modem can see):

| Category | Technique | Severity |
| --- | --- | --- |
| `simjacker` | S@T Browser OTA push | high |
| `wibattack` | WIB OTA push | high |
| `sim_ota` | Other SIM-directed OTA (RFM/RAM/unknown TAR) | high/medium |
| `silent_sms` | Type 0 ping (TP-PID 0x40), silent presence probing | high |
| `oma_cp` | WAP provisioning push (spoofed settings) | medium |
| `smishing` | Inbound text carrying a link (shorteners, lookalikes, lure wording raise the score) | medium/high |

Each detection carries a **confidence** score (0-100): a weighted heuristic of malicious
intent, not a calibrated probability. Honeypot numbers receive no legitimate traffic, so
even a moderate score is worth review. Network-layer (SS7/Diameter) attacks are **not**
visible to a subscriber modem and are out of scope.

## Contents

- [What is detected](#what-is-detected)
- [What is recorded](#what-is-recorded)
- [Fetching detections (remote honeypot)](#fetching-detections-remote-honeypot)
- [Real-time alerts](#real-time-alerts)
- [Limitations](#limitations)

## What is detected

An inbound message is classified as `sim_toolkit` when it is an 8-bit
(binary) SMS directed at the SIM — that is, message class 2 (SIM-specific)
and/or protocol identifier `0x7F` (SIM data download). When the payload is a
secured command packet (ETSI TS 102 225 / 3GPP TS 31.115) the gateway decodes:

- the **Toolkit Application Reference (TAR)** and a name for it (Remote File
  Management, Remote Applet Management, S@T Browser, WIB, or application-specific);
- the **security level** from the SPI: integrity (none / redundancy check /
  cryptographic checksum / digital signature) and whether the packet is ciphered;
- whether the push is **unsecured** — no integrity and no ciphering, which is
  the hallmark of a Simjacker/WIBattack attempt, since the attack only works
  because the browser accepts an unsecured push.

A message is marked `suspected_attack` when it targets a browser TAR or is an
unsecured SIM push.

> The browser TAR values are those reported in the public Simjacker/WIBattack
> research; an operator may use others. The raw TAR is always recorded, so an
> analyst can judge for themselves.

## What is recorded

A detection is an ordinary inbound message with `message_type = "sim_toolkit"`,
so it carries the sender, modem, SIM, timestamps and the **raw attack PDU**
(`raw_pdu`, when `sms.store_raw_pdu` is on) and payload. The decoded analysis is
in `metadata.sim_toolkit`:

```json
{
  "message_type": "sim_toolkit",
  "sender": "+4500000042",
  "raw_pdu": "...",
  "metadata": {
    "sim_toolkit": {
      "indicator": "class 2 (SIM) + PID 0x7F (SIM data download)",
      "command_packet": true,
      "tar": "B00010",
      "tar_name": "S@T Browser (SIMalliance Toolbox)",
      "application": "sat_browser",
      "security": { "integrity": "none", "ciphered": false, "unsecured": true },
      "suspected_attack": true,
      "summary": "SIM Toolkit OTA (SMS-PP data download): class 2 (SIM) + PID 0x7F (SIM data download); TAR B00010 (S@T Browser (SIMalliance Toolbox)); integrity none, not ciphered; consistent with an S@T/WIB browser push (Simjacker/WIBattack class)"
    }
  }
}
```

## Fetching detections (remote honeypot)

The gateway stores and aggregates everything so the collector only has to GET:

```bash
# All detections, newest first — each row is self-contained
curl -s 'https://<gateway>/api/v1/security/events' -H 'Authorization: Bearer <token>'

# Incremental: only what is new since the last poll
curl -s 'https://<gateway>/api/v1/security/events?from=2026-11-25T00:00:00Z' -H 'Authorization: Bearer <token>'

# Narrow to one attack type or one honeypot number
curl -s 'https://<gateway>/api/v1/security/events?category=simjacker' -H 'Authorization: Bearer <token>'

# Ready-made statistics (counts by category, severity, number, day; confidence)
curl -s 'https://<gateway>/api/v1/security/stats?range=30d' -H 'Authorization: Bearer <token>'
```

Each `security/events` row carries the attacker, the targeted honeypot number, the category,
severity, confidence and detail, plus the joined message (`message_type`, `raw_pdu`, `body`,
`metadata`) — so no per-event follow-up call is needed. Ordinary inbound SMS remain available
from `/api/v1/messages`; the SIM-toolkit ones are also `type=sim_toolkit` there.

## Real-time alerts

Every detection raises `security.threat_detected` (SIM-toolkit messages also raise
`sms.ota_toolkit` and `security.sim_attack`). Point an integration at your collector to have
each detection pushed as it happens, for example in `integrations.conf`:

```toml
[[integration]]
name = "honeypot-collector"
type = "webhook"
enabled = true
events = ["security.threat_detected"]
url = "https://collector.example/intake"
sign_payload = true
signing_secret = "${HONEYPOT_HMAC_SECRET}"
```

The webhook body carries the full message, including `metadata.sim_toolkit`. Any
integration type works (webhook, Discord, Slack, Telegram, ntfy, …), and a
`filter` can narrow it, for example to `suspected_attack` hits only.

## Limitations

- Detection is heuristic and based on the message structure; it classifies a
  push, it does not execute or validate it against a specific SIM.
- Only inbound detection is provided. The gateway does not build SIM Toolkit OTA
  messages. Authorised OTA to SIMs you operate uses an operator OTA platform
  with real OTA keys, not this gateway.
- A concatenated OTA push is assembled like any multipart message before it is
  analysed; a push with parts missing is stored as `incomplete`.

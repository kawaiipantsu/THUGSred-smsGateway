# SMS providers

A provider is an optional outbound route next to the modems. The gateway hands the message
to the provider's HTTPS API, and the provider delivers it. The reason to use one is the
**sender name**: a SIM always sends with its own number, while a provider can show a name
such as `Example` instead.

Supported provider type: `gatewayapi` ([GatewayAPI](https://gatewayapi.com), Mobile Messaging
API, `.com` and `.eu` platforms).

Status: tested against a local stand-in for the provider API. No message has been sent
through the real service as part of the project's tests.

## Contents

- [What a provider route can and cannot do](#what-a-provider-route-can-and-cannot-do)
- [Configuration](#configuration)
- [Sending](#sending)
- [Sender names](#sender-names)
- [Delivery status callbacks](#delivery-status-callbacks)
- [Failures and retries](#failures-and-retries)
- [Spending protection](#spending-protection)
- [Changing the API token](#changing-the-api-token)
- [Privacy](#privacy)

## What a provider route can and cannot do

| | Modem | Provider |
| --- | --- | --- |
| Sender shown to the recipient | The SIM's number | A configured sender name or number |
| Cost | The SIM's subscription | Billed by the provider per segment |
| Needs internet access | No | Yes (outbound HTTPS) |
| Text SMS, long messages, scheduling, priorities, idempotency | Yes | Yes |
| Binary, flash, application ports, forced encoding, message class | Yes | No (rejected with `400`) |
| Delivery status | Delivery reports from the network | Signed webhook from the provider |
| Receiving SMS | Yes | No |

A message goes to a provider only when the request names `provider` or `sender_name`.
Everything else, including automation rules, uses a modem as before.

## Configuration

Providers live in `/etc/thugsred/providers.conf` (path: `paths.providers`). The file is
optional. It holds an API token, so keep it at mode `0640`, owner `root`, group
`thugsred-smsgateway`. Changes apply on reload.

```toml
[[provider]]
name = "gatewayapi"
type = "gatewayapi"
enabled = true
api_token = "replace-with-your-api-token"
region = "com"
default_sender = "Example"
allowed_senders = []
daily_sms_limit = 100
monthly_sms_limit = 1000
callback_secret = ""
```

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | Identifier used in requests and in the callback URL: letters, digits, `-`, `_`; unique |
| `type` | string | `"gatewayapi"` | Provider type |
| `enabled` | boolean | `false` | Disabled providers are ignored; their queued messages wait until they expire |
| `api_token` | string | | API token. Required when enabled. May be a secret reference (`"${NAME}"`, `"${file:/path}"`) |
| `region` | string | `"com"` | `com` (`messaging.gatewayapi.com`) or `eu` (`messaging.gatewayapi.eu`). A token works on one platform only. |
| `base_url` | string | | Replaces the URL derived from `region` (testing) |
| `default_sender` | string | | Sender used when a request names none |
| `allowed_senders` | list of strings | `[]` | Further senders requests may ask for; `"*"` accepts any well-formed sender (warning) |
| `priority` | string | `"normal"` | `normal` or `urgent`; the provider may charge more for urgent |
| `label` | string | | Label for the provider's own statistics, at most 128 characters |
| `timeout_seconds` | integer | `15` | Time allowed for one API request |
| `rate_limit_per_minute` | integer | `60` | Messages per minute handed to the provider; `0` is unlimited |
| `daily_sms_limit`, `monthly_sms_limit` | integer | `0` | Spending protection in segments; `0` is unlimited (warning when both are) |
| `callback_secret` | string | | Shared secret of the status webhook. Empty: callbacks are rejected (warning) |

An enabled provider needs `api_token` and at least one of `default_sender` and
`allowed_senders`. `thugsred-smsgateway check-config` reports problems;
`GET /api/v1/providers` and the configuration view never show the token or the secret.

## Sending

Add `provider`, `sender_name`, or both to a normal send request:

```bash
curl -sS -X POST http://127.0.0.1:8080/api/v1/sms/send \
  -H 'Content-Type: application/json' \
  -d '{"to": "+4500000000", "message": "Hello", "provider": "gatewayapi", "sender_name": "Example"}'
```

```bash
thugsred-smsgateway sms send --to +4500000000 --message "Hello" --provider gatewayapi --sender-name Example
```

- `provider` alone uses the provider's `default_sender`.
- `sender_name` alone uses the first enabled provider.
- `modem` and `from` cannot be combined with either field.
- The destination must be in international format (`+<country code>…`).
- `validity_minutes` is limited to 7200 (5 days).

The response carries `"provider"` and `"sender"`. In message records the route appears in
the `modem_id` field as `provider:<name>`, which also works in filters (`modem_ids`) and in
the `modem` query parameter of the message list.

In the dashboard, the Send page shows a **Send via** choice once a provider is enabled.

## Sender names

A sender is either a name of 3 to 11 letters (`a-z`, `A-Z`), digits or spaces, or a number
of 3 to 15 digits. A request may only use `default_sender` or an entry of
`allowed_senders`; anything else is refused with `403`.

Use only senders you are entitled to use. Operators and countries apply their own rules:
some require registered senders, some replace an unregistered name with a generic one, and
some reject the message. Recipients cannot reply to a name. The provider's documentation
lists the rules per country.

## Delivery status callbacks

The provider API has no way to ask for the state of a message; it reports states through a
webhook. Without it a provider message stays `submitted`.

1. Choose a long random secret and set it as `callback_secret`. Reload.
2. In the provider's dashboard, add a webhook for message status events with the URL
   `https://<public name>/api/v1/providers/<name>/callback` and the same secret.

The gateway must be reachable from the provider's servers at that URL. The gateway itself
serves plain HTTP, so publish it through a reverse proxy that terminates TLS, and forward
only this one path if the rest of the API should stay private.

How a callback is handled:

- It is authenticated by its `Signature: v1=<hex HMAC-SHA256 of the raw body>` header alone.
  The path is exempt from `api.allowed_cidrs`, `api.allowed_hosts`, bearer tokens and the
  rate limit, because the provider's addresses are not fixed.
- A missing or wrong signature gives `401`; a provider without `callback_secret` gives `404`.
- `DELIVERED` and `ACCEPTED` set the message to `delivered`; `EXPIRED`, `UNDELIVERABLE`,
  `REJECTED`, `DELETED` and `SKIPPED` set it to `delivery_failed`. Other states are added to
  the message history.
- A final state is never overwritten, so repeated or late reports are harmless. A report
  that arrives while the submission is still being recorded gets `503` and is repeated by
  the provider.
- Events `sms.delivered` and `sms.failed` are emitted as for modem messages.

## Failures and retries

| Provider answer | Result |
| --- | --- |
| `2xx` | `submitted` |
| `408`, `429`, `5xx`, connection refused | Retried with backoff, up to `sms.max_send_attempts`, then `failed` |
| `401`, `403` | `failed` at once (token, credit or destination not permitted) |
| Other `4xx` | `failed` at once (the provider refused the message) |
| No answer after the request was sent | `submission_unknown`; never retried automatically, because the message may have been accepted and would be billed twice |

A failed or uncertain message can be retried by an operator (`POST /api/v1/queue/{id}/retry`).

## Spending protection

`daily_sms_limit` and `monthly_sms_limit` count segments submitted through the provider in
the current day and month (time zone: `general.timezone`). A request that would exceed a
limit is refused with `429`; a message already queued is deferred. The segment count is the
gateway's own estimate (160/153 characters GSM 7-bit, 70/67 UCS-2); the provider's invoice
is authoritative. The count undercounts when `retention.message_days` is shorter than a month.

`security.destination_allowlist`, `destination_blocklist` and
`sms.per_destination_interval_seconds` apply to provider messages too.

## Changing the API token

Edit `api_token` in `/etc/thugsred/providers.conf` and reload:

```bash
sudoedit /etc/thugsred/providers.conf
sudo thugsred-smsgateway check-config
sudo systemctl reload thugsred-smsgateway
```

The next message uses the new token. No restart is needed.

## Privacy

Destination number, sender and message text of provider messages leave the host and are
processed by the provider. Modem messages are not affected.

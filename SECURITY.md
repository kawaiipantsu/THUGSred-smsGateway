# Security policy

## Supported versions

| Version | Supported |
| --- | --- |
| 0.1.x | Yes |

THUGSred-smsGateway is at its first release. Security fixes are made on the latest released
version; there are no maintained older branches.

## Reporting a vulnerability

Please report vulnerabilities **privately**. Do not open a public issue, pull request or
discussion for a suspected vulnerability.

Use GitHub's private vulnerability reporting for the repository:

1. Go to <https://github.com/kawaiipantsu/THUGSred-smsGateway/security/advisories>.
2. Choose **Report a vulnerability** (or open
   <https://github.com/kawaiipantsu/THUGSred-smsGateway/security/advisories/new>).
3. Fill in the form. The report is visible only to you and the maintainers.

The advisory is the place for all further communication: questions, a fix under
development, and coordinated disclosure.

## What to include

- The affected version (`thugsred-smsgateway version`) and how it was installed.
- The component: API, dashboard, configuration handling, PDU decoding, modem handling,
  integrations, installer or packaging.
- A description of the problem and its impact: what an attacker can do, and from which
  position (an SMS sender, a host on the allowed network, a holder of an ordinary token, a
  local user, a receiver of integration traffic).
- Steps to reproduce, ideally against the built-in simulator
  (`thugsred-smsgateway run --simulate-modem`), with the relevant configuration settings.
- For issues triggered by message content: the PDU or text, built from **synthetic**
  numbers and content.
- Any suggested fix or mitigation.

Please do not include real telephone numbers, real messages, SIM or device identifiers
(ICCID, IMSI, IMEI), tokens or webhook URLs. Redact logs and diagnostic reports before
attaching them.

## Scope notes

The gateway is designed for a trusted internal network, serves plain HTTP, and ships with
API authentication disabled. Those are documented design decisions rather than
vulnerabilities; the documentation says how to enable tokens and how to put a TLS reverse
proxy in front. Reports that show a way **around** an enabled control (the network
allowlist, tokens, the administrative layer, cross-site request protection, the raw AT and
raw PDU gates, destination lists and quotas, anonymisation, secret redaction) are very much
in scope, as is anything that lets SMS content or modem output cause harm.

## More information

The threat model, every access control and how to enable it, secrets handling, spending
protection and sandboxing are described in [docs/SECURITY.md](docs/SECURITY.md).

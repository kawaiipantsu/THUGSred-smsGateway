//! Integration adapters. Each adapter turns an event into a request
//! description ([`RequestSpec`]); a shared sender performs the delivery.
//! Keeping payload construction pure makes every adapter testable offline.

pub mod worker;

use crate::config::{Config, IntegrationDef};
use crate::database::EventRecord;
use crate::templates::{self, Escape};
use hmac::{Hmac, Mac};
use serde_json::{json, Value as Json};
use sha2::Sha256;
use std::time::Duration;

pub const USER_AGENT: &str = concat!("THUGSred-smsGateway/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    None,
    Json(Json),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RequestSpec {
    Http {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        query: Vec<(String, String)>,
        body: Body,
        content_type: Option<String>,
    },
    /// One UDP datagram (syslog).
    Udp { server: String, payload: String },
}

/// Result of one delivery attempt.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Outcome {
    pub success: bool,
    pub status_code: Option<u16>,
    pub error: Option<String>,
    /// Whether a failed attempt may succeed later.
    pub retryable: bool,
    pub duration_ms: u64,
}

fn display(name: Option<&str>, number: &str) -> String {
    match name {
        Some(n) => format!("{n} ({number})"),
        None => number.to_string(),
    }
}

/// Prepare an event for one integration: add address book names and apply
/// anonymisation. Numbers on the global anonymous list are always replaced,
/// whatever the integration's own settings are; `anonymize = true` on the
/// integration replaces every number.
///
/// This runs after routing, so filters still see the real values.
pub fn prepare(def: &IntegrationDef, ev: &EventRecord, cfg: &Config) -> EventRecord {
    let mut ev = ev.clone();
    let label = cfg.anonymous.label.clone();
    let mut redact_body = !def.include_message_body;
    if let Some(m) = ev.data.get_mut("message").and_then(Json::as_object_mut) {
        let mut anonymised = false;
        for field in ["sender", "recipient"] {
            let Some(number) = m.get(field).and_then(Json::as_str).map(str::to_string) else {
                m.insert(format!("{field}_display"), json!(""));
                continue;
            };
            let listed = cfg.is_anonymous(&number);
            if def.anonymize || listed {
                anonymised = true;
                redact_body |= listed && cfg.anonymous.redact_body;
                m.insert(field.into(), json!(label));
                m.insert(format!("{field}_name"), Json::Null);
                m.insert(format!("{field}_display"), json!(label));
            } else {
                let name = cfg.contact_name(&number);
                m.insert(format!("{field}_name"), json!(name));
                m.insert(format!("{field}_display"), json!(display(name, &number)));
            }
        }
        if anonymised {
            // The raw PDU and binary payload carry the real address.
            m.remove("raw_pdu");
            m.remove("payload");
            m.insert("anonymized".into(), json!(true));
        }
        if redact_body && m.get("body").map(|b| !b.is_null()).unwrap_or(false) {
            m.insert("body".into(), json!("[redacted]"));
            m.remove("raw_pdu");
            m.remove("payload");
        }
    }
    if let Some(c) = ev.data.get_mut("call").and_then(Json::as_object_mut) {
        match c.get("number").and_then(Json::as_str).map(str::to_string) {
            Some(number) if def.anonymize || cfg.is_anonymous(&number) => {
                c.insert("number".into(), json!(label));
                c.insert("name".into(), Json::Null);
                c.insert("display".into(), json!(label));
                c.insert("anonymized".into(), json!(true));
            }
            Some(number) => {
                let name = cfg.contact_name(&number);
                c.insert("name".into(), json!(name));
                c.insert("display".into(), json!(display(name, &number)));
            }
            None => {
                c.insert("display".into(), json!("unknown number"));
            }
        }
    }
    ev
}

/// Template context: the event data's top-level keys plus `event` and `gateway`.
pub fn context(ev: &EventRecord, cfg: &Config) -> Json {
    let mut ctx = match &ev.data {
        Json::Object(o) => o.clone(),
        _ => Default::default(),
    };
    ctx.insert(
        "event".into(),
        json!({"id": ev.id, "type": ev.event_type, "timestamp": ev.timestamp, "instance_id": ev.instance_id, "correlation_id": ev.correlation_id}),
    );
    ctx.insert(
        "gateway".into(),
        json!({"instance_id": cfg.general.instance_id, "instance_name": cfg.general.instance_name}),
    );
    Json::Object(ctx)
}

/// Built-in message text for chat style integrations.
pub fn default_template(event_type: &str) -> &'static str {
    match event_type {
        "sms.received" | "sms.multipart_completed" => "SMS from {{message.sender_display}}: {{message.body}}",
        "sms.binary_received" => "Binary SMS from {{message.sender_display}} ({{message.encoding}}, port {{message.dst_port}})",
        "sms.service_received" => "Service message from {{message.sender_display}}: {{message.body}}",
        "sms.queued" => "SMS to {{message.recipient_display}} queued ({{message.parts}} part(s))",
        "sms.sent" => "SMS to {{message.recipient_display}} submitted to the network",
        "sms.delivered" => "SMS to {{message.recipient_display}} delivered",
        "sms.failed" => "SMS to {{message.recipient_display}} failed: {{message.error}}",
        "mms.notification" => "MMS notification from {{message.sender_display}}: {{message.body}}",
        "mms.received" => "MMS from {{message.sender_display}} downloaded",
        "ussd.response" => "USSD {{ussd.code}}: {{ussd.response}}",
        "call.incoming" => "Incoming call from {{call.display}}",
        "call.missed" => "Missed call from {{call.display}}",
        "call.ended" => "Call with {{call.display}} ended after {{call.duration_seconds}} s",
        "call.recorded" => "Call from {{call.display}} recorded ({{call.recording_seconds}} s)",
        "modem.connected" => "Modem {{modem_id}} connected ({{manufacturer}} {{model}})",
        "modem.disconnected" => "Modem {{modem_id}} disconnected: {{reason}}",
        "modem.error" => "Modem {{modem_id}} error: {{error}}",
        "modem.signal_changed" => "Modem {{modem_id}} signal {{signal_percent}}% ({{rssi_dbm}} dBm)",
        "sim.ready" => "SIM ready on modem {{modem_id}}",
        "sim.locked" => "SIM locked on modem {{modem_id}}: {{state}}",
        "network.registered" => "Modem {{modem_id}} registered on {{operator}}",
        "network.lost" => "Modem {{modem_id}} lost network registration",
        "gateway.started" => "{{gateway.instance_name}} started (version {{version}})",
        "gateway.stopped" => "{{gateway.instance_name}} stopped",
        "gateway.error" => "{{gateway.instance_name}} error: {{error}}",
        "integration.test" => "Test event from {{gateway.instance_name}}",
        _ => "{{event.type}} on {{gateway.instance_name}}",
    }
}

fn chat_text(
    def: &IntegrationDef,
    ev: &EventRecord,
    cfg: &Config,
    mode: Escape,
    limit: usize,
) -> String {
    let tpl = if def.template.is_empty() {
        default_template(&ev.event_type)
    } else {
        &def.template
    };
    let text = templates::render(tpl, &context(ev, cfg), mode);
    truncate(&text, limit)
}

fn truncate(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(limit.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Event envelope sent to generic webhooks.
pub fn envelope(def: &IntegrationDef, ev: &EventRecord) -> Json {
    let mut data = ev.data.clone();
    if !def.include_raw_pdu {
        if let Some(m) = data.get_mut("message").and_then(Json::as_object_mut) {
            m.remove("raw_pdu");
        }
    }
    json!({
        "id": ev.id,
        "type": ev.event_type,
        "timestamp": ev.timestamp,
        "instance_id": ev.instance_id,
        "correlation_id": ev.correlation_id,
        "data": data,
    })
}

/// `sha256=` HMAC over `<timestamp>.<body>`.
pub fn sign(secret: &str, timestamp: &str, body: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn http(method: &str, url: String, body: Body) -> RequestSpec {
    RequestSpec::Http {
        method: method.to_string(),
        url,
        headers: Vec::new(),
        query: Vec::new(),
        body,
        content_type: None,
    }
}

/// Build the request for an integration and event.
pub fn build_request(
    def: &IntegrationDef,
    ev: &EventRecord,
    cfg: &Config,
) -> Result<RequestSpec, String> {
    let ev = &prepare(def, ev, cfg);
    match def.kind.as_str() {
        "webhook" => Ok(build_webhook(def, ev, cfg)),
        "discord" => {
            let mut body = json!({
                "content": chat_text(def, ev, cfg, Escape::Markdown, 2000),
                "allowed_mentions": {"parse": []},
            });
            if !def.username.is_empty() {
                body["username"] = json!(truncate(&def.username, 80));
            }
            Ok(http("POST", def.webhook_url.clone(), Body::Json(body)))
        }
        "slack" => Ok(http(
            "POST",
            def.webhook_url.clone(),
            Body::Json(json!({"text": chat_text(def, ev, cfg, Escape::Slack, 3000)})),
        )),
        "teams" => {
            // Adaptive Card posted to a Teams Workflows ("When a Teams webhook
            // request is received") URL.
            let card = json!({
                "type": "message",
                "attachments": [{
                    "contentType": "application/vnd.microsoft.card.adaptive",
                    "contentUrl": null,
                    "content": {
                        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                        "type": "AdaptiveCard",
                        "version": "1.4",
                        "body": [
                            {"type": "TextBlock", "text": cfg.general.instance_name, "weight": "Bolder", "size": "Medium", "wrap": true},
                            {"type": "TextBlock", "text": chat_text(def, ev, cfg, Escape::Markdown, 4000), "wrap": true},
                            {"type": "TextBlock", "text": format!("{} · {}", ev.event_type, ev.timestamp), "isSubtle": true, "size": "Small", "wrap": true}
                        ]
                    }
                }]
            });
            Ok(http("POST", def.webhook_url.clone(), Body::Json(card)))
        }
        "telegram" => {
            let base = if def.url.is_empty() {
                "https://api.telegram.org"
            } else {
                def.url.trim_end_matches('/')
            };
            // No parse_mode: the text is sent as plain text, so message content
            // cannot inject formatting or links.
            Ok(http(
                "POST",
                format!("{base}/bot{}/sendMessage", def.bot_token),
                Body::Json(json!({
                    "chat_id": def.chat_id,
                    "text": chat_text(def, ev, cfg, Escape::Chat, 4096),
                    "disable_web_page_preview": true,
                })),
            ))
        }
        "ntfy" => {
            let mut spec = http(
                "POST",
                format!("{}/{}", def.url.trim_end_matches('/'), def.topic),
                Body::Text(chat_text(def, ev, cfg, Escape::None, 4000)),
            );
            if let RequestSpec::Http {
                headers,
                content_type,
                ..
            } = &mut spec
            {
                *content_type = Some("text/plain; charset=utf-8".into());
                headers.push(("Title".into(), ev.event_type.clone()));
                if def.priority > 0 {
                    headers.push(("Priority".into(), def.priority.clamp(1, 5).to_string()));
                }
                if !def.token.is_empty() {
                    headers.push(("Authorization".into(), format!("Bearer {}", def.token)));
                }
            }
            Ok(spec)
        }
        "gotify" => {
            let mut spec = http(
                "POST",
                format!("{}/message", def.url.trim_end_matches('/')),
                Body::Json(json!({
                    "title": format!("{}: {}", cfg.general.instance_name, ev.event_type),
                    "message": chat_text(def, ev, cfg, Escape::None, 4000),
                    "priority": if def.priority > 0 { def.priority } else { 5 },
                })),
            );
            if let RequestSpec::Http { headers, .. } = &mut spec {
                headers.push(("X-Gotify-Key".into(), def.token.clone()));
            }
            Ok(spec)
        }
        "syslog" => {
            let facility = match def.facility.as_str() {
                "user" => 1,
                "daemon" => 3,
                "local1" => 17,
                "local2" => 18,
                "local3" => 19,
                "local4" => 20,
                "local5" => 21,
                "local6" => 22,
                "local7" => 23,
                _ => 16,
            };
            let severity =
                if ev.event_type.ends_with(".error") || ev.event_type.ends_with(".failed") {
                    3
                } else {
                    6
                };
            let msg = chat_text(def, ev, cfg, Escape::None, 900).replace(['\r', '\n'], " ");
            Ok(RequestSpec::Udp {
                server: def.server.clone(),
                payload: format!(
                    "<{}>1 {} {} thugsred-smsgateway - {} - {}",
                    facility * 8 + severity,
                    ev.timestamp,
                    cfg.general.instance_id,
                    ev.event_type,
                    msg
                ),
            })
        }
        other => Err(format!("unsupported integration type {other:?}")),
    }
}

fn build_webhook(def: &IntegrationDef, ev: &EventRecord, cfg: &Config) -> RequestSpec {
    let ctx = context(ev, cfg);
    let method = def.method.to_ascii_uppercase();
    let mut headers: Vec<(String, String)> = def
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    headers.push(("X-THUGSred-Event".into(), ev.event_type.clone()));
    headers.push(("X-THUGSred-Event-Id".into(), ev.id.clone()));
    headers.push(("X-THUGSred-Timestamp".into(), ev.timestamp.clone()));
    let mut query: Vec<(String, String)> = def
        .query
        .iter()
        .map(|(k, v)| (k.clone(), templates::render(v, &ctx, Escape::None)))
        .collect();
    let mut content_type = None;
    let body = if method == "GET" {
        if query.is_empty() {
            for (k, path) in [
                ("event", "event.type"),
                ("id", "event.id"),
                ("timestamp", "event.timestamp"),
                ("sender", "message.sender"),
                ("recipient", "message.recipient"),
                ("message", "message.body"),
            ] {
                if let Some(v) = templates::lookup(&ctx, path).and_then(Json::as_str) {
                    query.push((k.into(), v.to_string()));
                }
            }
        }
        Body::None
    } else if def.body_template.is_empty() {
        Body::Json(envelope(def, ev))
    } else {
        let ct = def.content_type.to_ascii_lowercase();
        let mode = if ct.contains("json") {
            Escape::Json
        } else if ct.contains("x-www-form-urlencoded") {
            Escape::Url
        } else if ct.contains("html") || ct.contains("xml") {
            Escape::Html
        } else {
            Escape::None
        };
        content_type = Some(def.content_type.clone());
        Body::Text(templates::render(&def.body_template, &ctx, mode))
    };
    if def.sign_payload {
        let raw = match &body {
            Body::Json(j) => j.to_string(),
            Body::Text(t) => t.clone(),
            Body::None => query
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&"),
        };
        headers.push((
            "X-THUGSred-Signature".into(),
            sign(&def.signing_secret, &ev.timestamp, &raw),
        ));
    }
    RequestSpec::Http {
        method,
        url: def.url.clone(),
        headers,
        query,
        body,
        content_type,
    }
}

/// Shared HTTP clients (redirects disabled, rustls).
#[derive(Clone)]
pub struct Sender {
    verified: reqwest::Client,
    unverified: reqwest::Client,
}

impl Default for Sender {
    fn default() -> Self {
        Self::new()
    }
}

impl Sender {
    pub fn new() -> Self {
        let build = |insecure: bool| {
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .redirect(reqwest::redirect::Policy::none())
                .danger_accept_invalid_certs(insecure)
                .connect_timeout(Duration::from_secs(10))
                .build()
                .expect("HTTP client construction")
        };
        Self {
            verified: build(false),
            unverified: build(true),
        }
    }

    pub async fn deliver(&self, def: &IntegrationDef, ev: &EventRecord, cfg: &Config) -> Outcome {
        let started = std::time::Instant::now();
        let done = |success, status_code, error: Option<String>, retryable| Outcome {
            success,
            status_code,
            error,
            retryable,
            duration_ms: started.elapsed().as_millis() as u64,
        };
        let spec = match build_request(def, ev, cfg) {
            Ok(s) => s,
            Err(e) => return done(false, None, Some(e), false),
        };
        let timeout = Duration::from_secs(def.timeout_seconds.max(1));
        match spec {
            RequestSpec::Udp { server, payload } => {
                let send = async {
                    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
                    sock.send_to(payload.as_bytes(), &server).await
                };
                match tokio::time::timeout(timeout, send).await {
                    Ok(Ok(_)) => done(true, None, None, false),
                    Ok(Err(e)) => done(false, None, Some(format!("syslog send failed: {e}")), true),
                    Err(_) => done(false, None, Some("syslog send timed out".into()), true),
                }
            }
            RequestSpec::Http {
                method,
                url,
                headers,
                query,
                body,
                content_type,
            } => {
                let client = if def.verify_tls {
                    &self.verified
                } else {
                    &self.unverified
                };
                let method =
                    reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::POST);
                let mut req = client.request(method, &url).timeout(timeout);
                if !query.is_empty() {
                    req = req.query(&query);
                }
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                req = match body {
                    Body::Json(j) => req.json(&j),
                    Body::Text(t) => req
                        .header(
                            "Content-Type",
                            content_type.unwrap_or_else(|| "text/plain; charset=utf-8".into()),
                        )
                        .body(t),
                    Body::None => req,
                };
                match req.send().await {
                    Ok(resp) => {
                        let code = resp.status().as_u16();
                        if resp.status().is_success() {
                            done(true, Some(code), None, false)
                        } else {
                            let retryable = code >= 500 || code == 429 || code == 408;
                            done(
                                false,
                                Some(code),
                                Some(format!("HTTP status {code}")),
                                retryable,
                            )
                        }
                    }
                    // `without_url` keeps tokens embedded in URLs out of logs.
                    Err(e) => {
                        let e = e.without_url();
                        let kind = if e.is_timeout() {
                            "request timed out".to_string()
                        } else if e.is_connect() {
                            "connection failed".to_string()
                        } else {
                            format!("request failed: {e}")
                        };
                        done(false, None, Some(kind), true)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(body: &str) -> EventRecord {
        EventRecord {
            id: "evt-1".into(),
            event_type: "sms.received".into(),
            instance_id: "gw".into(),
            correlation_id: Some("msg-1".into()),
            timestamp: "2025-01-01T00:00:00.000Z".into(),
            data: json!({"message": {"sender": "+4500000000", "recipient": null, "body": body, "raw_pdu": "00FF"}}),
        }
    }

    fn def(kind: &str) -> IntegrationDef {
        IntegrationDef {
            name: "t".into(),
            kind: kind.into(),
            enabled: true,
            url: "https://example.invalid/hook".into(),
            webhook_url: "https://example.invalid/chat".into(),
            ..Default::default()
        }
    }

    type Parts = (
        String,
        String,
        Vec<(String, String)>,
        Vec<(String, String)>,
        Body,
    );

    fn parts(spec: RequestSpec) -> Parts {
        match spec {
            RequestSpec::Http {
                method,
                url,
                headers,
                query,
                body,
                ..
            } => (method, url, headers, query, body),
            _ => panic!("expected http"),
        }
    }

    const NASTY: &str =
        "@everyone <!channel> <@U123> **bold** [x](http://evil) \"quote\" \\ \n new";

    #[test]
    fn webhook_post_envelope_and_signature() {
        let mut d = def("webhook");
        d.sign_payload = true;
        d.signing_secret = "s3cret".into();
        d.headers
            .insert("X-Gateway".into(), "THUGSred-smsGateway".into());
        let (method, url, headers, _, body) =
            parts(build_request(&d, &ev("hi"), &Config::default()).unwrap());
        assert_eq!(
            (method.as_str(), url.as_str()),
            ("POST", "https://example.invalid/hook")
        );
        let Body::Json(j) = body else { panic!() };
        assert_eq!(j["type"], "sms.received");
        assert_eq!(j["data"]["message"]["body"], "hi");
        assert!(
            j["data"]["message"].get("raw_pdu").is_none(),
            "raw PDU excluded by default"
        );
        let sig = headers
            .iter()
            .find(|(k, _)| k == "X-THUGSred-Signature")
            .unwrap()
            .1
            .clone();
        assert_eq!(
            sig,
            sign("s3cret", "2025-01-01T00:00:00.000Z", &j.to_string())
        );
        assert!(headers
            .iter()
            .any(|(k, v)| k == "X-Gateway" && v == "THUGSred-smsGateway"));
        assert!(headers
            .iter()
            .any(|(k, v)| k == "X-THUGSred-Event-Id" && v == "evt-1"));
        d.include_raw_pdu = true;
        let (.., body) = parts(build_request(&d, &ev("hi"), &Config::default()).unwrap());
        let Body::Json(j) = body else { panic!() };
        assert_eq!(j["data"]["message"]["raw_pdu"], "00FF");
    }

    #[test]
    fn hmac_known_vector() {
        // RFC 4231 test case 2 with the message split as "<timestamp>.<body>".
        assert_eq!(
            sign(
                "Jefe",
                "what do ya want for nothing?"
                    .split_once('.')
                    .map(|x| x.0)
                    .unwrap_or("what do ya want for nothing?"),
                ""
            ),
            sign("Jefe", "what do ya want for nothing?", "")
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(b"Jefe").unwrap();
        mac.update(b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac.finalize().into_bytes()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn webhook_get_uses_query_templates() {
        let mut d = def("webhook");
        d.method = "get".into();
        d.query.insert("sender".into(), "{{message.sender}}".into());
        d.query.insert("message".into(), "{{message.body}}".into());
        d.query
            .insert("timestamp".into(), "{{event.timestamp}}".into());
        let (method, _, _, query, body) =
            parts(build_request(&d, &ev("a&b=c d"), &Config::default()).unwrap());
        assert_eq!(method, "GET");
        assert_eq!(body, Body::None);
        // raw values: the HTTP client performs URL encoding exactly once
        assert!(query.contains(&("message".into(), "a&b=c d".into())));
        assert!(query.contains(&("sender".into(), "+4500000000".into())));
        d.query.clear();
        let (.., query, _) = parts(build_request(&d, &ev("x"), &Config::default()).unwrap());
        assert!(query.iter().any(|(k, _)| k == "event"));
    }

    #[test]
    fn webhook_body_template_is_json_safe() {
        let mut d = def("webhook");
        d.body_template =
            "{\"from\": \"{{message.sender}}\", \"text\": \"{{message.body}}\"}".into();
        let (.., body) = parts(build_request(&d, &ev(NASTY), &Config::default()).unwrap());
        let Body::Text(t) = body else { panic!() };
        let parsed: Json = serde_json::from_str(&t).expect("template output is valid JSON");
        assert_eq!(parsed["text"], NASTY);
    }

    #[test]
    fn discord_blocks_mentions_and_markdown() {
        let mut d = def("discord");
        d.username = "THUGSred SMS Gateway".into();
        d.template = "SMS from {{message.sender}}: {{message.body}}".into();
        let (_, url, _, _, body) =
            parts(build_request(&d, &ev(NASTY), &Config::default()).unwrap());
        assert_eq!(url, "https://example.invalid/chat");
        let Body::Json(j) = body else { panic!() };
        assert_eq!(j["allowed_mentions"]["parse"], json!([]));
        let content = j["content"].as_str().unwrap();
        assert!(!content.contains("@everyone"));
        assert!(content.contains("\\*\\*bold\\*\\*"));
        assert!(content.contains("\\[x\\]\\(http://evil\\)"));
        assert_eq!(j["username"], "THUGSred SMS Gateway");
        let long = build_request(&d, &ev(&"x".repeat(5000)), &Config::default()).unwrap();
        let (.., Body::Json(j)) = parts(long) else {
            panic!()
        };
        assert_eq!(j["content"].as_str().unwrap().chars().count(), 2000);
    }

    #[test]
    fn slack_teams_telegram_payloads() {
        let (.., body) =
            parts(build_request(&def("slack"), &ev(NASTY), &Config::default()).unwrap());
        let Body::Json(j) = body else { panic!() };
        let text = j["text"].as_str().unwrap();
        assert!(!text.contains("<!channel>") && !text.contains("<@U123>"));
        assert!(text.contains("&lt;!channel&gt;"));

        let (.., body) =
            parts(build_request(&def("teams"), &ev(NASTY), &Config::default()).unwrap());
        let Body::Json(j) = body else { panic!() };
        assert_eq!(j["type"], "message");
        assert_eq!(
            j["attachments"][0]["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        assert_eq!(j["attachments"][0]["content"]["type"], "AdaptiveCard");
        assert!(j["attachments"][0]["content"]["body"][1]["text"]
            .as_str()
            .unwrap()
            .contains("\\*\\*bold"));

        let mut t = def("telegram");
        t.url.clear();
        t.bot_token = "123:ABC".into();
        t.chat_id = "-100200".into();
        let (_, url, _, _, body) =
            parts(build_request(&t, &ev(NASTY), &Config::default()).unwrap());
        assert_eq!(url, "https://api.telegram.org/bot123:ABC/sendMessage");
        let Body::Json(j) = body else { panic!() };
        assert_eq!(j["chat_id"], "-100200");
        assert!(j.get("parse_mode").is_none());
        assert!(!j["text"].as_str().unwrap().contains("@everyone"));
    }

    #[test]
    fn anonymisation_never_leaks_listed_numbers() {
        let mut cfg = Config::default();
        cfg.contacts.push(crate::config::Contact {
            name: "Secret Person".into(),
            number: "+4500000000".into(),
            ..Default::default()
        });
        cfg.anonymous.numbers = vec!["+4500000000".into()];
        for kind in [
            "webhook", "discord", "slack", "teams", "telegram", "ntfy", "gotify", "syslog",
        ] {
            let mut d = def(kind);
            d.include_raw_pdu = true;
            d.bot_token = "1:A".into();
            d.chat_id = "1".into();
            d.topic = "t".into();
            d.token = "k".into();
            d.server = "127.0.0.1:514".into();
            let spec = build_request(&d, &ev("hi"), &cfg).unwrap();
            let dump = format!("{spec:?}");
            assert!(
                !dump.contains("4500000000"),
                "{kind} leaked the number: {dump}"
            );
            assert!(!dump.contains("Secret Person"), "{kind} leaked the name");
            assert!(!dump.contains("00FF"), "{kind} leaked the raw PDU");
            assert!(dump.contains("Anonymous"), "{kind}: {dump}");
        }
        // GET webhooks with query templates are covered as well.
        let mut g = def("webhook");
        g.method = "GET".into();
        g.query.insert("from".into(), "{{message.sender}}".into());
        assert!(
            !format!("{:?}", build_request(&g, &ev("hi"), &cfg).unwrap()).contains("4500000000")
        );
        // Body redaction for listed numbers is opt-in.
        cfg.anonymous.redact_body = true;
        let (.., Body::Json(j)) =
            parts(build_request(&def("webhook"), &ev("secret text"), &cfg).unwrap())
        else {
            panic!()
        };
        assert_eq!(j["data"]["message"]["body"], "[redacted]");
        assert_eq!(j["data"]["message"]["anonymized"], true);
    }

    #[test]
    fn per_integration_anonymise_and_body_switch() {
        let mut cfg = Config::default();
        cfg.contacts.push(crate::config::Contact {
            name: "Alice".into(),
            number: "+4500000000".into(),
            ..Default::default()
        });
        let mut d = def("webhook");
        let (.., Body::Json(j)) = parts(build_request(&d, &ev("hi"), &cfg).unwrap()) else {
            panic!()
        };
        assert_eq!(j["data"]["message"]["sender_name"], "Alice");
        assert_eq!(j["data"]["message"]["sender"], "+4500000000");
        d.anonymize = true;
        d.include_message_body = false;
        let (.., Body::Json(j)) = parts(build_request(&d, &ev("hi"), &cfg).unwrap()) else {
            panic!()
        };
        assert_eq!(j["data"]["message"]["sender"], "Anonymous");
        assert_eq!(j["data"]["message"]["sender_name"], Json::Null);
        assert_eq!(j["data"]["message"]["body"], "[redacted]");
        // calls
        let call = EventRecord {
            event_type: "call.missed".into(),
            data: json!({"call": {"number": "+4500000000"}}),
            ..ev("x")
        };
        let RequestSpec::Http {
            body: Body::Json(j),
            ..
        } = build_request(&def("slack"), &call, &cfg).unwrap()
        else {
            panic!()
        };
        assert_eq!(j["text"], "Missed call from Alice (+4500000000)");
        let RequestSpec::Http {
            body: Body::Json(j),
            ..
        } = build_request(&d, &call, &cfg).unwrap()
        else {
            panic!()
        };
        assert_eq!(j["data"]["call"]["number"], "Anonymous");
    }

    #[test]
    fn ntfy_gotify_syslog() {
        let mut n = def("ntfy");
        n.topic = "sms".into();
        n.token = "tk".into();
        let (_, url, headers, _, body) =
            parts(build_request(&n, &ev("hello"), &Config::default()).unwrap());
        assert_eq!(url, "https://example.invalid/hook/sms");
        assert!(headers.contains(&("Authorization".into(), "Bearer tk".into())));
        assert_eq!(body, Body::Text("SMS from +4500000000: hello".into()));

        let mut cfg = Config::default();
        cfg.contacts.push(crate::config::Contact {
            name: "Alice Example".into(),
            number: "+45 00 00 00 00".into(),
            ..Default::default()
        });
        let (.., body) = parts(build_request(&n, &ev("hello"), &cfg).unwrap());
        assert_eq!(
            body,
            Body::Text("SMS from Alice Example (+4500000000): hello".into())
        );

        let mut g = def("gotify");
        g.token = "app".into();
        let (_, url, headers, _, _) =
            parts(build_request(&g, &ev("hello"), &Config::default()).unwrap());
        assert_eq!(url, "https://example.invalid/hook/message");
        assert!(headers.contains(&("X-Gotify-Key".into(), "app".into())));

        let mut s = def("syslog");
        s.server = "127.0.0.1:514".into();
        let RequestSpec::Udp { payload, .. } =
            build_request(&s, &ev("line1\nline2"), &Config::default()).unwrap()
        else {
            panic!()
        };
        assert!(payload.starts_with("<134>1 2025-01-01T00:00:00.000Z"));
        assert!(!payload.contains('\n'));
    }
}

//! GatewayAPI (gatewayapi.com / gatewayapi.eu) Mobile Messaging API client.
//!
//! Sending: `POST /mobile/single` with `Authorization: Token <token>`; the
//! API answers `202` with the message id. Delivery states arrive as signed
//! webhooks (`message.status.sms`); the API offers no polling.

use super::{Outgoing, SendFailure, StatusUpdate};
use crate::config::ProviderDef;
use serde_json::{json, Value};
use std::time::Duration;

/// Longest validity the API accepts.
const MAX_VALIDITY_MINUTES: u64 = 5 * 24 * 60;

pub fn base_url(def: &ProviderDef) -> String {
    if !def.base_url.is_empty() {
        return def.base_url.trim_end_matches('/').to_string();
    }
    match def.region.as_str() {
        "eu" => "https://messaging.gatewayapi.eu".into(),
        _ => "https://messaging.gatewayapi.com".into(),
    }
}

/// ISO 8601 duration for a validity in minutes.
fn iso_duration(minutes: u64) -> String {
    let m = minutes.clamp(1, MAX_VALIDITY_MINUTES);
    let (d, h, min) = (m / 1440, (m % 1440) / 60, m % 60);
    let mut out = String::from("P");
    if d > 0 {
        out.push_str(&format!("{d}D"));
    }
    if h > 0 || min > 0 {
        out.push('T');
        if h > 0 {
            out.push_str(&format!("{h}H"));
        }
        if min > 0 {
            out.push_str(&format!("{min}M"));
        }
    }
    out
}

/// Request body for one message.
pub fn request_body(def: &ProviderDef, m: &Outgoing<'_>) -> Result<Value, String> {
    let recipient: u64 =
        m.to.trim_start_matches('+')
            .parse()
            .map_err(|_| "destination is not an international number".to_string())?;
    let mut body = json!({
        "sender": m.sender,
        "recipient": recipient,
        "message": m.text,
        "reference": m.reference,
    });
    if let Some(v) = m.validity_minutes {
        body["expiration"] = json!(iso_duration(v));
    }
    if def.priority == "urgent" {
        body["priority"] = json!("urgent");
    }
    if !def.label.is_empty() {
        body["label"] = json!(def.label);
    }
    Ok(body)
}

/// Short, single-line description of an error response.
fn describe(status: u16, text: &str) -> String {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let detail = if let Some(first) = v["detail"].as_array().and_then(|a| a.first()) {
        // Validation error: name the offending field.
        let field = first["loc"]
            .as_array()
            .and_then(|l| l.last())
            .map(|f| f.to_string().trim_matches('"').to_string())
            .unwrap_or_default();
        format!("{field}: {}", first["msg"].as_str().unwrap_or("invalid"))
    } else if let Some(msg) = v["message"]
        .as_str()
        .or(v["detail"].as_str())
        .or(v["description"].as_str())
    {
        match v["code"].as_str() {
            Some(code) => format!("{msg} ({code})"),
            None => msg.to_string(),
        }
    } else {
        text.chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect::<String>()
    };
    let detail = detail.trim();
    if detail.is_empty() {
        format!("provider answered HTTP {status}")
    } else {
        format!("provider answered HTTP {status}: {detail}")
    }
}

/// Submit one message. Returns the provider's message id when it gave one.
pub async fn send(
    http: &reqwest::Client,
    def: &ProviderDef,
    m: &Outgoing<'_>,
) -> Result<Option<String>, SendFailure> {
    let body = request_body(def, m).map_err(SendFailure::Permanent)?;
    let url = format!("{}/mobile/single", base_url(def));
    let sent = http
        .post(&url)
        .header("Authorization", format!("Token {}", def.api_token))
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(def.timeout_seconds.max(1)))
        .json(&body)
        .send()
        .await;
    let resp = match sent {
        Ok(r) => r,
        // Nothing was transmitted when the connection could not be opened.
        Err(e) if e.is_connect() => {
            return Err(SendFailure::Retry(
                "could not connect to the provider".into(),
            ))
        }
        Err(e) if e.is_timeout() => {
            return Err(SendFailure::Ambiguous(
                "the provider did not answer in time".into(),
            ))
        }
        Err(_) => {
            return Err(SendFailure::Ambiguous(
                "the connection to the provider failed during the request".into(),
            ))
        }
    };
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    match status {
        200..=299 => {
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            Ok(v["msg_id"].as_str().map(str::to_string))
        }
        // Throttling and server-side failures are worth another attempt.
        408 | 429 | 500..=599 => Err(SendFailure::Retry(describe(status, &text))),
        401 | 403 => Err(SendFailure::Permanent(format!(
            "{} (check the API token, account credit and allowed destinations)",
            describe(status, &text)
        ))),
        _ => Err(SendFailure::Permanent(describe(status, &text))),
    }
}

/// Map a GatewayAPI delivery state to the gateway's categories.
pub fn category(status: &str) -> &'static str {
    match status {
        "DELIVERED" | "ACCEPTED" => "delivered",
        "EXPIRED" | "UNDELIVERABLE" | "REJECTED" | "DELETED" | "SKIPPED" => "failed",
        // ENROUTE, BUFFERED, SCHEDULED, UNKNOWN and anything new.
        _ => "pending",
    }
}

/// Parse a webhook body. `Ok(None)` is a well-formed event the gateway does
/// not use (for example an RCS or inbound-message event).
pub fn parse_callback(body: &[u8]) -> Result<Option<StatusUpdate>, String> {
    let v: Value =
        serde_json::from_slice(body).map_err(|_| "the body is not valid JSON".to_string())?;
    let Some(kind) = v["event_type"].as_str() else {
        return Err("event_type is missing".into());
    };
    if kind != "message.status.sms" {
        return Ok(None);
    }
    let e = &v["event"];
    let Some(status) = e["status"].as_str() else {
        return Err("event.status is missing".into());
    };
    let Some(reference) = e["reference"].as_str().filter(|r| !r.is_empty()) else {
        // A message that was not sent by this gateway.
        return Ok(None);
    };
    let provider_message_id = e["msg_id"].as_str().unwrap_or_default().to_string();
    let detail = match (
        e["error"]["details"].as_str(),
        e["error"]["hex_code"].as_str(),
    ) {
        (Some(d), Some(c)) => Some(format!("{d} ({c})")),
        (Some(d), None) => Some(d.to_string()),
        (None, Some(c)) => Some(c.to_string()),
        (None, None) => None,
    };
    Ok(Some(StatusUpdate {
        event_id: v["event_id"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| provider_message_id.clone()),
        reference: reference.to_string(),
        provider_message_id,
        recipient: e["recipient"]
            .as_u64()
            .map(|n| format!("+{n}"))
            .unwrap_or_default(),
        status: status.chars().take(32).collect(),
        category: category(status),
        detail: detail.map(|d| d.chars().take(200).collect()),
        status_at: e["status_at"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| crate::utils::ts(t.with_timezone(&chrono::Utc))),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> ProviderDef {
        ProviderDef {
            name: "gw".into(),
            ..Default::default()
        }
    }

    #[test]
    fn urls() {
        let mut d = def();
        assert_eq!(base_url(&d), "https://messaging.gatewayapi.com");
        d.region = "eu".into();
        assert_eq!(base_url(&d), "https://messaging.gatewayapi.eu");
        d.base_url = "http://127.0.0.1:9/".into();
        assert_eq!(base_url(&d), "http://127.0.0.1:9");
    }

    #[test]
    fn durations() {
        assert_eq!(iso_duration(1), "PT1M");
        assert_eq!(iso_duration(60), "PT1H");
        assert_eq!(iso_duration(1500), "P1DT1H");
        assert_eq!(iso_duration(1440 * 5), "P5D");
        assert_eq!(iso_duration(999_999), "P5D");
    }

    #[test]
    fn body() {
        let mut d = def();
        let m = Outgoing {
            to: "+4500000000",
            sender: "Example",
            text: "Hello",
            reference: "id-1",
            validity_minutes: Some(90),
        };
        let b = request_body(&d, &m).unwrap();
        assert_eq!(
            b,
            json!({"sender": "Example", "recipient": 4500000000u64, "message": "Hello", "reference": "id-1", "expiration": "PT1H30M"})
        );
        d.priority = "urgent".into();
        d.label = "ops".into();
        let b = request_body(&d, &m).unwrap();
        assert_eq!(
            (b["priority"].as_str(), b["label"].as_str()),
            (Some("urgent"), Some("ops"))
        );
        assert!(request_body(&d, &Outgoing { to: "abc", ..m }).is_err());
    }

    #[test]
    fn errors() {
        assert_eq!(
            describe(
                422,
                r#"{"detail":[{"loc":["body","sender"],"msg":"String too long"}]}"#
            ),
            "provider answered HTTP 422: sender: String too long"
        );
        assert_eq!(
            describe(403, r#"{"code":"0x0216","message":"Insufficient credit"}"#),
            "provider answered HTTP 403: Insufficient credit (0x0216)"
        );
        assert_eq!(describe(500, ""), "provider answered HTTP 500");
        assert_eq!(
            describe(403, r#"{"description":"Forbidden","detail":[]}"#),
            "provider answered HTTP 403: Forbidden"
        );
    }

    #[test]
    fn callbacks() {
        let ok = br#"{"event_id":"ev1","timestamp":"2025-04-01T11:50:12+00:00","event_type":"message.status.sms","event":{"msg_id":"01ABC","recipient":4500000000,"reference":"id-1","status":"DELIVERED","status_at":"2025-04-01T11:50:11+00:00","error":null}}"#;
        let u = parse_callback(ok).unwrap().unwrap();
        assert_eq!(u.category, "delivered");
        assert_eq!(u.reference, "id-1");
        assert_eq!(u.recipient, "+4500000000");
        assert_eq!(u.event_id, "ev1");
        assert!(u.status_at.is_some());

        let failed = br#"{"event_id":"ev2","event_type":"message.status.sms","event":{"msg_id":"01ABC","recipient":4500000000,"reference":"id-1","status":"REJECTED","error":{"hex_code":"0x1905","details":"Unsupported sender"}}}"#;
        let u = parse_callback(failed).unwrap().unwrap();
        assert_eq!(u.category, "failed");
        assert_eq!(u.detail.as_deref(), Some("Unsupported sender (0x1905)"));

        assert_eq!(category("ENROUTE"), "pending");
        assert_eq!(category("SOMETHING_NEW"), "pending");
        let other = br#"{"event_type":"user-message.text.sms","event":{}}"#;
        assert_eq!(parse_callback(other).unwrap(), None);
        let foreign = br#"{"event_type":"message.status.sms","event":{"status":"DELIVERED","reference":null}}"#;
        assert_eq!(parse_callback(foreign).unwrap(), None);
        assert!(parse_callback(b"nope").is_err());
        assert!(parse_callback(b"{}").is_err());
    }
}

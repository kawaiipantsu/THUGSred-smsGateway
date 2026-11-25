//! SMS provider route (GatewayAPI): submission, sender policy, failure
//! handling, quotas and signed delivery callbacks. The provider is a local
//! mock; nothing leaves the machine.

mod common;

use common::{mock_server, serve, start, wait, Setup};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

fn provider_conf(base: &str, extra: &str) -> String {
    format!(
        "[[provider]]\nname = \"gw\"\ntype = \"gatewayapi\"\nenabled = true\napi_token = \"test-token\"\nbase_url = \"{base}\"\ndefault_sender = \"Example\"\nallowed_senders = [\"Other\"]\ncallback_secret = \"cb-secret\"\ndaily_sms_limit = 50\n{extra}"
    )
}

async fn post(base: &str, path: &str, body: Value) -> (u16, Value) {
    let r = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

fn sign(secret: &str, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    format!("v1={}", hex::encode(mac.finalize().into_bytes()))
}

async fn callback(base: &str, signature: Option<&str>, body: &str) -> (u16, Value) {
    let mut r = reqwest::Client::new()
        .post(format!("{base}/api/v1/providers/gw/callback"))
        .header("content-type", "application/json")
        .body(body.to_string());
    if let Some(s) = signature {
        r = r.header("Signature", s);
    }
    let r = r.send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

fn status_event(event_id: &str, reference: &str, status: &str) -> String {
    json!({"event_id": event_id, "timestamp": "2025-04-01T11:50:12+00:00", "event_type": "message.status.sms",
        "event": {"msg_id": "01TEST", "recipient": 4500000001u64, "reference": reference, "status": status,
                  "status_at": "2025-04-01T11:50:11+00:00",
                  "error": if status == "REJECTED" { json!({"hex_code": "0x1905", "details": "Unsupported sender"}) } else { Value::Null }}})
    .to_string()
}

#[tokio::test]
async fn sends_with_sender_name_and_applies_signed_delivery_callbacks() {
    let (url, mock) = mock_server().await;
    mock.set_status(202);
    let h = start(Setup {
        providers: provider_conf(&url, ""),
        ..Default::default()
    })
    .await;
    h.ready().await;
    let base = serve(&h).await;

    let (code, v) = post(
        &base,
        "/api/v1/sms/send",
        json!({"to": "+45 00 00 00 01", "message": "Hello æøå", "provider": "gw", "validity_minutes": 90}),
    )
    .await;
    assert_eq!(code, 202, "{v}");
    assert_eq!(v["provider"], "gw");
    assert_eq!(v["sender"], "Example");
    assert_eq!(v["modem"], "provider:gw");
    let id = v["message_id"].as_str().unwrap().to_string();
    h.wait_status(&id, "submitted").await;

    // Exactly one request reached the provider, with the token and sender.
    assert_eq!(mock.count(), 1);
    let req = mock.requests.lock().unwrap()[0].clone();
    assert_eq!(
        (req.method.as_str(), req.path.as_str()),
        ("POST", "/mobile/single")
    );
    assert_eq!(req.headers["authorization"], "Token test-token");
    let sent: Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(
        sent,
        json!({"sender": "Example", "recipient": 4500000001u64, "message": "Hello æøå", "reference": id, "expiration": "PT1H30M"})
    );
    // The modem was not used.
    assert!(h.sim().sent().is_empty());

    // The token never appears in API output.
    let r = reqwest::get(format!("{base}/api/v1/providers"))
        .await
        .unwrap();
    let text = r.text().await.unwrap();
    assert!(!text.contains("test-token") && !text.contains("cb-secret"));
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["data"]["providers"][0]["segments_today"], 1);
    let text = reqwest::get(format!("{base}/api/v1/config"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!text.contains("test-token") && !text.contains("cb-secret"));

    // Callbacks: unsigned and wrongly signed requests are rejected.
    let pending = status_event("ev-0", &id, "ENROUTE");
    assert_eq!(callback(&base, None, &pending).await.0, 401);
    assert_eq!(
        callback(&base, Some(&sign("wrong", &pending)), &pending)
            .await
            .0,
        401
    );
    let (code, v) = callback(&base, Some(&sign("cb-secret", &pending)), &pending).await;
    assert_eq!((code, v["data"]["result"].as_str()), (200, Some("noted")));
    assert_eq!(h.message_status(&id).await, "submitted");

    let done = status_event("ev-1", &id, "DELIVERED");
    let (code, v) = callback(&base, Some(&sign("cb-secret", &done)), &done).await;
    assert_eq!((code, v["data"]["result"].as_str()), (200, Some("applied")));
    assert_eq!(h.message_status(&id).await, "delivered");
    assert_eq!(h.events("sms.delivered").await.len(), 1);

    // A later contradicting report never overwrites a final state.
    let late = status_event("ev-2", &id, "REJECTED");
    let (code, v) = callback(&base, Some(&sign("cb-secret", &late)), &late).await;
    assert_eq!(
        (code, v["data"]["result"].as_str()),
        (200, Some("already_final"))
    );
    assert_eq!(h.message_status(&id).await, "delivered");

    // Reports for messages of other routes are acknowledged and ignored.
    let foreign = status_event("ev-3", "no-such-message", "DELIVERED");
    let (code, v) = callback(&base, Some(&sign("cb-secret", &foreign)), &foreign).await;
    assert_eq!(
        (code, v["data"]["result"].as_str()),
        (200, Some("unknown_message"))
    );
    h.stop().await;
}

#[tokio::test]
async fn sender_policy_and_request_validation() {
    let (url, mock) = mock_server().await;
    mock.set_status(202);
    let h = start(Setup {
        providers: provider_conf(&url, ""),
        ..Default::default()
    })
    .await;
    h.ready().await;
    let base = serve(&h).await;
    let send = |body: Value| post(&base, "/api/v1/sms/send", body);

    // A listed sender is accepted and selects the provider by itself.
    let (code, v) =
        send(json!({"to": "+4500000001", "message": "a", "sender_name": "Other"})).await;
    assert_eq!(
        (code, v["sender"].as_str(), v["provider"].as_str()),
        (202, Some("Other"), Some("gw"))
    );
    // Anything else is refused.
    let (code, v) =
        send(json!({"to": "+4500000001", "message": "a", "sender_name": "MyBank"})).await;
    assert_eq!(code, 403, "{v}");
    let (code, _) =
        send(json!({"to": "+4500000001", "message": "a", "sender_name": "Bad!Name"})).await;
    assert_eq!(code, 400);
    let (code, _) = send(json!({"to": "+4500000001", "message": "a", "provider": "nope"})).await;
    assert_eq!(code, 400);
    // Modem-only features and conflicting routes.
    for body in [
        json!({"to": "+4500000001", "binary": "0102", "provider": "gw"}),
        json!({"to": "+4500000001", "message": "a", "provider": "gw", "flash": true}),
        json!({"to": "+4500000001", "message": "a", "provider": "gw", "destination_port": 5000}),
        json!({"to": "+4500000001", "message": "a", "provider": "gw", "modem": "modem-1"}),
        json!({"to": "12345678", "message": "a", "provider": "gw"}),
        json!({"to": "+4500000001", "message": "a", "provider": "gw", "validity_minutes": 10000}),
    ] {
        let (code, v) = send(body.clone()).await;
        assert_eq!(code, 400, "{body} -> {v}");
    }
    // Without the provider fields the modem is used as before.
    let (code, v) = send(json!({"to": "+4500000001", "message": "via modem"})).await;
    assert_eq!((code, v["provider"].is_null()), (202, true));
    h.wait_status(v["message_id"].as_str().unwrap(), "submitted")
        .await;
    assert_eq!(h.sim().sent().len(), 1);
    h.stop().await;

    // No provider configured: a sender name cannot be honoured.
    let h = start(Setup::default()).await;
    h.ready().await;
    let base = serve(&h).await;
    let (code, v) = post(
        &base,
        "/api/v1/sms/send",
        json!({"to": "+4500000001", "message": "a", "sender_name": "Example"}),
    )
    .await;
    assert_eq!(code, 400, "{v}");
    let pending = status_event("ev", "x", "DELIVERED");
    assert_eq!(
        callback(&base, Some(&sign("cb-secret", &pending)), &pending)
            .await
            .0,
        404
    );
    h.stop().await;
}

#[tokio::test]
async fn provider_failures_retries_and_quota() {
    let (url, mock) = mock_server().await;
    let h = start(Setup {
        providers: provider_conf(&url, "monthly_sms_limit = 3\n"),
        overrides: vec![
            "sms.max_send_attempts=2".into(),
            "sms.send_retry_delay_seconds=1".into(),
        ],
        ..Default::default()
    })
    .await;
    h.ready().await;
    let base = serve(&h).await;
    let send = || {
        post(
            &base,
            "/api/v1/sms/send",
            json!({"to": "+4500000001", "message": "a", "provider": "gw"}),
        )
    };

    // A refusal is final: one request, no retry.
    mock.set_status(422);
    let (_, v) = send().await;
    let id = v["message_id"].as_str().unwrap().to_string();
    h.wait_status(&id, "failed").await;
    assert_eq!(mock.count(), 1);

    // A server error is retried, then succeeds.
    mock.set_status(503);
    let (_, v) = send().await;
    let id = v["message_id"].as_str().unwrap().to_string();
    h.wait_status(&id, "retrying").await;
    mock.set_status(202);
    h.wait_status(&id, "submitted").await;
    assert_eq!(mock.count(), 3);

    // Authentication problems are final and explain themselves.
    mock.set_status(403);
    let (_, v) = send().await;
    let id = v["message_id"].as_str().unwrap().to_string();
    h.wait_status(&id, "failed").await;
    let m = h.gw.db.get_message(&id).await.unwrap().unwrap();
    assert!(m.error.unwrap().contains("API token"));

    // Spending protection counts submitted segments only (1 so far).
    mock.set_status(202);
    for _ in 0..2 {
        let (code, v) = send().await;
        assert_eq!(code, 202, "{v}");
        h.wait_status(v["message_id"].as_str().unwrap(), "submitted")
            .await;
    }
    let (code, v) = send().await;
    assert_eq!(code, 429, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("monthly"));
    h.stop().await;
}

#[tokio::test]
async fn callback_is_reachable_from_outside_the_allowed_networks_and_reload_applies() {
    let (url, mock) = mock_server().await;
    mock.set_status(202);
    let h = start(Setup {
        providers: provider_conf(&url, ""),
        overrides: vec!["api.allowed_cidrs=[\"10.0.0.0/8\"]".into()],
        ..Default::default()
    })
    .await;
    h.ready().await;
    let base = serve(&h).await;
    // Ordinary API calls from this address are refused...
    let r = reqwest::get(format!("{base}/api/v1/status")).await.unwrap();
    assert_eq!(r.status().as_u16(), 403);
    // ...but a provider callback reaches its signature check.
    let ev = status_event("ev", "unknown", "DELIVERED");
    assert_eq!(callback(&base, None, &ev).await.0, 401);
    assert_eq!(
        callback(&base, Some(&sign("cb-secret", &ev)), &ev).await.0,
        200
    );
    // Only POST on the exact callback path is exempt.
    let r = reqwest::get(format!("{base}/api/v1/providers/gw/callback"))
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);

    // Changing the token in providers.conf takes effect on reload.
    let path = h.dir.path().join("providers.conf");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("test-token", "rotated-token")).unwrap();
    h.gw.reload().await.unwrap();
    let done =
        h.gw.sms
            .submit(thugsred_smsgateway::sms::SendRequest {
                to: "+4500000001".into(),
                message: Some("after reload".into()),
                provider: Some("gw".into()),
                origin: "test".into(),
                ..Default::default()
            })
            .await
            .unwrap();
    h.wait_status(&done.message.id, "submitted").await;
    wait("provider request", 5, || async { mock.count() == 1 }).await;
    assert_eq!(
        mock.requests.lock().unwrap()[0].headers["authorization"],
        "Token rotated-token"
    );
    h.stop().await;
}

//! REST API tests: validation, idempotency, access control, exports.

mod common;

use common::{mock_server, serve, start, ts, wait, Setup};
use serde_json::{json, Value};
use thugsred_smsgateway::sms::pdu::{encode_deliver, DeliverSpec};

async fn get(base: &str, path: &str) -> (u16, Value) {
    let r = reqwest::get(format!("{base}{path}")).await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
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

#[tokio::test]
async fn health_status_metrics_and_docs() {
    let h = start(Setup::default()).await;
    h.ready().await;
    let base = serve(&h).await;
    let (code, v) = get(&base, "/health").await;
    assert_eq!((code, v["status"].as_str()), (200, Some("ok")));
    let (code, v) = get(&base, "/ready").await;
    assert_eq!((code, v["ready"].as_bool()), (200, Some(true)));
    let (_, v) = get(&base, "/api/v1/status").await;
    assert_eq!(v["data"]["modems"][0]["state"], "READY");
    assert_eq!(v["data"]["database"]["healthy"], true);
    let (_, v) = get(&base, "/api/v1/version").await;
    assert_eq!(v["data"]["version"], env!("CARGO_PKG_VERSION"));
    let (_, v) = get(&base, "/api/v1/capabilities").await;
    assert_eq!(
        v["data"]["modems"][0]["capabilities"]["sms_modes"][0],
        "pdu"
    );
    assert_eq!(v["data"]["gateway"]["mms_sending"], "unsupported");
    let (_, v) = get(&base, "/api/v1/openapi.json").await;
    assert_eq!(v["openapi"], "3.0.3");
    let (_, v) = get(&base, "/api/v1/sim").await;
    assert_eq!(v["data"]["state"], "ready");
    assert!(v.to_string().find("\"pin\"").is_none());
    for range in ["24h", "7d", "30d"] {
        let (code, v) = get(&base, &format!("/api/v1/stats/activity?range={range}")).await;
        assert_eq!(code, 200);
        assert!(v["data"]["buckets"].as_array().unwrap().len() >= 7);
    }
    let r = reqwest::get(format!("{base}/metrics")).await.unwrap();
    assert!(r.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
    let text = r.text().await.unwrap();
    assert!(text.contains("smsgw_modem_connected{modem=\"modem-1\"} 1"));
    assert!(text.contains("smsgw_api_requests_total"));
    assert!(
        !text.contains("+45"),
        "no telephone numbers in metric labels"
    );
    // every response carries a request id and security headers
    let r = reqwest::Client::new()
        .get(format!("{base}/api/v1/nope"))
        .header("X-Request-Id", "abc-123")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(r.headers()["x-request-id"], "abc-123");
    assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    assert!(r.headers().contains_key("content-security-policy"));
    let v: Value = r.json().await.unwrap();
    assert_eq!(
        (v["success"].as_bool(), v["error"]["code"].as_str()),
        (Some(false), Some("not_found"))
    );
    // dashboard is served from embedded assets
    let r = reqwest::get(format!("{base}/")).await.unwrap();
    assert!(r.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    assert_eq!(
        reqwest::get(format!("{base}/assets/../../Cargo.toml"))
            .await
            .unwrap()
            .status(),
        404
    );
    h.stop().await;
}

#[tokio::test]
async fn send_validation_idempotency_and_lifecycle() {
    let h = start(Setup::default()).await;
    h.ready().await;
    let base = serve(&h).await;
    for (body, needle) in [
        (json!({"message": "x"}), "telephone number"),
        (json!({"to": "+4500000000"}), "message is required"),
        (
            json!({"to": "+4500000000", "message": ""}),
            "must not be empty",
        ),
        (
            json!({"to": "+4500000000", "message": "x", "bogus": 1}),
            "unknown field",
        ),
        (
            json!({"to": "+4500000000", "message": "x", "priority": "urgent"}),
            "priority",
        ),
        (
            json!({"to": "+4500000000", "message": "x", "scheduled_at": "tomorrow"}),
            "RFC 3339",
        ),
        (
            json!({"to": "+4500000000; DROP TABLE messages", "message": "x"}),
            "telephone number",
        ),
        (
            json!({"to": "+4500000000", "message": "x", "from": "+4599999999"}),
            "no configured SIM",
        ),
        (
            json!({"to": "+4500000000", "message": "x", "protocol_identifier": 64}),
            "allow_raw_pdu_submission",
        ),
    ] {
        let (code, v) = post(&base, "/api/v1/sms/send", body.clone()).await;
        assert!(code == 400 || code == 403, "{body} -> {code}");
        assert_eq!(v["success"], false);
        assert!(
            v["error"]["message"].as_str().unwrap().contains(needle),
            "{body} -> {v}"
        );
    }
    let r = reqwest::Client::new()
        .post(format!("{base}/api/v1/sms/send"))
        .body("to=1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 415);
    let r = reqwest::Client::new()
        .post(format!("{base}/api/v1/sms/send"))
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "invalid_request"
    );

    let (code, est) = post(
        &base,
        "/api/v1/sms/estimate",
        json!({"message": "héllo wörld €"}),
    )
    .await;
    assert_eq!(
        (
            code,
            est["data"]["encoding"].as_str(),
            est["data"]["parts"].as_i64()
        ),
        (200, Some("gsm7"), Some(1))
    );
    let (_, est) = post(
        &base,
        "/api/v1/sms/estimate",
        json!({"message": "😀".repeat(40)}),
    )
    .await;
    assert_eq!(
        (
            est["data"]["encoding"].as_str(),
            est["data"]["parts"].as_i64()
        ),
        (Some("ucs2"), Some(2))
    );

    let client = reqwest::Client::new();
    let send = || {
        client.post(format!("{base}/api/v1/sms/send")).header("Idempotency-Key", "example-message-001").json(&json!({"to": "+4500000000", "message": "Hello from THUGSred SMS Gateway", "encoding": "auto", "delivery_report": true, "priority": "normal"})).send()
    };
    let r = send().await.unwrap();
    assert_eq!(r.status(), 202);
    let v: Value = r.json().await.unwrap();
    assert_eq!(
        (
            v["success"].as_bool(),
            v["status"].as_str(),
            v["encoding"].as_str(),
            v["parts"].as_i64(),
            v["duplicate"].as_bool()
        ),
        (
            Some(true),
            Some("queued"),
            Some("gsm7"),
            Some(1),
            Some(false)
        )
    );
    let id = v["message_id"].as_str().unwrap().to_string();
    let again: Value = send().await.unwrap().json().await.unwrap();
    assert_eq!(
        (again["message_id"].as_str(), again["duplicate"].as_bool()),
        (Some(id.as_str()), Some(true))
    );
    h.wait_status(&id, "delivered").await;
    assert_eq!(h.sim().sent().len(), 1);

    let (_, st) = get(&base, &format!("/api/v1/messages/{id}/status")).await;
    assert_eq!(st["data"]["status"], "delivered");
    assert!(st["data"]["history"].as_array().unwrap().len() >= 3);
    assert_eq!(st["data"]["delivery_reports"][0]["category"], "delivered");
    let (_, m) = get(&base, &format!("/api/v1/messages/{id}")).await;
    assert_eq!(m["data"]["decoded_pdu"]["type"], "submit");
    assert_eq!(
        m["data"]["decoded_pdu"]["recipient"]["number"],
        "+4500000000"
    );
    let (_, parts) = get(&base, &format!("/api/v1/messages/{id}/parts")).await;
    assert_eq!(parts["data"].as_array().unwrap().len(), 1);
    let (code, _) = get(&base, "/api/v1/messages/does-not-exist").await;
    assert_eq!(code, 404);

    // scheduling and cancellation
    let (code, _) = post(
        &base,
        "/api/v1/sms/schedule",
        json!({"to": "+4500000000", "message": "x"}),
    )
    .await;
    assert_eq!(code, 400);
    let (code, v) = post(
        &base,
        "/api/v1/sms/schedule",
        json!({"to": "+4500000000", "message": "later", "scheduled_at": "2099-01-01T00:00:00Z"}),
    )
    .await;
    assert_eq!(code, 400, "more than a year ahead is rejected: {v}");
    let soon = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let (code, v) = post(
        &base,
        "/api/v1/sms/schedule",
        json!({"to": "+4500000000", "message": "later", "scheduled_at": soon}),
    )
    .await;
    assert_eq!((code, v["status"].as_str()), (202, Some("scheduled")));
    let sid = v["message_id"].as_str().unwrap();
    let (_, q) = get(&base, "/api/v1/queue").await;
    assert_eq!(q["pagination"]["total"], 1);
    assert_eq!(q["data"][0]["message_id"], sid);
    let (code, _) = post(&base, &format!("/api/v1/queue/{sid}/cancel"), Value::Null).await;
    assert_eq!(code, 200);
    let (code, v) = post(&base, &format!("/api/v1/queue/{sid}/cancel"), Value::Null).await;
    assert_eq!((code, v["error"]["code"].as_str()), (409, Some("conflict")));
    let (code, _) = post(&base, &format!("/api/v1/queue/{sid}/retry"), Value::Null).await;
    assert_eq!(code, 200);
    let (_, audit) = get(&base, "/api/v1/audit").await;
    assert!(audit["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["action"] == "sms.submit"));
    h.stop().await;
}

#[tokio::test]
async fn history_search_export_and_untrusted_content() {
    let h = start(Setup::default()).await;
    h.ready().await;
    let base = serve(&h).await;
    let nasty = "=HYPERLINK(\"http://x\") <script>alert(1)</script> \"q\", 100%";
    for (sender, text) in [
        ("+4500000001", nasty),
        ("+4500000002", "second message"),
        ("@Evil", "+SUM(1)"),
    ] {
        h.sim()
            .incoming_sms(&encode_deliver(&DeliverSpec::text(sender, text, ts())).unwrap()[0]);
    }
    h.wait_inbound(3).await;
    let (_, v) = get(&base, "/api/v1/messages?direction=inbound&order=asc").await;
    assert_eq!(v["pagination"]["total"], 3);
    assert_eq!(
        v["data"][0]["body"], nasty,
        "content is returned verbatim as JSON data"
    );
    let (_, v) = get(&base, "/api/v1/messages?search=second").await;
    assert_eq!(v["pagination"]["total"], 1);
    let (_, v) = get(&base, "/api/v1/messages?search=100%25").await;
    assert_eq!(v["pagination"]["total"], 1);
    let (_, v) = get(&base, "/api/v1/messages?sender=%2B4500000002&limit=1").await;
    assert_eq!(
        (
            v["pagination"]["total"].as_i64(),
            v["pagination"]["limit"].as_i64()
        ),
        (Some(1), Some(1))
    );
    let (_, v) = get(&base, "/api/v1/messages?from=2000-01-01&to=2000-12-31").await;
    assert_eq!(v["pagination"]["total"], 0);
    let (code, _) = get(&base, "/api/v1/messages?from=yesterday").await;
    assert_eq!(code, 400);
    let (code, _) = get(&base, "/api/v1/messages?direction=sideways").await;
    assert_eq!(code, 400);
    let (code, v) = get(&base, "/api/v1/messages?limit=abc").await;
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (400, Some("invalid_request"))
    );
    let (_, v) = get(&base, "/api/v1/messages?search=x%27%20OR%20%271%27%3D%271").await;
    assert_eq!(v["pagination"]["total"], 0, "SQL metacharacters are data");

    let r = reqwest::get(format!("{base}/api/v1/history/export?format=csv&order=asc"))
        .await
        .unwrap();
    assert!(r.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/csv"));
    assert!(r.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .contains("attachment"));
    let csv = r.text().await.unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 4);
    let evil = lines
        .iter()
        .find(|l| l.contains("Evil"))
        .expect("row for the alphanumeric sender");
    assert!(
        evil.contains("\"'@Evil\"") && evil.contains("\"'+SUM(1)\""),
        "{evil}"
    );
    let first = lines.iter().find(|l| l.contains("HYPERLINK")).unwrap();
    assert!(
        first.contains("\"'=HYPERLINK(\"\"http://x\"\")"),
        "formula neutralised and quotes doubled: {first}"
    );
    assert!(
        first.contains("\"'+4500000001\""),
        "leading plus is neutralised too"
    );
    let (_, v) = get(&base, "/api/v1/history/export?format=json").await;
    assert_eq!(v["count"], 3);
    let (code, _) = get(&base, "/api/v1/history/export?format=xml").await;
    assert_eq!(code, 400);

    // events and deletion
    let (_, ev) = get(&base, "/api/v1/events?type=sms.").await;
    assert_eq!(ev["pagination"]["total"], 3);
    let id = v["messages"][0]["id"].as_str().unwrap();
    let r = reqwest::Client::new()
        .delete(format!("{base}/api/v1/messages/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let (code, _) = get(&base, &format!("/api/v1/messages/{id}")).await;
    assert_eq!(code, 404);
    let (_, ev) = get(&base, "/api/v1/events?type=sms.").await;
    assert_eq!(
        ev["pagination"]["total"], 2,
        "events carrying the deleted message's content are removed with it"
    );
    h.stop().await;
}

#[tokio::test]
async fn pdu_tools_are_offline() {
    let h = start(Setup::default()).await;
    h.ready().await;
    let base = serve(&h).await;
    let (code, v) = post(
        &base,
        "/api/v1/pdu/encode",
        json!({"to": "+46708251358", "message": "hellohello", "validity_minutes": 5760}),
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(v["data"]["transmitted"], false);
    assert_eq!(
        v["data"]["encoded"]["parts"][0]["pdu"],
        "0011000B916407281553F80000AA0AE8329BFD4697D9EC37"
    );
    let (_, v) = post(
        &base,
        "/api/v1/pdu/decode",
        json!({"pdu": "07917283010010F5040BC87238880900F10000993092516195800AE8329BFD4697D9EC37"}),
    )
    .await;
    assert_eq!(v["data"]["decoded"]["type"], "deliver");
    assert_eq!(v["data"]["decoded"]["user_data"]["text"], "hellohello");
    let (code, v) = post(&base, "/api/v1/pdu/decode", json!({"pdu": "0791ZZ"})).await;
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (422, Some("invalid_pdu"))
    );
    let (code, _) = post(
        &base,
        "/api/v1/pdu/encode",
        json!({"to": "abc", "message": "x"}),
    )
    .await;
    assert_eq!(code, 400);
    assert!(h.sim().sent().is_empty(), "PDU tools never transmit");
    // privileged operations are off by default
    let (code, v) = post(&base, "/api/v1/modem/at", json!({"command": "AT+CSQ"})).await;
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (403, Some("forbidden"))
    );
    let (code, _) = post(
        &base,
        "/api/v1/sms/send-pdu",
        json!({"pdu": "0011000B916407281553F80000AA0AE8329BFD4697D9EC37"}),
    )
    .await;
    assert_eq!(code, 403);
    assert!(h.sim().sent().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn raw_at_and_ussd_when_enabled() {
    let h = start(Setup {
        main: "[security]\nallow_raw_at_commands = true\n[ussd.commands]\nbalance = \"*101#\"\n"
            .into(),
        ..Default::default()
    })
    .await;
    h.ready().await;
    let base = serve(&h).await;
    let (code, v) = post(&base, "/api/v1/modem/at", json!({"command": "AT+CSQ"})).await;
    assert_eq!(code, 200);
    assert_eq!(v["data"]["response"][0], "+CSQ: 20,0");
    for bad in [
        "AT+CPIN=\"0000\"",
        "AT+CMGS=10",
        "AT\r\nATD123",
        "echo hi",
        "AT+CLCK=\"SC\",1,\"1234\"",
    ] {
        let (code, _) = post(&base, "/api/v1/modem/at", json!({"command": bad})).await;
        assert!(code == 400 || code == 403, "{bad:?} -> {code}");
    }
    let (code, v) = post(&base, "/api/v1/ussd", json!({"command": "balance"})).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["data"]["response"], "Balance: 42.00 credits");
    let (code, _) = post(&base, "/api/v1/ussd", json!({"code": "*999#"})).await;
    assert_eq!(code, 403, "arbitrary codes need ussd.allow_arbitrary_codes");
    let (code, _) = post(&base, "/api/v1/ussd", json!({"command": "nope"})).await;
    assert_eq!(code, 404);
    let (_, hist) = get(&base, "/api/v1/ussd").await;
    assert_eq!(hist["data"][0]["status"], "completed");
    h.stop().await;
}

#[tokio::test]
async fn bearer_tokens_admin_restrictions_and_csrf() {
    let h = start(Setup {
        main: "[api]\nport = 18081\nauthentication = \"bearer\"\ntokens = [\"user-token-0123456789\"]\nadmin_tokens = [\"admin-token-0123456789\"]\nallowed_hosts = [\"gateway.test\", \"127.0.0.1\"]\n".into(),
        ..Default::default()
    })
    .await;
    h.ready().await;
    let base = serve(&h).await;
    let c = reqwest::Client::new();
    let status =
        |r: reqwest::RequestBuilder| async move { r.send().await.unwrap().status().as_u16() };
    assert_eq!(
        status(c.get(format!("{base}/health"))).await,
        200,
        "probes need no token"
    );
    assert_eq!(
        status(c.get(format!("{base}/"))).await,
        200,
        "the dashboard shell is public; it asks for the token"
    );
    assert_eq!(status(c.get(format!("{base}/api/v1/status"))).await, 401);
    assert_eq!(status(c.get(format!("{base}/metrics"))).await, 401);
    assert_eq!(
        status(
            c.get(format!("{base}/api/v1/status"))
                .bearer_auth("wrong-token-0123456789")
        )
        .await,
        401
    );
    assert_eq!(
        status(
            c.get(format!("{base}/api/v1/status"))
                .bearer_auth("user-token-0123456789")
        )
        .await,
        200
    );
    // administrative endpoints need the admin token
    assert_eq!(
        status(
            c.post(format!("{base}/api/v1/modem/reconnect"))
                .bearer_auth("user-token-0123456789")
        )
        .await,
        401
    );
    assert_eq!(
        status(
            c.get(format!("{base}/api/v1/config"))
                .bearer_auth("user-token-0123456789")
        )
        .await,
        401
    );
    let r = c
        .get(format!("{base}/api/v1/config"))
        .bearer_auth("admin-token-0123456789")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body = r.text().await.unwrap();
    assert!(
        !body.contains("user-token-0123456789") && !body.contains("admin-token-0123456789"),
        "tokens are redacted"
    );
    // Host header validation
    assert_eq!(
        status(
            c.get(format!("{base}/api/v1/status"))
                .bearer_auth("user-token-0123456789")
                .header("Host", "evil.example")
        )
        .await,
        421
    );
    assert_eq!(
        status(
            c.get(format!("{base}/api/v1/status"))
                .bearer_auth("user-token-0123456789")
                .header("Host", "gateway.test:8080")
        )
        .await,
        200
    );
    // cross-origin state-changing requests are rejected
    let cross = c
        .post(format!("{base}/api/v1/sms/send"))
        .bearer_auth("user-token-0123456789")
        .header("Origin", "http://evil.example")
        .json(&json!({"to": "+4500000000", "message": "x"}));
    assert_eq!(status(cross).await, 403);
    let fetch = c
        .post(format!("{base}/api/v1/sms/send"))
        .bearer_auth("user-token-0123456789")
        .header("Sec-Fetch-Site", "cross-site")
        .json(&json!({"to": "+4500000000", "message": "x"}));
    assert_eq!(status(fetch).await, 403);
    let host = base.trim_start_matches("http://");
    let same = c
        .post(format!("{base}/api/v1/sms/send"))
        .bearer_auth("user-token-0123456789")
        .header("Origin", format!("http://{host}"))
        .json(&json!({"to": "+4500000000", "message": "x"}));
    assert_eq!(status(same).await, 202);
    h.stop().await;
}

#[tokio::test]
async fn cidr_restrictions_proxies_rate_and_size_limits() {
    let h = start(Setup {
        main: "[api]\nport = 18082\nallowed_cidrs = [\"127.0.0.1/32\", \"10.1.0.0/16\"]\ntrusted_proxies = [\"127.0.0.1\"]\nadmin_allowed_cidrs = [\"10.1.2.3/32\"]\nrate_limit_per_minute = 12\nmax_request_size_kb = 1\n".into(),
        ..Default::default()
    })
    .await;
    let base = serve(&h).await;
    let c = reqwest::Client::new();
    let status =
        |r: reqwest::RequestBuilder| async move { r.send().await.unwrap().status().as_u16() };
    // The peer (127.0.0.1) is a trusted proxy, so X-Forwarded-For decides.
    assert_eq!(
        status(
            c.get(format!("{base}/api/v1/version"))
                .header("X-Forwarded-For", "203.0.113.9")
        )
        .await,
        403
    );
    assert_eq!(
        status(
            c.get(format!("{base}/api/v1/version"))
                .header("X-Forwarded-For", "10.1.9.9")
        )
        .await,
        200
    );
    // a spoofed left-most entry does not help: the right-most untrusted hop counts
    assert_eq!(
        status(
            c.get(format!("{base}/api/v1/version"))
                .header("X-Forwarded-For", "10.1.9.9, 203.0.113.9")
        )
        .await,
        403
    );
    // administrative endpoints have their own source restriction
    assert_eq!(
        status(
            c.post(format!("{base}/api/v1/config/reload"))
                .header("X-Forwarded-For", "10.1.9.9")
        )
        .await,
        403
    );
    assert_eq!(
        status(
            c.post(format!("{base}/api/v1/config/reload"))
                .header("X-Forwarded-For", "10.1.2.3")
        )
        .await,
        200
    );
    // oversized bodies are refused
    let big = json!({"to": "+4500000000", "message": "x".repeat(5000)});
    assert_eq!(
        status(
            c.post(format!("{base}/api/v1/sms/send"))
                .header("X-Forwarded-For", "10.1.7.7")
                .json(&big)
        )
        .await,
        413
    );
    // per-client rate limit
    let mut limited = false;
    for _ in 0..20 {
        if status(
            c.get(format!("{base}/api/v1/version"))
                .header("X-Forwarded-For", "10.1.8.8"),
        )
        .await
            == 429
        {
            limited = true;
            break;
        }
    }
    assert!(limited);
    assert_eq!(
        status(
            c.get(format!("{base}/health"))
                .header("X-Forwarded-For", "10.1.8.8")
        )
        .await,
        200,
        "probes are not rate limited"
    );
    assert_eq!(
        status(
            c.get(format!("{base}/api/v1/version"))
                .header("X-Forwarded-For", "10.1.8.9")
        )
        .await,
        200,
        "other clients are unaffected"
    );
    h.stop().await;
}

#[tokio::test]
async fn untrusted_forwarded_header_is_ignored() {
    let h = start(Setup {
        main: "[api]\nport = 18083\nallowed_cidrs = [\"10.0.0.0/8\"]\n".into(),
        ..Default::default()
    })
    .await;
    let base = serve(&h).await;
    let r = reqwest::Client::new()
        .get(format!("{base}/api/v1/version"))
        .header("X-Forwarded-For", "10.1.1.1")
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        403,
        "X-Forwarded-For from an untrusted peer must not grant access"
    );
    assert_eq!(
        reqwest::get(format!("{base}/health"))
            .await
            .unwrap()
            .status(),
        403,
        "CIDR policy also covers probes"
    );
    h.stop().await;
}

#[tokio::test]
async fn live_event_stream_and_integration_test_endpoint() {
    let (hook, mock) = mock_server().await;
    let h = start(Setup {
        integrations: format!("[[integration]]\nname = \"hook\"\ntype = \"webhook\"\nenabled = false\nevents = [\"sms.received\"]\nurl = \"{hook}/in\"\n"),
        ..Default::default()
    })
    .await;
    h.ready().await;
    let base = serve(&h).await;
    let mut stream = reqwest::get(format!("{base}/api/v1/events/stream?type=sms."))
        .await
        .unwrap();
    assert!(stream.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    h.sim()
        .incoming_sms(&encode_deliver(&DeliverSpec::text("+4500000001", "live", ts())).unwrap()[0]);
    let mut seen = String::new();
    let got = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(chunk) = stream.chunk().await.unwrap() {
            seen.push_str(&String::from_utf8_lossy(&chunk));
            if seen.contains("event: sms.received") {
                return true;
            }
        }
        false
    })
    .await;
    assert_eq!(got, Ok(true), "{seen}");
    assert!(seen.contains("\"body\":\"live\""));
    assert!(!seen.contains("modem.connected"), "type filter applies");

    // A disabled integration receives nothing automatically...
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(mock.count(), 0);
    // ...but can be tested explicitly with a synthetic event.
    let (code, v) = post(&base, "/api/v1/integrations/hook/test", Value::Null).await;
    assert_eq!(
        (code, v["data"]["outcome"]["success"].as_bool()),
        (200, Some(true))
    );
    let reqs = mock.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].headers["x-thugsred-event"], "integration.test");
    let sent: Value = serde_json::from_str(&reqs[0].body).unwrap();
    assert_eq!(sent["data"]["test"], true);
    let (code, _) = post(&base, "/api/v1/integrations/missing/test", Value::Null).await;
    assert_eq!(code, 404);
    let (_, list) = get(&base, "/api/v1/integrations").await;
    assert_eq!(list["data"]["integrations"][0]["enabled"], false);
    assert!(
        !list.to_string().contains("/in"),
        "integration URLs are not exposed"
    );
    let m = h.gw.modems[0].clone();
    wait("still ready", 2, || {
        let m = m.clone();
        async move { m.is_ready() }
    })
    .await;
    h.stop().await;
}

#[tokio::test]
async fn contacts_and_page_paths() {
    let h = start(Setup::default()).await;
    h.ready().await;
    let base = serve(&h).await;
    let http = reqwest::Client::new();

    // Every dashboard page has a real path that serves the application shell.
    for page in ["", "send", "contacts", "inbox", "history", "docs"] {
        let r = reqwest::get(format!("{base}/{page}")).await.unwrap();
        assert_eq!(r.status().as_u16(), 200, "/{page}");
        assert!(r.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html"));
    }
    let r = reqwest::get(format!("{base}/send?to=%2B4500000001"))
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(
        reqwest::get(format!("{base}/nope"))
            .await
            .unwrap()
            .status()
            .as_u16(),
        404
    );

    // Create, list, use, change, delete.
    let (code, v) = post(
        &base,
        "/api/v1/contacts",
        json!({"name": " Alice ", "number": "+45 00 00 00 07", "note": "on call"}),
    )
    .await;
    assert_eq!(code, 201, "{v}");
    let id = v["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        (v["data"]["name"].as_str(), v["data"]["number"].as_str()),
        (Some("Alice"), Some("+4500000007"))
    );
    assert_eq!(
        (
            v["data"]["source"].as_str(),
            v["data"]["editable"].as_bool()
        ),
        (Some("saved"), Some(true))
    );
    let (code, _) = post(
        &base,
        "/api/v1/contacts",
        json!({"name": "Twin", "number": "+4500000007"}),
    )
    .await;
    assert_eq!(code, 409);
    for bad in [
        json!({"name": "", "number": "+4500000008"}),
        json!({"name": "X", "number": "abc"}),
        json!({"name": "X"}),
    ] {
        assert_eq!(post(&base, "/api/v1/contacts", bad).await.0, 400);
    }
    let (_, v) = get(&base, "/api/v1/contacts").await;
    assert_eq!(v["data"]["contacts"].as_array().unwrap().len(), 1);

    // The name is used for messages from and to that number at once.
    let (_, v) = post(
        &base,
        "/api/v1/sms/send",
        json!({"to": "+4500000007", "message": "hi"}),
    )
    .await;
    let mid = v["message_id"].as_str().unwrap().to_string();
    let (_, v) = get(&base, &format!("/api/v1/messages/{mid}")).await;
    assert_eq!(v["data"]["recipient_name"], "Alice");

    let (code, v) = post(
        &base,
        &format!("/api/v1/contacts/{id}"),
        json!({"name": "Alice B", "number": "+4500000007"}),
    )
    .await;
    assert_eq!(
        (code, v["data"]["name"].as_str(), v["data"]["note"].as_str()),
        (200, Some("Alice B"), Some(""))
    );
    let (_, v) = get(&base, &format!("/api/v1/messages/{mid}")).await;
    assert_eq!(v["data"]["recipient_name"], "Alice B");
    assert_eq!(
        post(
            &base,
            "/api/v1/contacts/unknown",
            json!({"name": "Z", "number": "+4500000009"})
        )
        .await
        .0,
        404
    );

    // Saved contacts survive a configuration reload.
    h.gw.reload().await.unwrap();
    let (_, v) = get(&base, "/api/v1/contacts").await;
    assert_eq!(v["data"]["contacts"][0]["name"], "Alice B");

    let r = http
        .delete(format!("{base}/api/v1/contacts/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let r = http
        .delete(format!("{base}/api/v1/contacts/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
    let (_, v) = get(&base, &format!("/api/v1/messages/{mid}")).await;
    assert!(v["data"]["recipient_name"].is_null());
    h.stop().await;
}

#[tokio::test]
async fn managed_api_tokens_lifecycle() {
    // Bearer auth on, with one configured admin token to bootstrap.
    let h = start(Setup {
        overrides: vec![
            "api.authentication=bearer".into(),
            "api.admin_tokens=[\"bootstrap-admin-token-0123456789\"]".into(),
        ],
        ..Default::default()
    })
    .await;
    h.ready().await;
    let base = serve(&h).await;
    let client = reqwest::Client::new();
    let authed = |tok: &str, path: String| {
        let c = client.clone();
        let url = format!("{base}{path}");
        let tok = tok.to_string();
        async move {
            let r = c
                .get(url)
                .header("authorization", format!("Bearer {tok}"))
                .send()
                .await
                .unwrap();
            r.status().as_u16()
        }
    };

    // No token: rejected. Managing tokens needs the admin token.
    assert_eq!(
        reqwest::get(format!("{base}/api/v1/status"))
            .await
            .unwrap()
            .status()
            .as_u16(),
        401
    );
    let r = client
        .post(format!("{base}/api/v1/tokens"))
        .header("authorization", "Bearer bootstrap-admin-token-0123456789")
        .json(&json!({"name": "honeypot", "scope": "read"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let created: Value = r.json().await.unwrap();
    let raw = created["data"]["token"].as_str().unwrap().to_string();
    let id = created["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(raw.len(), 64);

    // The raw token is never listed, only a prefix.
    let r = client
        .get(format!("{base}/api/v1/tokens"))
        .header("authorization", "Bearer bootstrap-admin-token-0123456789")
        .send()
        .await
        .unwrap();
    let list: Value = r.json().await.unwrap();
    let body = list.to_string();
    assert!(!body.contains(&raw));
    assert_eq!(list["data"]["tokens"][0]["scope"], "read");

    // The managed read token authenticates ordinary endpoints...
    assert_eq!(authed(&raw, "/api/v1/status".into()).await, 200);
    assert_eq!(authed(&raw, "/api/v1/security/events".into()).await, 200);
    // ...but cannot reach admin endpoints or manage tokens.
    assert_eq!(authed(&raw, "/api/v1/tokens".into()).await, 401);

    // Disable: effective immediately, no reload.
    let r = client
        .post(format!("{base}/api/v1/tokens/{id}"))
        .header("authorization", "Bearer bootstrap-admin-token-0123456789")
        .json(&json!({"enabled": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(authed(&raw, "/api/v1/status".into()).await, 401);

    // Revoke.
    let r = client
        .delete(format!("{base}/api/v1/tokens/{id}"))
        .header("authorization", "Bearer bootstrap-admin-token-0123456789")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let r = client
        .delete(format!("{base}/api/v1/tokens/{id}"))
        .header("authorization", "Bearer bootstrap-admin-token-0123456789")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
    h.stop().await;
}

#[tokio::test]
async fn local_networks_skip_the_token_but_external_callers_need_one() {
    let h = start(Setup {
        overrides: vec![
            "api.authentication=bearer".into(),
            "api.admin_tokens=[\"bootstrap-admin-token-0123456789\"]".into(),
            // Loopback is local (no token); a public test range is allowed but external.
            "api.local_networks=[\"127.0.0.0/8\",\"::1/128\"]".into(),
            "api.trusted_proxies=[\"127.0.0.1\"]".into(),
            "api.allowed_cidrs=[\"127.0.0.0/8\",\"::1/128\",\"203.0.113.0/24\"]".into(),
        ],
        ..Default::default()
    })
    .await;
    h.ready().await;
    let base = serve(&h).await;
    let client = reqwest::Client::new();

    // Local (loopback) needs no token and has admin rights.
    assert_eq!(
        reqwest::get(format!("{base}/api/v1/status"))
            .await
            .unwrap()
            .status()
            .as_u16(),
        200
    );
    let r = client
        .post(format!("{base}/api/v1/tokens"))
        .json(&json!({"name": "svc", "scope": "read"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        201,
        "local network manages tokens without a token"
    );

    // An external but allowed address (via the trusted proxy) still needs a token.
    let ext = |tok: Option<&str>| {
        let c = client.clone();
        let url = format!("{base}/api/v1/status");
        let tok = tok.map(str::to_string);
        async move {
            let mut rq = c.get(url).header("x-forwarded-for", "203.0.113.9");
            if let Some(t) = tok {
                rq = rq.header("authorization", format!("Bearer {t}"));
            }
            rq.send().await.unwrap().status().as_u16()
        }
    };
    assert_eq!(
        ext(None).await,
        401,
        "external caller without a token is rejected"
    );
    assert_eq!(
        ext(Some("bootstrap-admin-token-0123456789")).await,
        200,
        "external caller with a token works"
    );
    h.stop().await;
}

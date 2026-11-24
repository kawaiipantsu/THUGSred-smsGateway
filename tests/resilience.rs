//! Modem outages, SIM handling, restarts, multi-modem routing, calls,
//! integrations, automation and configuration reload.

mod common;

use common::{mock_server, start, ts, wait, Setup};
use serde_json::Value;
use thugsred_smsgateway::modem::simulator::SimHandle;
use thugsred_smsgateway::modem::ModemState;
use thugsred_smsgateway::sms::pdu::{encode_deliver, DeliverSpec};
use thugsred_smsgateway::sms::{SendRequest, SubmitError};

fn text_req(to: &str, body: &str) -> SendRequest {
    SendRequest {
        to: to.into(),
        message: Some(body.into()),
        origin: "test".into(),
        ..Default::default()
    }
}

async fn wait_state(h: &common::Harness, idx: usize, state: ModemState) {
    let m = h.gw.modems[idx].clone();
    wait(&format!("modem state {state:?}"), 20, || {
        let m = m.clone();
        async move { m.status().state == state }
    })
    .await;
}

#[tokio::test]
async fn survives_disconnect_and_sends_queued_messages_after_reconnect() {
    let h = start(Setup::default()).await;
    h.ready().await;
    // Device disappears (cable pulled).
    h.sim().with(|s| s.available = false);
    h.sim().disconnect();
    let m = h.gw.modems[0].clone();
    wait("device reported missing", 20, || {
        let m = m.clone();
        async move {
            m.status()
                .last_error
                .map(|e| e.contains("cannot open"))
                .unwrap_or(false)
        }
    })
    .await;
    let st = h.gw.modems[0].status();
    assert_eq!(st.state, ModemState::Disconnected);
    assert!(st.hint.unwrap().contains("does not exist"));
    // The daemon keeps working: history and queueing remain available.
    let id =
        h.gw.sms
            .submit(text_req("+4500000099", "queued during outage"))
            .await
            .unwrap()
            .message
            .id;
    assert_eq!(h.message_status(&id).await, "queued");
    assert!(h.gw.db.ping().await);
    // A message that arrives on the SIM while we are away is not lost.
    h.sim().store_silently(
        &encode_deliver(&DeliverSpec::text("+4500000042", "while offline", ts())).unwrap()[0],
    );
    // Device returns.
    h.sim().with(|s| s.available = true);
    h.ready().await;
    h.wait_status(&id, "delivered").await;
    assert_eq!(
        h.wait_inbound(1).await[0].body.as_deref(),
        Some("while offline")
    );
    assert_eq!(h.events("modem.disconnected").await.len(), 1);
    assert!(h.events("modem.connected").await.len() >= 2);
    assert!(h.gw.modems[0].status().hint.is_none());
    h.stop().await;
}

#[tokio::test]
async fn unpowered_module_is_reported_with_power_on_advice() {
    let sim = SimHandle::new();
    sim.with(|s| s.powered = false);
    let h = start(Setup {
        sims: vec![sim.clone()],
        ..Default::default()
    })
    .await;
    wait_state(&h, 0, ModemState::Disconnected).await;
    let st = h.gw.modems[0].status();
    assert!(st
        .last_error
        .as_deref()
        .unwrap()
        .contains("no response to AT"));
    let hint = st.hint.unwrap();
    assert!(
        hint.contains("PWRKEY") && hint.contains("powered on"),
        "{hint}"
    );
    let errors = h.events("modem.error").await;
    assert_eq!(
        errors.len(),
        1,
        "the same error is reported once, not on every retry"
    );
    assert!(errors[0].data["hint"].as_str().unwrap().contains("PWRKEY"));
    // The operator holds PWRKEY: the gateway connects on its own.
    sim.with(|s| s.powered = true);
    h.ready().await;
    // The module is switched off again while running.
    sim.raw_urc("NORMAL POWER DOWN");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    sim.with(|s| s.powered = false);
    wait_state(&h, 0, ModemState::Disconnected).await;
    assert!(h.gw.modems[0]
        .status()
        .last_error
        .unwrap()
        .contains("powered down"));
    sim.with(|s| s.powered = true);
    h.ready().await;
    h.stop().await;
}

#[tokio::test]
async fn sim_pin_is_entered_once_and_never_retried_when_wrong() {
    // Correct PIN: unlocks.
    let good = SimHandle::new();
    good.with(|s| {
        s.pin = Some("1234".into());
        s.pin_state = "SIM PIN".into();
    });
    let h = start(Setup {
        sim: "[sim]\npin = \"1234\"\npin_enabled = true\n".into(),
        sims: vec![good.clone()],
        ..Default::default()
    })
    .await;
    h.ready().await;
    assert_eq!(
        good.commands()
            .iter()
            .filter(|c| c.starts_with("AT+CPIN="))
            .count(),
        1
    );
    assert_eq!(h.events("sim.ready").await.len(), 1);
    h.stop().await;

    // Wrong PIN: exactly one attempt, then a safe locked state.
    let bad = SimHandle::new();
    bad.with(|s| {
        s.pin = Some("9999".into());
        s.pin_state = "SIM PIN".into();
    });
    let h = start(Setup {
        sim: "[sim]\npin = \"1234\"\npin_enabled = true\n".into(),
        sims: vec![bad.clone()],
        ..Default::default()
    })
    .await;
    wait_state(&h, 0, ModemState::SimLocked).await;
    // several health-check cycles and a forced reconnect later...
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    h.gw.modems[0].reconnect().await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert_eq!(
        bad.commands()
            .iter()
            .filter(|c| c.starts_with("AT+CPIN="))
            .count(),
        1,
        "a rejected PIN must not be tried again"
    );
    assert_eq!(bad.with(|s| s.pin_tries_left), 2);
    let st = h.gw.modems[0].status();
    assert_eq!(
        (st.state, st.sim.state.as_str()),
        (ModemState::SimLocked, "pin_required")
    );
    assert!(matches!(
        h.gw.modems[0].send_pdu(1, "00".into()).await,
        Err(thugsred_smsgateway::modem::SendError::NotReady(_))
    ));
    assert_eq!(h.events("sim.locked").await.len(), 1);
    // The PIN never appears in stored events.
    for e in h.gw.db.list_events(None, None, 500, 0).await.unwrap().0 {
        assert!(!e.data.to_string().contains("1234"));
    }
    h.stop().await;

    // PUK required: never attempted.
    let puk = SimHandle::new();
    puk.with(|s| s.pin_state = "SIM PUK".into());
    let h = start(Setup {
        sim: "[sim]\npin = \"1234\"\npin_enabled = true\n".into(),
        sims: vec![puk.clone()],
        ..Default::default()
    })
    .await;
    wait_state(&h, 0, ModemState::SimLocked).await;
    assert_eq!(h.gw.modems[0].status().sim.state, "puk_required");
    assert!(!puk.commands().iter().any(|c| c.starts_with("AT+CPIN=")));
    h.stop().await;
}

#[tokio::test]
async fn queue_survives_restart_and_uncertain_parts_are_not_resent() {
    let sim = SimHandle::new();
    sim.with(|s| s.available = false);
    let h = start(Setup {
        sims: vec![sim.clone()],
        ..Default::default()
    })
    .await;
    let queued =
        h.gw.sms
            .submit(text_req("+4500000099", "persisted across restart"))
            .await
            .unwrap()
            .message
            .id;
    let mid_flight =
        h.gw.sms
            .submit(text_req("+4500000098", "was being sent"))
            .await
            .unwrap()
            .message
            .id;
    // Simulate a crash while the second message was on the wire.
    let db = h.gw.db.clone();
    let claimed = db.claim_next("modem-1", true).await.unwrap().unwrap();
    let other = db.claim_next("modem-1", true).await.unwrap().unwrap();
    for (item, sending) in [
        (&claimed, claimed.message_id == mid_flight),
        (&other, other.message_id == mid_flight),
    ] {
        if sending {
            let part = db
                .parts_for_message(&item.message_id)
                .await
                .unwrap()
                .remove(0);
            db.update_part(&part.id, "sending", None, None)
                .await
                .unwrap();
        }
    }
    let (dir, sims) = h.stop().await;

    sim.with(|s| s.available = true);
    let h = start(Setup {
        dir: Some(dir),
        sims,
        ..Default::default()
    })
    .await;
    h.ready().await;
    h.wait_status(&queued, "delivered").await;
    assert_eq!(h.message_status(&mid_flight).await, "submission_unknown");
    let sent = sim.sent();
    assert_eq!(
        sent.len(),
        1,
        "only the message that certainly was not transmitted is sent"
    );
    // An operator may explicitly retry the uncertain one.
    assert!(h.gw.db.retry_message(&mid_flight, None).await.unwrap());
    h.gw.sms.wake.notify_waiters();
    h.wait_status(&mid_flight, "delivered").await;
    h.stop().await;
}

#[tokio::test]
async fn multiple_modems_route_by_sim_and_enforce_quotas() {
    let h = start(Setup {
        modems: 3,
        sim: "[[sims]]\nid = \"sim-1\"\nmobile_number = \"+4500000101\"\n[[sims]]\nid = \"sim-2\"\nmobile_number = \"+4500000102\"\n[sims.limits]\ndaily_sms_limit = 1\n[[sims]]\nid = \"sim-3\"\nmobile_number = \"+4500000103\"\n".into(),
        ..Default::default()
    })
    .await;
    h.ready().await;
    assert_eq!(h.gw.modems.len(), 3);
    // `from` selects the SIM; it cannot name a foreign number.
    let mut r = text_req("+4500000099", "via second SIM");
    r.from = Some("+4500000102".into());
    let a = h.gw.sms.submit(r).await.unwrap().message;
    assert_eq!(
        (a.modem_id.as_deref(), a.sender.as_deref()),
        (Some("modem-2"), Some("+4500000102"))
    );
    let mut r = text_req("+4500000099", "via third modem");
    r.modem = Some("modem-3".into());
    let b = h.gw.sms.submit(r).await.unwrap().message.id;
    let mut r = text_req("+4500000099", "spoof?");
    r.from = Some("+4599999999".into());
    assert!(matches!(
        h.gw.sms.submit(r).await,
        Err(SubmitError::Invalid(_))
    ));
    h.wait_status(&a.id, "delivered").await;
    h.wait_status(&b, "delivered").await;
    assert_eq!(
        (
            h.sims[0].sent().len(),
            h.sims[1].sent().len(),
            h.sims[2].sent().len()
        ),
        (0, 1, 1)
    );
    // The second SIM's daily limit is now used up.
    let mut r = text_req("+4500000099", "over quota");
    r.from = Some("+4500000102".into());
    assert!(matches!(
        h.gw.sms.submit(r).await,
        Err(SubmitError::LimitReached(_))
    ));
    // Unpinned messages are taken by a ready modem; the exhausted SIM defers.
    let c =
        h.gw.sms
            .submit(text_req("+4500000097", "any modem"))
            .await
            .unwrap()
            .message
            .id;
    h.wait_status(&c, "delivered").await;
    assert_eq!(
        h.sims[1].sent().len(),
        1,
        "the SIM at its quota sent nothing more"
    );
    // Inbound messages record which modem and SIM received them.
    h.sims[2].incoming_sms(
        &encode_deliver(&DeliverSpec::text("+4500000042", "to the third", ts())).unwrap()[0],
    );
    let m = h.wait_inbound(1).await.remove(0);
    assert_eq!(
        (
            m.modem_id.as_deref(),
            m.sim_id.as_deref(),
            m.recipient.as_deref()
        ),
        (Some("modem-3"), Some("sim-3"), Some("+4500000103"))
    );
    // One modem failing does not affect the others.
    h.sims[0].with(|s| s.available = false);
    h.sims[0].disconnect();
    wait_state(&h, 0, ModemState::Disconnected).await;
    let d =
        h.gw.sms
            .submit(text_req("+4500000096", "still works"))
            .await
            .unwrap()
            .message
            .id;
    h.wait_status(&d, "delivered").await;
    h.stop().await;
}

#[tokio::test]
async fn integrations_fan_out_filter_anonymise_retry_and_dead_letter() {
    let (hook, mock) = mock_server().await;
    let integrations = format!(
        r#"
[[integration]]
name = "all-events"
type = "webhook"
enabled = true
events = ["sms.received"]
url = "{hook}/all"
sign_payload = true
signing_secret = "topsecret"

[[integration]]
name = "discord-alerts"
type = "discord"
enabled = true
events = ["sms.received"]
webhook_url = "{hook}/discord-alerts"
[integration.filter]
sender_allowlist = ["+4500000001"]

[[integration]]
name = "discord-private"
type = "discord"
enabled = true
events = ["sms.received"]
webhook_url = "{hook}/discord-private"
anonymize = true

[[integration]]
name = "legacy-get"
type = "webhook"
enabled = true
events = ["sms.received"]
method = "GET"
url = "{hook}/get"
[integration.query]
sender = "{{{{message.sender}}}}"
message = "{{{{message.body}}}}"

[[integration]]
name = "off"
type = "slack"
enabled = false
events = ["sms.received"]
webhook_url = "{hook}/off"
"#
    );
    let h = start(Setup {
        integrations,
        overrides: vec!["integrations.max_attempts=3".into()],
        ..Default::default()
    })
    .await;
    let dir = h.dir.path().to_path_buf();
    std::fs::write(
        dir.join("addressbook.conf"),
        "[contacts]\n\"+4500000001\" = \"Alice Example\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("anonymous.conf"), "numbers = [\"+4500000007\"]\n").unwrap();
    h.gw.reload().await.unwrap();
    h.ready().await;

    h.sim().incoming_sms(
        &encode_deliver(&DeliverSpec::text("+4500000001", "ping & <b>", ts())).unwrap()[0],
    );
    let m = mock.clone();
    wait("four deliveries", 15, || {
        let m = m.clone();
        async move { m.count() >= 4 }
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let reqs = mock.requests.lock().unwrap().clone();
    assert_eq!(
        reqs.len(),
        4,
        "one event fans out to every matching enabled integration: {:?}",
        reqs.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
    let by = |p: &str| {
        reqs.iter()
            .find(|r| r.path == p)
            .unwrap_or_else(|| panic!("no request to {p}"))
    };
    let all: Value = serde_json::from_str(&by("/all").body).unwrap();
    assert_eq!(all["type"], "sms.received");
    assert_eq!(all["data"]["message"]["sender_name"], "Alice Example");
    let sig = &by("/all").headers["x-thugsred-signature"];
    assert_eq!(
        *sig,
        thugsred_smsgateway::integrations::sign(
            "topsecret",
            &by("/all").headers["x-thugsred-timestamp"],
            &by("/all").body
        )
    );
    let alerts: Value = serde_json::from_str(&by("/discord-alerts").body).unwrap();
    assert!(alerts["content"]
        .as_str()
        .unwrap()
        .starts_with("SMS from Alice Example"));
    let private = &by("/discord-private").body;
    assert!(
        !private.contains("4500000001")
            && !private.contains("Alice")
            && private.contains("Anonymous")
    );
    let get = by("/get");
    assert_eq!(get.method, "GET");
    assert!(
        get.query.contains("sender=%2B4500000001")
            && get.query.contains("message=ping+%26+%3Cb%3E"),
        "{}",
        get.query
    );
    assert!(
        !reqs.iter().any(|r| r.path == "/off"),
        "disabled integrations receive nothing"
    );

    // A sender on the global anonymous list is hidden everywhere, and the
    // sender filter of "discord-alerts" does not match it.
    mock.requests.lock().unwrap().clear();
    h.sim().incoming_sms(
        &encode_deliver(&DeliverSpec::text("+4500000007", "sensitive sender", ts())).unwrap()[0],
    );
    let m = mock.clone();
    wait("three deliveries", 15, || {
        let m = m.clone();
        async move { m.count() >= 3 }
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let reqs = mock.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 3);
    for r in &reqs {
        assert!(
            !r.body.contains("4500000007") && !r.query.contains("4500000007"),
            "{} leaked the anonymous number",
            r.path
        );
    }
    // ...while the gateway's own history keeps the real number.
    assert!(h
        .inbound()
        .await
        .iter()
        .any(|m| m.sender.as_deref() == Some("+4500000007")));

    // Failures: retried with backoff, then dead-lettered; replay works.
    mock.requests.lock().unwrap().clear();
    mock.set_status(503);
    h.sim().incoming_sms(
        &encode_deliver(&DeliverSpec::text(
            "+4500000002",
            "will not get through",
            ts(),
        ))
        .unwrap()[0],
    );
    let db = h.gw.db.clone();
    wait("dead letters", 30, || {
        let db = db.clone();
        async move {
            db.list_deliveries(None, Some("dead"), 50, 0)
                .await
                .unwrap()
                .1
                == 3
        }
    })
    .await;
    assert_eq!(mock.count(), 9, "3 integrations x 3 attempts");
    assert_eq!(h.gw.db.list_dead_letters(50, 0).await.unwrap().1, 3);
    let stats = h.gw.db.integration_stats("all-events").await.unwrap();
    assert_eq!(
        (stats["delivered"].as_i64(), stats["dead"].as_i64()),
        (Some(2), Some(1))
    );
    assert_eq!(stats["last_attempt"]["status_code"], 503);
    mock.set_status(200);
    let dead =
        h.gw.db
            .list_deliveries(Some("all-events"), Some("dead"), 1, 0)
            .await
            .unwrap()
            .0
            .remove(0);
    assert!(h
        .gw
        .db
        .replay_delivery(dead["id"].as_str().unwrap())
        .await
        .unwrap());
    h.gw.events.wake_handle().notify_waiters();
    wait("replayed", 15, || {
        let db = db.clone();
        async move { db.integration_stats("all-events").await.unwrap()["delivered"] == 3 }
    })
    .await;
    // A permanent client error is not retried.
    mock.requests.lock().unwrap().clear();
    mock.set_status(404);
    h.sim().incoming_sms(
        &encode_deliver(&DeliverSpec::text("+4500000003", "bad endpoint", ts())).unwrap()[0],
    );
    wait("dead without retries", 15, || {
        let db = db.clone();
        async move {
            db.list_deliveries(None, Some("dead"), 50, 0)
                .await
                .unwrap()
                .1
                == 5
        }
    })
    .await;
    assert_eq!(mock.count(), 3);
    h.stop().await;
}

#[tokio::test]
async fn routing_rules_and_automation_with_loop_protection() {
    let (hook, mock) = mock_server().await;
    let integrations = format!(
        r#"
[[integration]]
name = "ops"
type = "webhook"
enabled = true
events = ["sms.received"]
url = "{hook}/ops"

[[integration]]
name = "archive"
type = "webhook"
enabled = true
events = ["sms.received"]
url = "{hook}/archive"

[[routing.rule]]
name = "private"
events = ["sms.received"]
sender_allowlist = ["+4500000009"]
destinations = []
stop_processing = true

[[routing.rule]]
name = "alerts"
events = ["sms.received"]
message_regex = "(?i)alert"
destinations = ["ops"]

[[automation.rule]]
name = "status-reply"
enabled = true
action = "reply"
sender_allowlist = ["+4500000001", "ShortSvc", "+4500000100"]
message_contains = ["status"]
template = "Gateway {{{{gateway.instance_id}}}} is up. You wrote: {{{{message.body}}}}"
cooldown_seconds = 3600

[[automation.rule]]
name = "forward-alerts"
enabled = true
action = "forward"
message_regex = "(?i)alert"
to = ["+4500000055"]
template = "FWD {{{{message.sender}}}}: {{{{message.body}}}}"
cooldown_seconds = 0
max_per_hour = 2
"#
    );
    let h = start(Setup {
        integrations,
        sim: "[sim]\nmobile_number = \"+4500000100\"\n".into(),
        ..Default::default()
    })
    .await;
    h.ready().await;
    let deliver = |from: &str, text: &str| {
        encode_deliver(&DeliverSpec::text(from, text, ts()))
            .unwrap()
            .remove(0)
    };

    h.sim()
        .incoming_sms(&deliver("+4500000009", "private alert"));
    h.sim()
        .incoming_sms(&deliver("+4500000002", "ALERT: disk full"));
    h.sim()
        .incoming_sms(&deliver("+4500000002", "just chatting"));
    h.wait_inbound(3).await;
    let m = mock.clone();
    wait("routed deliveries", 15, || {
        let m = m.clone();
        async move { m.count() >= 3 }
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let paths: Vec<String> = mock
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.path.clone())
        .collect();
    assert_eq!(
        paths.iter().filter(|p| *p == "/ops").count(),
        2,
        "alert rule + default routing: {paths:?}"
    );
    assert_eq!(
        paths.iter().filter(|p| *p == "/archive").count(),
        1,
        "only the unmatched message reaches archive"
    );
    assert!(
        !mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.body.contains("private alert")),
        "the private rule stops forwarding"
    );

    // Automatic reply: once per sender within the cooldown.
    h.sim()
        .incoming_sms(&deliver("+4500000001", "status please"));
    h.sim()
        .incoming_sms(&deliver("+4500000001", "status again"));
    // Alphanumeric senders and the gateway's own number never get replies.
    h.sim().incoming_sms(&deliver("ShortSvc", "status"));
    h.sim().incoming_sms(&deliver("+4500000100", "status loop"));
    // Forwarding is capped per hour.
    for i in 0..3 {
        h.sim()
            .incoming_sms(&deliver("+4500000003", &format!("alert number {i}")));
    }
    h.wait_inbound(10).await;
    let sim = h.sim().clone();
    wait("automation messages sent", 20, || {
        let sim = sim.clone();
        async move { sim.sent().len() >= 3 }
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let q = thugsred_smsgateway::database::MessageQuery {
        direction: Some("outbound".into()),
        limit: 50,
        ..Default::default()
    };
    let out = h.gw.db.search_messages(&q).await.unwrap().0;
    let replies: Vec<_> = out
        .iter()
        .filter(|m| m.recipient.as_deref() == Some("+4500000001"))
        .collect();
    assert_eq!(replies.len(), 1, "cooldown allows one automatic reply");
    assert_eq!(
        replies[0].body.as_deref(),
        Some("Gateway test-gw is up. You wrote: status please")
    );
    assert!(!out
        .iter()
        .any(|m| m.recipient.as_deref() == Some("ShortSvc")
            || m.recipient.as_deref() == Some("+4500000100")));
    let forwards: Vec<_> = out
        .iter()
        .filter(|m| m.recipient.as_deref() == Some("+4500000055"))
        .collect();
    // Five messages matched the forward rule; the hourly cap of 2 holds.
    assert_eq!(
        forwards.len(),
        2,
        "{:?}",
        forwards.iter().map(|m| &m.body).collect::<Vec<_>>()
    );
    assert!(forwards
        .iter()
        .all(|m| m.body.as_deref().unwrap().starts_with("FWD +45")));
    assert_eq!(out.len(), 3);
    h.stop().await;
}

#[tokio::test]
async fn reload_applies_valid_changes_and_keeps_last_good_configuration() {
    let h = start(Setup::default()).await;
    h.ready().await;
    let path = h.dir.path().join("smsgateway.conf");
    let original = std::fs::read_to_string(&path).unwrap();
    assert!(h.gw.cfg.load().sms.send_enabled);

    std::fs::write(&path, format!("{original}\n[sms]\nsend_enabled = false\noutbound_rate_limit_per_minute = 77\n[api]\nport = 19999\n")).unwrap();
    let restart = h.gw.reload().await.unwrap();
    assert_eq!(restart, vec!["api.enabled/bind/port".to_string()]);
    assert_eq!(h.gw.cfg.load().sms.outbound_rate_limit_per_minute, 77);
    assert!(
        matches!(
            h.gw.sms.submit(text_req("+4500000099", "x")).await,
            Err(SubmitError::Unavailable(_))
        ),
        "reloaded setting is effective immediately"
    );

    // An invalid file is rejected as a whole; the running config is untouched.
    std::fs::write(
        &path,
        format!("{original}\n[sms]\nsend_enabled = true\nmax_message_parts = 0\n"),
    )
    .unwrap();
    let err = h.gw.reload().await.unwrap_err();
    assert!(err.to_string().contains("sms.max_message_parts"));
    assert_eq!(h.gw.cfg.load().sms.outbound_rate_limit_per_minute, 77);
    assert!(!h.gw.cfg.load().sms.send_enabled);
    let st = h.gw.config_status();
    assert!(st.last_reload_error.unwrap().contains("max_message_parts"));
    std::fs::write(&path, "this is [not toml").unwrap();
    assert!(h.gw.reload().await.is_err());
    assert_eq!(h.gw.cfg.load().sms.outbound_rate_limit_per_minute, 77);

    // A modem setting change makes the modem reconnect with the new settings.
    std::fs::write(&path, format!("{original}\n[sms]\nsend_enabled = true\n")).unwrap();
    h.gw.reload().await.unwrap();
    assert!(h.gw.config_status().last_reload_error.is_none());
    let before = h.events("modem.connected").await.len();
    std::fs::write(
        &path,
        original.replace("command_timeout_seconds = 2", "command_timeout_seconds = 3"),
    )
    .unwrap();
    h.gw.reload().await.unwrap();
    let db = h.gw.db.clone();
    wait("modem reconnected with new settings", 20, || {
        let db = db.clone();
        async move {
            db.list_events(Some("modem.connected"), None, 50, 0)
                .await
                .unwrap()
                .1 as usize
                > before
        }
    })
    .await;
    h.ready().await;
    assert_eq!(h.events("gateway.config_reloaded").await.len(), 3);
    // A SIM setting change is applied by reconnecting the modem as well.
    let before = h.events("modem.connected").await.len();
    std::fs::write(
        h.dir.path().join("sim.conf"),
        "[sim]\nsmsc = \"+4500000001\"\n",
    )
    .unwrap();
    h.gw.reload().await.unwrap();
    let db = h.gw.db.clone();
    wait("modem reconnected after SIM change", 20, || {
        let db = db.clone();
        async move {
            db.list_events(Some("modem.connected"), None, 50, 0)
                .await
                .unwrap()
                .1 as usize
                > before
        }
    })
    .await;
    h.ready().await;
    assert!(h
        .sim()
        .commands()
        .iter()
        .any(|c| c.starts_with("AT+CSCA=\"+4500000001\"")));
    h.stop().await;
}

#[tokio::test]
async fn calls_are_logged_and_recording_is_reported_honestly() {
    let h = start(Setup {
        main: "[calls]\nring_timeout_seconds = 2\n".into(),
        ..Default::default()
    })
    .await;
    h.ready().await;
    // Missed call that the caller hangs up.
    h.sim().ring("+4500000042");
    let db = h.gw.db.clone();
    wait("call logged", 10, || {
        let db = db.clone();
        async move { db.list_calls(10, 0).await.unwrap().1 == 1 }
    })
    .await;
    h.sim().end_call();
    wait("call missed", 10, || {
        let db = db.clone();
        async move { db.list_calls(10, 0).await.unwrap().0[0]["state"] == "missed" }
    })
    .await;
    let c = h.gw.db.list_calls(10, 0).await.unwrap().0.remove(0);
    assert_eq!(
        (
            c["number"].as_str(),
            c["missed"].as_i64(),
            c["answered"].as_i64(),
            c["termination_reason"].as_str()
        ),
        (Some("+4500000042"), Some(1), Some(0), Some("no_carrier"))
    );
    assert_eq!(c["recording_status"], "disabled");
    assert_eq!(h.events("call.incoming").await.len(), 1);
    assert_eq!(h.events("call.missed").await.len(), 1);
    assert!(
        !h.sim().commands().iter().any(|c| c == "ATA"),
        "calls are not answered unless enabled"
    );
    // A call without caller id that simply stops ringing.
    h.sim().raw_urc("RING");
    wait("second call missed by timeout", 15, || {
        let db = db.clone();
        async move {
            db.list_calls(10, 0)
                .await
                .unwrap()
                .0
                .iter()
                .filter(|c| c["state"] == "missed")
                .count()
                == 2
        }
    })
    .await;
    h.stop().await;

    // Auto-answer with recording enabled but no audio device: the call is
    // answered and logged, and the recording is reported as unavailable.
    let h = start(Setup { main: "[calls]\nauto_answer = true\nrecording_enabled = true\nrecording_duration_seconds = 1\n".into(), ..Default::default() }).await;
    h.ready().await;
    h.sim().ring("+4500000043");
    let db = h.gw.db.clone();
    wait("call ended", 15, || {
        let db = db.clone();
        async move {
            db.list_calls(10, 0)
                .await
                .unwrap()
                .0
                .first()
                .map(|c| c["state"] == "ended")
                .unwrap_or(false)
        }
    })
    .await;
    let c = h.gw.db.list_calls(10, 0).await.unwrap().0.remove(0);
    assert_eq!(
        (
            c["answered"].as_i64(),
            c["recording_status"].as_str(),
            c["termination_reason"].as_str()
        ),
        (Some(1), Some("unavailable"), Some("gateway_hangup"))
    );
    assert!(c["recording_id"].is_null());
    assert_eq!(
        h.gw.db.list_recordings(10, 0).await.unwrap().1,
        0,
        "no recording is fabricated"
    );
    assert!(h.events("call.recorded").await.is_empty());
    let cmds = h.sim().commands();
    assert!(cmds.iter().any(|c| c == "ATA") && cmds.iter().any(|c| c == "ATH"));
    assert!(
        std::fs::read_dir(h.dir.path().join("recordings"))
            .map(|d| d.count())
            .unwrap_or(0)
            == 0
    );
    h.stop().await;
}

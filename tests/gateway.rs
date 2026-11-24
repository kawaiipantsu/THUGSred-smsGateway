//! End-to-end tests of the gateway core against the simulated modem.

mod common;

use common::{start, ts, wait, Setup};
use thugsred_smsgateway::modem::simulator::SendMode;
use thugsred_smsgateway::modem::ModemState;
use thugsred_smsgateway::sms::pdu::{
    encode_deliver, encode_status_report, DeliverSpec, Encoding, Payload, Pdu, PduDirection,
};
use thugsred_smsgateway::sms::{SendRequest, SubmitError};

fn text_req(to: &str, body: &str) -> SendRequest {
    SendRequest {
        to: to.into(),
        message: Some(body.into()),
        origin: "test".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn receives_stores_and_deletes_from_modem() {
    let h = start(Setup::default()).await;
    h.ready().await;
    let st = h.gw.modems[0].status();
    assert_eq!(st.manufacturer.as_deref(), Some("SIMCOM_Ltd"));
    assert_eq!(st.profile.as_deref(), Some("simcom-2g"));
    assert!(st.capabilities.detected && st.capabilities.sms_modes.contains(&"pdu".to_string()));
    assert_eq!(st.capabilities.gnss, "supported");

    let pdu = encode_deliver(&DeliverSpec::text("+4500000042", "Hello gateway €", ts()))
        .unwrap()
        .remove(0);
    h.sim().incoming_sms(&pdu);
    let m = h.wait_inbound(1).await.remove(0);
    assert_eq!(m.sender.as_deref(), Some("+4500000042"));
    assert_eq!(m.body.as_deref(), Some("Hello gateway €"));
    assert_eq!(
        (
            m.message_type.as_str(),
            m.status.as_str(),
            m.encoding.as_deref()
        ),
        ("sms", "received", Some("gsm7"))
    );
    assert_eq!(
        m.network_timestamp.as_deref(),
        Some("2025-03-04T04:06:07.000Z")
    );
    assert_eq!(m.raw_pdu.as_deref(), Some(pdu.as_str()));
    assert_eq!(m.smsc.as_deref(), Some("+4500000001"));
    // persisted first, then removed from modem storage
    let sim = h.sim().clone();
    wait("deleted from modem", 5, || {
        let sim = sim.clone();
        async move { sim.stored() == 0 }
    })
    .await;
    assert_eq!(h.events("sms.received").await.len(), 1);
    assert_eq!(h.gw.db.parts_for_message(&m.id).await.unwrap().len(), 1);

    // The same PDU again (e.g. re-read after a failed delete) is not duplicated.
    h.sim().incoming_sms(&pdu);
    h.sim().incoming_direct(&pdu);
    wait("duplicate removed", 5, || {
        let sim = sim.clone();
        async move { sim.stored() == 0 }
    })
    .await;
    assert_eq!(h.inbound().await.len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn unicode_binary_flash_and_malformed_messages() {
    let h = start(Setup::default()).await;
    h.ready().await;
    let mut uni = DeliverSpec::text("ExampleSvc", "Привет 😀 你好", ts());
    uni.message_class = Some(0);
    let mut bin = DeliverSpec::text("+4500000042", "", ts());
    bin.payload = Payload::Binary(vec![1, 2, 3, 254, 255]);
    bin.encoding = Encoding::Binary;
    bin.destination_port = Some(16000);
    bin.source_port = Some(16001);
    for spec in [&uni, &bin] {
        h.sim().incoming_sms(&encode_deliver(spec).unwrap()[0]);
    }
    h.sim().incoming_direct("07911234");
    h.sim()
        .incoming_sms("0791447758100650040C9144779700208000004250120000000005E8329BFD");
    let all = h.wait_inbound(4).await;
    let by_type = |t: &str| {
        all.iter()
            .find(|m| m.message_type == t)
            .unwrap_or_else(|| panic!("no {t}: {all:?}"))
    };
    let flash = by_type("flash");
    assert_eq!(flash.sender.as_deref(), Some("ExampleSvc"));
    assert_eq!(flash.body.as_deref(), Some("Привет 😀 你好"));
    assert_eq!(
        (flash.encoding.as_deref(), flash.message_class),
        (Some("ucs2"), Some(0))
    );
    let b = by_type("binary");
    assert_eq!(b.payload.as_deref(), Some(&[1u8, 2, 3, 254, 255][..]));
    assert_eq!(
        (b.dst_port, b.src_port, b.body.clone()),
        (Some(16000), Some(16001), None)
    );
    let bad: Vec<_> = all.iter().filter(|m| m.message_type == "unknown").collect();
    assert_eq!(bad.len(), 2, "malformed PDUs are kept as raw records");
    assert!(bad
        .iter()
        .all(|m| m.status == "malformed" && m.raw_pdu.is_some() && m.error.is_some()));
    assert_eq!(h.events("sms.binary_received").await.len(), 1);
    assert!(
        h.gw.modems[0].is_ready(),
        "malformed input must not disturb the modem session"
    );
    h.stop().await;
}

#[tokio::test]
async fn multipart_out_of_order_duplicates_and_reference_reuse() {
    let h = start(Setup {
        main: "[sms]\nmultipart_timeout_seconds = 2\n".into(),
        ..Default::default()
    })
    .await;
    h.ready().await;
    let body = "Part one of a long story. ".repeat(14);
    let mut spec = DeliverSpec::text("+4500000042", &body, ts());
    spec.reference = 77;
    let pdus = encode_deliver(&spec).unwrap();
    assert_eq!(pdus.len(), 3);
    // out of order, with a duplicate segment
    for i in [2usize, 0, 2, 1] {
        h.sim().incoming_sms(&pdus[i]);
    }
    let m = h.wait_inbound(1).await.remove(0);
    assert_eq!(m.body.as_deref(), Some(body.as_str()));
    assert_eq!(
        (m.parts, m.concat_ref, m.status.as_str()),
        (3, Some(77), "received")
    );
    assert_eq!(h.gw.db.parts_for_message(&m.id).await.unwrap().len(), 3);
    assert_eq!(h.events("sms.multipart_completed").await.len(), 1);

    // Incomplete message: only 2 of 3 segments ever arrive.
    let mut lost = DeliverSpec::text("+4500000043", &"x".repeat(400), ts());
    lost.reference = 5;
    let lp = encode_deliver(&lost).unwrap();
    h.sim().incoming_sms(&lp[0]);
    h.sim().incoming_sms(&lp[2]);
    // Housekeeping does this periodically; trigger it directly once the
    // multipart timeout has passed.
    tokio::time::sleep(std::time::Duration::from_millis(2600)).await;
    assert_eq!(
        h.inbound().await.len(),
        1,
        "incomplete message is held back until the timeout"
    );
    assert_eq!(h.gw.sms.flush_stale_multipart().await.unwrap(), 1);
    let all = h.wait_inbound(2).await;
    let inc = all
        .iter()
        .find(|m| m.sender.as_deref() == Some("+4500000043"))
        .unwrap();
    assert_eq!(inc.status, "incomplete");
    assert_eq!(
        inc.metadata.as_ref().unwrap()["missing_parts"],
        serde_json::json!([2])
    );
    assert!(inc.body.as_ref().unwrap().contains("[...]"));

    // Reference reuse: a new message with the same reference while an old
    // group is still pending must not be mixed into it.
    let mut a = DeliverSpec::text("+4500000044", &"A".repeat(200), ts());
    a.reference = 9;
    let mut b = DeliverSpec::text(
        "+4500000044",
        &"B".repeat(200),
        chrono::DateTime::parse_from_rfc3339("2025-03-04T06:00:00+01:00").unwrap(),
    );
    b.reference = 9;
    let (pa, pb) = (encode_deliver(&a).unwrap(), encode_deliver(&b).unwrap());
    h.sim().incoming_sms(&pa[0]);
    h.sim().incoming_sms(&pb[0]);
    h.sim().incoming_sms(&pb[1]);
    let all = h.wait_inbound(4).await;
    assert_eq!(h.gw.sms.flush_stale_multipart().await.unwrap(), 0);
    let from44: Vec<_> = all
        .iter()
        .filter(|m| m.sender.as_deref() == Some("+4500000044"))
        .collect();
    assert_eq!(from44.len(), 2);
    assert!(from44
        .iter()
        .any(|m| m.status == "received" && m.body.as_deref() == Some("B".repeat(200).as_str())));
    assert!(from44.iter().any(|m| m.status == "incomplete"
        && m.body.as_ref().unwrap().starts_with("AAAA")
        && !m.body.as_ref().unwrap().contains('B')));
    h.stop().await;
}

#[tokio::test]
async fn sends_multipart_and_correlates_delivery_reports() {
    let h = start(Setup {
        sim: "[sim]\nmobile_number = \"+4500000100\"\n".into(),
        ..Default::default()
    })
    .await;
    h.ready().await;
    let body = "Outbound text with Unicode ✓ ".repeat(8);
    let mut req = text_req("+45 00 00 00 99", &body);
    req.idempotency_key = Some("idem-1".into());
    let first = h.gw.sms.submit(req.clone()).await.unwrap();
    assert!(!first.duplicate);
    assert_eq!(
        (
            first.message.status.as_str(),
            first.message.encoding.as_deref()
        ),
        ("queued", Some("ucs2"))
    );
    assert_eq!(first.message.recipient.as_deref(), Some("+4500000099"));
    let again = h.gw.sms.submit(req).await.unwrap();
    assert!(again.duplicate && again.message.id == first.message.id);

    let id = first.message.id.clone();
    h.wait_status(&id, "delivered").await;
    let sent = h.sim().sent();
    assert_eq!(
        sent.len() as i64,
        first.message.parts,
        "idempotent request is sent once"
    );
    let mut text = String::new();
    for p in &sent {
        let Pdu::Submit(s) =
            thugsred_smsgateway::sms::pdu::decode(p, PduDirection::MobileOriginated, true).unwrap()
        else {
            panic!()
        };
        assert_eq!(s.recipient.number, "+4500000099");
        assert!(s.status_report_request);
        text.push_str(s.user_data.text.as_ref().unwrap());
    }
    assert_eq!(text, body);
    let m = h.gw.db.get_message(&id).await.unwrap().unwrap();
    assert_eq!(
        m.sender.as_deref(),
        Some("+4500000100"),
        "sender is the SIM's configured gateway number"
    );
    assert!(m.submitted_at.is_some() && m.delivered_at.is_some());
    assert_eq!(m.delivery_status.as_deref(), Some("delivered"));
    let hist: Vec<String> =
        h.gw.db
            .history(&id)
            .await
            .unwrap()
            .iter()
            .map(|j| j["status"].as_str().unwrap().to_string())
            .collect();
    assert_eq!(hist.first().map(String::as_str), Some("queued"));
    assert!(
        hist.contains(&"submitted".to_string())
            && hist.last().map(String::as_str) == Some("delivered")
    );
    assert_eq!(
        h.gw.db.delivery_reports(&id).await.unwrap().len(),
        sent.len()
    );
    for e in ["sms.queued", "sms.sent", "sms.delivered"] {
        assert_eq!(h.events(e).await.len(), 1, "{e}");
    }
    h.stop().await;
}

#[tokio::test]
async fn failed_delivery_report_and_uncorrelated_report() {
    let h = start(Setup::default()).await;
    h.ready().await;
    h.sim().with(|s| s.auto_delivery_report = false);
    let id =
        h.gw.sms
            .submit(text_req("+4500000099", "report me"))
            .await
            .unwrap()
            .message
            .id;
    h.wait_status(&id, "submitted").await;
    let part = h.gw.db.parts_for_message(&id).await.unwrap().remove(0);
    let mr = part.modem_ref.unwrap() as u8;
    // a report for an unknown reference is stored but changes nothing
    h.sim().status_report(
        &encode_status_report(None, mr.wrapping_add(50), "+4500000099", ts(), ts(), 0x00).unwrap(),
    );
    // a report for the right reference but another recipient is not correlated
    h.sim()
        .status_report(&encode_status_report(None, mr, "+4599999999", ts(), ts(), 0x00).unwrap());
    // temporary "still trying" keeps the message submitted
    h.sim()
        .status_report(&encode_status_report(None, mr, "+4500000099", ts(), ts(), 0x20).unwrap());
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(h.message_status(&id).await, "submitted");
    // permanent failure
    let later = chrono::DateTime::parse_from_rfc3339("2025-03-04T05:07:00+01:00").unwrap();
    h.sim()
        .status_report(&encode_status_report(None, mr, "004500000099", ts(), later, 0x46).unwrap());
    h.wait_status(&id, "delivery_failed").await;
    let m = h.gw.db.get_message(&id).await.unwrap().unwrap();
    assert!(m.error.unwrap().contains("validity period expired"));
    assert_eq!(h.events("sms.failed").await.len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn rejected_submissions_retry_then_dead_letter() {
    let h = start(Setup {
        main: "[sms]\nmax_send_attempts = 2\nsend_retry_delay_seconds = 1\n".into(),
        ..Default::default()
    })
    .await;
    h.ready().await;
    h.sim().with(|s| s.send_mode = SendMode::CmsError(38));
    let id =
        h.gw.sms
            .submit(text_req("+4500000099", "will fail"))
            .await
            .unwrap()
            .message
            .id;
    h.wait_status(&id, "retrying").await;
    h.wait_status(&id, "failed").await;
    let m = h.gw.db.get_message(&id).await.unwrap().unwrap();
    assert_eq!(m.retry_count, 2);
    assert!(m.error.unwrap().contains("+CMS ERROR: 38"));
    assert_eq!(h.gw.db.list_dead_letters(10, 0).await.unwrap().1, 1);
    assert!(h.sim().sent().is_empty());

    // operator retry succeeds once the network accepts messages again
    h.sim().with(|s| s.send_mode = SendMode::Normal);
    assert!(h.gw.db.retry_message(&id, None).await.unwrap());
    h.gw.sms.wake.notify_waiters();
    h.wait_status(&id, "delivered").await;
    assert_eq!(h.sim().sent().len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn ambiguous_submission_is_never_retried_automatically() {
    let h = start(Setup::default()).await;
    h.ready().await;
    h.sim().with(|s| s.send_mode = SendMode::Silent);
    let id =
        h.gw.sms
            .submit(text_req("+4500000099", "did it go out?"))
            .await
            .unwrap()
            .message
            .id;
    h.wait_status(&id, "submission_unknown").await;
    h.sim().with(|s| s.send_mode = SendMode::Normal);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert_eq!(h.message_status(&id).await, "submission_unknown");
    assert!(
        h.sim().sent().is_empty(),
        "no automatic second transmission"
    );
    let part = h.gw.db.parts_for_message(&id).await.unwrap().remove(0);
    assert_eq!(part.status, "unknown");
    // "no prompt" is a definite failure and is retried
    h.sim().with(|s| s.send_mode = SendMode::NoPrompt);
    let id2 =
        h.gw.sms
            .submit(text_req("+4500000098", "no prompt"))
            .await
            .unwrap()
            .message
            .id;
    h.wait_status(&id2, "retrying").await;
    h.stop().await;
}

#[tokio::test]
async fn cancel_schedule_expiry_and_policy() {
    let h = start(Setup {
        main: "[sms]\nqueue_expiry_seconds = 1\nmax_message_parts = 2\nallow_flash_sms = false\n[security]\ndestination_blocklist = [\"+4590*\"]\nmax_outbound_queue_size = 3\n".into(),
        ..Default::default()
    })
    .await;
    // The modem is made unavailable so that nothing leaves the queue.
    h.sim().with(|s| s.available = false);
    h.sim().disconnect();
    let m = h.gw.modems[0].clone();
    wait("disconnected", 10, || {
        let m = m.clone();
        async move { m.status().state == ModemState::Disconnected }
    })
    .await;
    let err = |r: Result<_, SubmitError>| r.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(err(h.gw.sms.submit(text_req("not-a-number", "x")).await).contains("telephone number"));
    assert!(matches!(
        h.gw.sms.submit(text_req("+4590123456", "x")).await,
        Err(SubmitError::Forbidden(_))
    ));
    assert!(matches!(
        h.gw.sms
            .submit(text_req("+4500000099", &"long ".repeat(100)))
            .await,
        Err(SubmitError::Invalid(_))
    ));
    let mut flash = text_req("+4500000099", "x");
    flash.flash = true;
    assert!(matches!(
        h.gw.sms.submit(flash).await,
        Err(SubmitError::Forbidden(_))
    ));
    let mut m2 = text_req("+4500000099", "x");
    m2.modem = Some("nope".into());
    assert!(matches!(
        h.gw.sms.submit(m2).await,
        Err(SubmitError::Invalid(_))
    ));

    let mut sched = text_req("+4500000099", "later");
    sched.scheduled_at = Some(chrono::Utc::now() + chrono::Duration::hours(2));
    let sched = h.gw.sms.submit(sched).await.unwrap().message;
    assert_eq!(sched.status, "scheduled");
    assert!(h.gw.db.cancel_queued(&sched.id).await.unwrap());
    assert!(!h.gw.db.cancel_queued(&sched.id).await.unwrap());
    assert_eq!(h.message_status(&sched.id).await, "cancelled");

    let a =
        h.gw.sms
            .submit(text_req("+4500000099", "expires"))
            .await
            .unwrap()
            .message
            .id;
    for _ in 0..2 {
        h.gw.sms
            .submit(text_req("+4500000099", "fill"))
            .await
            .unwrap();
    }
    assert!(matches!(
        h.gw.sms.submit(text_req("+4500000099", "x")).await,
        Err(SubmitError::LimitReached(_))
    ));
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert_eq!(h.gw.db.expire_due().await.unwrap().len(), 3);
    assert_eq!(h.message_status(&a).await, "expired");
    h.stop().await;
}
